use crate::layout::{FontFamily, FontWeight, TextMeasurer, TextMetrics, TextStyle};
use fontique::{
    Attributes, Blob, Collection, CollectionOptions, FamilyId, FontStyle,
    FontWeight as MatchWeight, GenericFamily, QueryFamily, QueryStatus, SourceCache,
};
use harfrust::{Direction, FontRef as HarfFontRef, ShapeOptions, ShaperData, UnicodeBuffer};
use hashbrown::{Equivalent, HashMap};
use lru::LruCache;
use skrifa::{FontRef as SkrifaFontRef, MetadataProvider, instance::Size};
use std::{
    hash::{Hash, Hasher},
    mem::size_of,
    sync::Arc,
};
use swash::{
    FontRef as SwashFontRef,
    scale::{
        Render, ScaleContext, Source, StrikeWith,
        image::{Content, Image},
    },
};
use unicode_normalization::UnicodeNormalization;
use unicode_script::{Script as UnicodeScript, UnicodeScript as _};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlyphPositioning {
    Fractional,
    PixelSnapped,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterizationOptions {
    pub regular_gamma: f32,
    pub emphasized_gamma: f32,
    pub hinting: bool,
    pub positioning: GlyphPositioning,
}

#[derive(Clone)]
struct Face {
    blob: Blob<u8>,
    index: u32,
    family: (fontique::FamilyId, usize),
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ShaperDataKey {
    blob_id: u64,
    index: u32,
}

struct CacheEntry<K, V> {
    key: K,
    value: V,
    epoch: u64,
}

/// A small linear-scan LRU. Font sets are typically tiny, and a linear cache
/// avoids hashing on every shaped run.
struct SmallLru<K, V> {
    entries: Vec<CacheEntry<K, V>>,
    epoch: u64,
    capacity: usize,
}

impl<K: Eq, V> SmallLru<K, V> {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            entries: Vec::with_capacity(capacity),
            epoch: 0,
            capacity,
        }
    }

    fn get_or_insert_with(&mut self, key: K, make_value: impl FnOnce() -> V) -> &V {
        self.epoch = self.epoch.wrapping_add(1);
        let index = if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            index
        } else if self.entries.len() < self.capacity {
            let index = self.entries.len();
            self.entries.push(CacheEntry {
                key,
                value: make_value(),
                epoch: self.epoch,
            });
            index
        } else {
            let index = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.epoch)
                .map(|(index, _)| index)
                .expect("a nonzero-capacity full cache has an entry");
            self.entries[index] = CacheEntry {
                key,
                value: make_value(),
                epoch: self.epoch,
            };
            index
        };
        self.entries[index].epoch = self.epoch;
        &self.entries[index].value
    }
}

const GLYPH_IMAGE_CACHE_BUDGET: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum SubpixelBin {
    Zero,
    One,
    Two,
    Three,
}

impl SubpixelBin {
    const fn as_float(self) -> f32 {
        match self {
            Self::Zero => 0.0,
            Self::One => 0.25,
            Self::Two => 0.5,
            Self::Three => 0.75,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct GlyphImageKey {
    blob_id: u64,
    font_index: u32,
    glyph_id: u16,
    ppem: u32,
    x_bin: SubpixelBin,
    y_bin: SubpixelBin,
    foreground: [u8; 3],
    coverage: u32,
    hinting: bool,
}

impl GlyphImageKey {
    fn paint_resource_id(self) -> crate::pixels::PaintResourceId {
        let mut digest = blake3::Hasher::new();
        digest.update(b"flow-glyph-mask-v1\0");
        digest.update(&self.blob_id.to_le_bytes());
        digest.update(&self.font_index.to_le_bytes());
        digest.update(&self.glyph_id.to_le_bytes());
        digest.update(&self.ppem.to_le_bytes());
        digest.update(&[self.x_bin as u8, self.y_bin as u8]);
        digest.update(&self.foreground);
        digest.update(&self.coverage.to_le_bytes());
        digest.update(&[u8::from(self.hinting)]);
        let hash = digest.finalize();
        let mut id = [0; 16];
        id.copy_from_slice(&hash.as_bytes()[..16]);
        crate::pixels::PaintResourceId(id)
    }
}

struct GlyphImageCache {
    entries: LruCache<GlyphImageKey, Option<Image>>,
    bytes: usize,
    budget: usize,
}

impl GlyphImageCache {
    fn new(budget: usize) -> Self {
        Self {
            entries: LruCache::unbounded(),
            bytes: 0,
            budget,
        }
    }

    fn get_or_insert_with(
        &mut self,
        key: GlyphImageKey,
        render: impl FnOnce() -> Option<Image>,
    ) -> &Option<Image> {
        if self.entries.contains(&key) {
            return self
                .entries
                .get(&key)
                .expect("a contained glyph is present");
        }

        let image = render();
        self.bytes += glyph_image_bytes(&image);
        self.entries.put(key, image);
        while self.bytes > self.budget && self.entries.len() > 1 {
            let (_, evicted) = self
                .entries
                .pop_lru()
                .expect("a nonempty glyph cache has an LRU entry");
            self.bytes = self.bytes.saturating_sub(glyph_image_bytes(&evicted));
        }
        self.entries
            .get(&key)
            .expect("the most recent glyph remains cached")
    }
}

fn glyph_image_bytes(image: &Option<Image>) -> usize {
    size_of::<GlyphImageKey>()
        + size_of::<Option<Image>>()
        + image.as_ref().map_or(0, |image| image.data.len())
}

#[derive(Clone, Copy, Debug)]
pub struct ShapedCluster {
    pub start: usize,
    pub end: usize,
    pub x: f32,
    pub width: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct PositionedGlyph {
    pub id: u16,
    pub x_advance: f32,
    pub y_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    #[allow(dead_code)]
    pub cluster: usize,
}

#[derive(Clone)]
struct ShapedSegment {
    face: Face,
    glyphs: Vec<PositionedGlyph>,
}

#[derive(Clone)]
struct ShapedText {
    segments: Vec<ShapedSegment>,
    clusters: Vec<ShapedCluster>,
}

const SHAPED_TEXT_CACHE_BUDGET: usize = 32 * 1024 * 1024;

#[derive(Eq, Hash, PartialEq)]
struct ShapedTextKey {
    text: Box<str>,
    family: FontFamily,
    size: u32,
    weight: FontWeight,
    italic: bool,
    letter_spacing: u32,
    bidi: crate::model::flow::BidiState,
    shape_direction: crate::model::flow::BaseDirection,
    scale: u32,
}

struct ShapedTextLookup<'a> {
    text: &'a str,
    style: &'a TextStyle,
    scale: u32,
}

impl Hash for ShapedTextLookup<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text.hash(state);
        self.style.family.hash(state);
        self.style.size.to_bits().hash(state);
        self.style.weight.hash(state);
        self.style.italic.hash(state);
        self.style.letter_spacing.to_bits().hash(state);
        self.style.bidi.hash(state);
        self.style.shape_direction.hash(state);
        self.scale.hash(state);
    }
}

impl Equivalent<ShapedTextKey> for ShapedTextLookup<'_> {
    fn equivalent(&self, key: &ShapedTextKey) -> bool {
        self.text == key.text.as_ref()
            && self.style.family == key.family
            && self.style.size.to_bits() == key.size
            && self.style.weight == key.weight
            && self.style.italic == key.italic
            && self.style.letter_spacing.to_bits() == key.letter_spacing
            && self.style.bidi == key.bidi
            && self.style.shape_direction == key.shape_direction
            && self.scale == key.scale
    }
}

struct CachedShapedText {
    shaped: Arc<ShapedText>,
    bytes: usize,
    last_used: u64,
}

struct ShapedTextCache {
    entries: HashMap<ShapedTextKey, CachedShapedText>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl ShapedTextCache {
    fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            budget,
            clock: 0,
        }
    }

    fn get(&mut self, text: &str, style: &TextStyle, scale: f32) -> Option<Arc<ShapedText>> {
        self.clock = self.clock.wrapping_add(1);
        let cached = self.entries.get_mut(&ShapedTextLookup {
            text,
            style,
            scale: scale.to_bits(),
        })?;
        cached.last_used = self.clock;
        Some(Arc::clone(&cached.shaped))
    }

    fn insert(&mut self, text: &str, style: &TextStyle, scale: f32, shaped: Arc<ShapedText>) {
        self.clock = self.clock.wrapping_add(1);
        let bytes = shaped_text_bytes(text, &shaped);
        self.bytes += bytes;
        self.entries.insert(
            ShapedTextKey {
                text: text.into(),
                family: style.family.clone(),
                size: style.size.to_bits(),
                weight: style.weight,
                italic: style.italic,
                letter_spacing: style.letter_spacing.to_bits(),
                bidi: style.bidi,
                shape_direction: style.shape_direction,
                scale: scale.to_bits(),
            },
            CachedShapedText {
                shaped,
                bytes,
                last_used: self.clock,
            },
        );
        while self.bytes > self.budget && self.entries.len() > 1 {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| key)
                .expect("a nonempty shaped text cache has an oldest entry");
            let oldest = ShapedTextKey {
                text: oldest.text.clone(),
                family: oldest.family.clone(),
                size: oldest.size,
                weight: oldest.weight,
                italic: oldest.italic,
                letter_spacing: oldest.letter_spacing,
                bidi: oldest.bidi,
                shape_direction: oldest.shape_direction,
                scale: oldest.scale,
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(evicted.bytes);
            }
        }
    }
}

fn shaped_text_bytes(text: &str, shaped: &ShapedText) -> usize {
    size_of::<ShapedTextKey>()
        + size_of::<CachedShapedText>()
        + text.len()
        + shaped.clusters.len() * size_of::<ShapedCluster>()
        + shaped
            .segments
            .iter()
            .map(|segment| {
                size_of::<ShapedSegment>() + segment.glyphs.len() * size_of::<PositionedGlyph>()
            })
            .sum::<usize>()
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MetricKey {
    text: String,
    family: FontFamily,
    size: u32,
    weight: FontWeight,
    italic: bool,
    letter_spacing: u32,
    shape_direction: crate::model::flow::BaseDirection,
}

#[derive(Clone)]
pub struct FontResource {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    digest: [u8; 32],
}

impl FontResource {
    pub fn from_bytes(data: Arc<[u8]>) -> Self {
        let digest = *blake3::hash(data.as_ref()).as_bytes();
        Self {
            data: Arc::new(data),
            digest,
        }
    }

    /// Wraps arbitrary shared, immutable font backing without copying it.
    ///
    /// Hosts can use this for memory maps, static data, or platform resource
    /// handles whose [`AsRef`] implementation exposes the font bytes. The
    /// backing is retained for as long as Fontique may shape or rasterize from
    /// the resource.
    pub fn from_backing(data: impl AsRef<[u8]> + Send + Sync + 'static) -> Self {
        let digest = *blake3::hash(data.as_ref()).as_bytes();
        Self {
            data: Arc::new(data),
            digest,
        }
    }

    pub(crate) fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

pub struct FontSystem {
    collection: Collection,
    source_cache: SourceCache,
    scale_context: ScaleContext,
    metric_cache: HashMap<MetricKey, TextMetrics>,
    shaper_data_cache: SmallLru<ShaperDataKey, ShaperData>,
    shaped_text_cache: ShapedTextCache,
    glyph_image_cache: GlyphImageCache,
    body_fonts: Vec<String>,
    code_fonts: Vec<String>,
    registered_families: Vec<FamilyId>,
    author_fonts: bool,
    regular_coverage: [u8; 256],
    emphasized_coverage: [u8; 256],
    regular_coverage_key: u32,
    emphasized_coverage_key: u32,
    hinting: bool,
    positioning: GlyphPositioning,
}

impl FontSystem {
    pub fn new(
        body_fonts: &[String],
        code_fonts: &[String],
        regular_gamma: f32,
        emphasized_gamma: f32,
        hinting: bool,
        positioning: GlyphPositioning,
    ) -> Self {
        #[allow(unused_mut)]
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            // Font acquisition belongs to the host, including in tests.
            system_fonts: false,
        });
        // An explicit fixture set makes renderer tests independent of host fonts.
        #[cfg(any(test, feature = "test-fonts"))]
        for bytes in [
            include_bytes!("../tests/fixtures/fonts/charter.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/charter-bold.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/charter-italic.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/charter-bold-italic.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/fira-code.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-emoji.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans-arabic.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans-hebrew.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans-devanagari.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans-jp.ttf").as_slice(),
        ] {
            collection.register_fonts(Blob::new(Arc::new(bytes)), None);
        }
        // Map generic families to fixture fonts for deterministic test shaping.
        #[cfg(any(test, feature = "test-fonts"))]
        for (generic, name) in [
            (GenericFamily::Serif, "Charter"),
            (GenericFamily::SansSerif, "Noto Sans"),
            (GenericFamily::Monospace, "Fira Code"),
            (GenericFamily::Emoji, "Noto Emoji"),
        ] {
            if let Some(id) = collection.family_id(name) {
                collection.set_generic_families(generic, std::iter::once(id));
            }
        }
        Self {
            collection,
            source_cache: SourceCache::new(Default::default()),
            scale_context: ScaleContext::new(),
            metric_cache: HashMap::new(),
            // Match Parley's deliberately small per-font ShaperData cache.
            shaper_data_cache: SmallLru::new(16),
            shaped_text_cache: ShapedTextCache::new(SHAPED_TEXT_CACHE_BUDGET),
            glyph_image_cache: GlyphImageCache::new(GLYPH_IMAGE_CACHE_BUDGET),
            body_fonts: body_fonts.to_vec(),
            code_fonts: code_fonts.to_vec(),
            registered_families: Vec::new(),
            author_fonts: true,
            regular_coverage: coverage_lut(regular_gamma),
            emphasized_coverage: coverage_lut(emphasized_gamma),
            regular_coverage_key: regular_gamma.to_bits(),
            emphasized_coverage_key: emphasized_gamma.to_bits(),
            hinting,
            positioning,
        }
    }

    pub fn register_font(&mut self, resource: FontResource) -> bool {
        let registered = self
            .collection
            .register_fonts(Blob::new(Arc::clone(&resource.data)), None);
        if registered.is_empty() {
            return false;
        }
        for (family, _) in registered {
            if !self.registered_families.contains(&family) {
                self.registered_families.push(family);
            }
        }
        self.metric_cache.clear();
        self.shaper_data_cache = SmallLru::new(16);
        self.shaped_text_cache = ShapedTextCache::new(SHAPED_TEXT_CACHE_BUDGET);
        self.glyph_image_cache = GlyphImageCache::new(GLYPH_IMAGE_CACHE_BUDGET);
        true
    }

    pub fn set_author_fonts(&mut self, enabled: bool) {
        self.author_fonts = enabled;
    }

    pub fn set_rasterization(&mut self, options: RasterizationOptions) {
        self.regular_coverage = coverage_lut(options.regular_gamma);
        self.emphasized_coverage = coverage_lut(options.emphasized_gamma);
        self.regular_coverage_key = options.regular_gamma.to_bits();
        self.emphasized_coverage_key = options.emphasized_gamma.to_bits();
        self.hinting = options.hinting;
        self.positioning = options.positioning;
    }

    pub fn measure(&mut self, text: &str, style: &TextStyle) -> TextMetrics {
        let key = MetricKey {
            text: text.to_owned(),
            family: style.family.clone(),
            size: style.size.to_bits(),
            weight: style.weight,
            italic: style.italic,
            letter_spacing: style.letter_spacing.to_bits(),
            shape_direction: style.shape_direction,
        };
        if let Some(metrics) = self.metric_cache.get(&key) {
            return *metrics;
        }
        let segments = self.shape(text, style, 1.0);
        let width = segments
            .segments
            .iter()
            .flat_map(|segment| &segment.glyphs)
            .map(|glyph| glyph.x_advance)
            .sum();
        // A line's baseline belongs to the requested style, not whichever
        // fallback face happens to contain its text. Using fallback metrics
        // here makes emoji and non-Latin words move the baseline for every run
        // on the line. Keep a stable primary-font strut and let fallback glyphs
        // paint relative to that shared baseline.
        let (ascent, descent) = self
            .select_face("M", style, Some(fontique::Script::from_bytes(*b"Latn")))
            .and_then(|face| font_metrics(&face, style.size))
            .unwrap_or((style.size * 0.8, style.size * 0.2));
        let metrics = TextMetrics {
            width,
            ascent,
            descent,
        };
        self.metric_cache.insert(key, metrics);
        metrics
    }

    pub fn clusters(&mut self, text: &str, style: &TextStyle) -> Vec<ShapedCluster> {
        self.shape(text, style, 1.0).clusters.clone()
    }

    pub fn draw<T: crate::pixels::PaintTarget>(
        &mut self,
        pixels: &mut T,
        text: &str,
        style: &TextStyle,
        origin: (f32, f32),
        scale: f32,
        color: [u8; 3],
    ) {
        let shaped = self.shape(text, style, scale);
        self.draw_prepared(pixels, &shaped, style, origin, scale, color);
    }

    fn draw_prepared<T: crate::pixels::PaintTarget>(
        &mut self,
        pixels: &mut T,
        shaped: &ShapedText,
        style: &TextStyle,
        origin: (f32, f32),
        scale: f32,
        color: [u8; 3],
    ) {
        let mut pen_x = origin.0 * scale;
        let mut pen_y = origin.1 * scale;
        for segment in &shaped.segments {
            let Some(font) =
                SwashFontRef::from_index(segment.face.blob.as_ref(), segment.face.index as usize)
            else {
                continue;
            };
            let ppem = style.size * scale;
            let hinting = self.hinting;
            let positioning = self.positioning;
            let blob_id = segment.face.blob.id();
            let font_index = segment.face.index;
            let mut scaler = self
                .scale_context
                .builder(font)
                .size(ppem)
                .hint(hinting)
                .build();
            let emphasized = style.weight == FontWeight::Bold || style.italic;
            let (coverage, coverage_key) = if emphasized {
                (&self.emphasized_coverage, self.emphasized_coverage_key)
            } else {
                (&self.regular_coverage, self.regular_coverage_key)
            };
            let glyph_image_cache = &mut self.glyph_image_cache;
            for glyph in &segment.glyphs {
                let glyph_x = pen_x + glyph.x_offset;
                let glyph_y = pen_y - glyph.y_offset;
                let (base_x, x_bin) = raster_position(glyph_x, positioning);
                let (base_y, y_bin) = raster_position(glyph_y, positioning);
                let key = GlyphImageKey {
                    blob_id,
                    font_index,
                    glyph_id: glyph.id,
                    ppem: ppem.to_bits(),
                    x_bin,
                    y_bin,
                    foreground: color,
                    coverage: coverage_key,
                    hinting,
                };
                let resource_id = key.paint_resource_id();
                let image = glyph_image_cache.get_or_insert_with(key, || {
                    let mut renderer = Render::new(&[
                        Source::ColorOutline(0),
                        Source::ColorBitmap(StrikeWith::BestFit),
                        Source::Outline,
                    ]);
                    renderer
                        .offset(swash::zeno::Vector::new(x_bin.as_float(), y_bin.as_float()))
                        .default_color([color[0], color[1], color[2], 255]);
                    renderer.render(&mut scaler, glyph.id).map(|mut image| {
                        if let Content::Mask = image.content {
                            apply_coverage(&mut image.data, coverage);
                        }
                        image
                    })
                });
                if let Some(image) = image {
                    let x = base_x + image.placement.left;
                    let y = base_y - image.placement.top;
                    match image.content {
                        Content::Mask => pixels.blend_mask_resource(
                            resource_id,
                            (x, y),
                            image.placement.width,
                            image.placement.height,
                            &image.data,
                            color,
                        ),
                        Content::Color => pixels.blend_pixels_resource(
                            resource_id,
                            (x, y),
                            image.placement.width,
                            image.placement.height,
                            image.placement.width as usize * 4,
                            crate::pixels::PixelFormat::Rgba8888,
                            &image.data,
                        ),
                        Content::SubpixelMask => {}
                    }
                }
                pen_x += glyph.x_advance;
                pen_y += glyph.y_advance;
            }
        }
    }

    fn shape(&mut self, text: &str, style: &TextStyle, scale: f32) -> Arc<ShapedText> {
        if let Some(shaped) = self.shaped_text_cache.get(text, style, scale) {
            return shaped;
        }
        let shaped = Arc::new(self.shape_uncached(text, style, scale));
        self.shaped_text_cache
            .insert(text, style, scale, Arc::clone(&shaped));
        shaped
    }

    fn shape_uncached(&mut self, text: &str, style: &TextStyle, scale: f32) -> ShapedText {
        let (configured, primary, author_family) = match &style.family {
            FontFamily::Proportional => (&self.body_fonts, GenericFamily::Serif, None),
            FontFamily::SansSerif => (&self.body_fonts, GenericFamily::SansSerif, None),
            FontFamily::Monospace => (&self.code_fonts, GenericFamily::Monospace, None),
            FontFamily::Named(name) => (
                &self.body_fonts,
                GenericFamily::Serif,
                self.author_fonts.then_some(name.as_str()),
            ),
        };
        let attributes = match_attributes(style);
        let runs = collect_face_runs(
            &mut self.collection,
            &mut self.source_cache,
            configured,
            &self.registered_families,
            primary,
            author_family,
            attributes,
            text,
            true,
        )
        .unwrap_or_default();

        let mut segments: Vec<_> = runs
            .into_iter()
            .filter_map(|(face, text, source_offset)| {
                let font = HarfFontRef::from_index(face.blob.as_ref(), face.index).ok()?;
                let shaper_data = self.shaper_data_cache.get_or_insert_with(
                    ShaperDataKey {
                        blob_id: face.blob.id(),
                        index: face.index,
                    },
                    || ShaperData::new(&font),
                );
                let shaper = shaper_data.shaper(&font).build();
                let mut buffer = UnicodeBuffer::new();
                buffer.push_str(&text);
                buffer.guess_segment_properties();
                match style.shape_direction {
                    crate::model::flow::BaseDirection::LeftToRight => {
                        buffer.set_direction(Direction::LeftToRight)
                    }
                    crate::model::flow::BaseDirection::RightToLeft => {
                        buffer.set_direction(Direction::RightToLeft)
                    }
                    crate::model::flow::BaseDirection::Auto => {}
                }
                let precision = 64.0;
                let shaped = shaper.shape(
                    buffer,
                    ShapeOptions::new()
                        .scale(Some((style.size * scale * precision).round() as i32)),
                );
                let glyphs = shaped
                    .glyph_infos()
                    .iter()
                    .zip(shaped.glyph_positions())
                    .map(|(info, position)| PositionedGlyph {
                        id: info.glyph_id as u16,
                        x_advance: position.x_advance as f32 / precision
                            + style.letter_spacing * scale,
                        y_advance: position.y_advance as f32 / precision,
                        x_offset: position.x_offset as f32 / precision,
                        y_offset: position.y_offset as f32 / precision,
                        cluster: source_offset + info.cluster as usize,
                    })
                    .collect();
                Some(ShapedSegment { face, glyphs })
            })
            .collect();
        if matches!(
            style.shape_direction,
            crate::model::flow::BaseDirection::RightToLeft
        ) {
            segments.reverse();
        }
        let clusters = shaped_clusters(text, &segments, scale);
        ShapedText { segments, clusters }
    }

    fn select_face(
        &mut self,
        grapheme: &str,
        style: &TextStyle,
        script: Option<fontique::Script>,
    ) -> Option<Face> {
        let (configured, primary, author_family) = match &style.family {
            FontFamily::Proportional => (&self.body_fonts, GenericFamily::Serif, None),
            FontFamily::SansSerif => (&self.body_fonts, GenericFamily::SansSerif, None),
            FontFamily::Monospace => (&self.code_fonts, GenericFamily::Monospace, None),
            FontFamily::Named(name) => (
                &self.body_fonts,
                GenericFamily::Serif,
                self.author_fonts.then_some(name.as_str()),
            ),
        };
        let attributes = match_attributes(style);
        let required = required_characters(grapheme);
        query_face(
            &mut self.collection,
            &mut self.source_cache,
            configured,
            &self.registered_families,
            primary,
            author_family,
            attributes,
            script,
            &required,
            true,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn query_face(
    collection: &mut Collection,
    source_cache: &mut SourceCache,
    configured: &[String],
    registered: &[FamilyId],
    primary: GenericFamily,
    author_family: Option<&str>,
    attributes: Attributes,
    script: Option<fontique::Script>,
    required: &[char],
    allow_unsupported: bool,
) -> Option<Face> {
    let mut query = collection.query(source_cache);
    query.set_families(query_families(
        configured,
        registered,
        primary,
        author_family,
    ));
    query.set_attributes(attributes);
    if let Some(script) = script {
        query.set_fallbacks(script);
    }
    match_query_face(&mut query, required, allow_unsupported)
}

#[allow(clippy::too_many_arguments)]
fn collect_face_runs(
    collection: &mut Collection,
    source_cache: &mut SourceCache,
    configured: &[String],
    registered: &[FamilyId],
    primary: GenericFamily,
    author_family: Option<&str>,
    attributes: Attributes,
    text: &str,
    allow_unsupported: bool,
) -> Result<Vec<(Face, String, usize)>, ()> {
    let mut query = collection.query(source_cache);
    query.set_families(query_families(
        configured,
        registered,
        primary,
        author_family,
    ));
    query.set_attributes(attributes);

    let mut runs: Vec<(Face, String, usize)> = Vec::new();
    let mut script_hint = None;
    let mut query_script = None;
    for (offset, grapheme) in text.grapheme_indices(true) {
        script_hint = grapheme_script(grapheme).or(script_hint);
        if script_hint != query_script {
            if let Some(script) = script_hint {
                query.set_fallbacks(script);
            }
            query_script = script_hint;
        }
        let required = required_characters(grapheme);
        let Some(face) = match_query_face(&mut query, &required, allow_unsupported) else {
            if allow_unsupported {
                continue;
            }
            return Err(());
        };
        if let Some((previous, previous_text, _)) = runs.last_mut()
            && previous.family == face.family
        {
            previous_text.push_str(grapheme);
        } else {
            runs.push((face, grapheme.to_owned(), offset));
        }
    }
    Ok(runs)
}

fn query_families<'a>(
    configured: &'a [String],
    registered: &[FamilyId],
    primary: GenericFamily,
    author_family: Option<&'a str>,
) -> Vec<QueryFamily<'a>> {
    let mut families = Vec::new();
    families.extend(author_family.map(QueryFamily::from));
    families.extend(
        configured
            .iter()
            .map(|family| QueryFamily::from(family.as_str())),
    );
    families.extend(registered.iter().copied().map(QueryFamily::Id));
    families.push(QueryFamily::Generic(primary));
    families.push(QueryFamily::Generic(GenericFamily::Emoji));
    families.push(QueryFamily::Generic(GenericFamily::Math));
    families.push(QueryFamily::Generic(GenericFamily::SystemUi));
    families
}

fn required_characters(grapheme: &str) -> Vec<char> {
    // HarfRust canonically composes where the selected face has a composed
    // glyph. Match against that same NFC repertoire so a decomposed sequence
    // such as `o` + COMBINING DIAERESIS stays in the body face instead of
    // falling back merely because the face does not map the combining scalar.
    grapheme
        .nfc()
        .filter(|character| !is_default_ignorable(*character))
        .collect()
}

fn match_query_face(
    query: &mut fontique::Query<'_>,
    required: &[char],
    allow_unsupported: bool,
) -> Option<Face> {
    let mut selected = None;
    let mut fallback = None;
    query.matches_with(|candidate| {
        fallback.get_or_insert_with(|| Face {
            blob: candidate.blob.clone(),
            index: candidate.index,
            family: candidate.family,
        });
        let supports = candidate.charmap().is_some_and(|charmap| {
            required
                .iter()
                .all(|character| charmap.map(*character).is_some())
        });
        if supports {
            selected = Some(Face {
                blob: candidate.blob.clone(),
                index: candidate.index,
                family: candidate.family,
            });
            QueryStatus::Stop
        } else {
            QueryStatus::Continue
        }
    });
    selected.or_else(|| allow_unsupported.then_some(fallback).flatten())
}

fn match_attributes(style: &TextStyle) -> Attributes {
    Attributes::new(
        fontique::FontWidth::NORMAL,
        if style.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        if style.weight == FontWeight::Bold {
            MatchWeight::BOLD
        } else {
            MatchWeight::NORMAL
        },
    )
}

impl Default for FontSystem {
    fn default() -> Self {
        let settings = crate::settings::Settings::default();
        Self::new(
            &settings.body_fonts,
            &settings.code_fonts,
            settings.regular_gamma,
            settings.emphasized_gamma,
            settings.font_hinting,
            settings.glyph_positioning(),
        )
    }
}

fn shaped_clusters(text: &str, segments: &[ShapedSegment], scale: f32) -> Vec<ShapedCluster> {
    let mut bounds = HashMap::<usize, (f32, f32)>::new();
    let mut pen_x = 0.0_f32;
    for glyph in segments.iter().flat_map(|segment| &segment.glyphs) {
        let start = pen_x.min(pen_x + glyph.x_advance) / scale;
        let end = pen_x.max(pen_x + glyph.x_advance) / scale;
        bounds
            .entry(glyph.cluster)
            .and_modify(|current| {
                current.0 = current.0.min(start);
                current.1 = current.1.max(end);
            })
            .or_insert((start, end));
        pen_x += glyph.x_advance;
    }
    let mut starts: Vec<_> = bounds.keys().copied().collect();
    starts.sort_unstable();
    let ends: HashMap<_, _> = starts
        .iter()
        .enumerate()
        .map(|(index, start)| (*start, starts.get(index + 1).copied().unwrap_or(text.len())))
        .collect();
    let mut clusters: Vec<_> = bounds
        .into_iter()
        .map(|(start, (x0, x1))| ShapedCluster {
            start,
            end: ends[&start],
            x: x0,
            width: x1 - x0,
        })
        .collect();
    clusters.sort_by(|left, right| left.x.total_cmp(&right.x));
    clusters
}

fn raster_position(position: f32, positioning: GlyphPositioning) -> (i32, SubpixelBin) {
    let base = position.floor();
    if positioning == GlyphPositioning::PixelSnapped {
        return (base as i32, SubpixelBin::Zero);
    }

    let fraction = position - base;
    if fraction < 0.125 {
        (base as i32, SubpixelBin::Zero)
    } else if fraction < 0.375 {
        (base as i32, SubpixelBin::One)
    } else if fraction < 0.625 {
        (base as i32, SubpixelBin::Two)
    } else if fraction < 0.875 {
        (base as i32, SubpixelBin::Three)
    } else {
        ((base + 1.0) as i32, SubpixelBin::Zero)
    }
}

fn coverage_lut(gamma: f32) -> [u8; 256] {
    std::array::from_fn(|value| ((value as f32 / 255.0).powf(gamma) * 255.0).round() as u8)
}

fn apply_coverage(mask: &mut [u8], coverage: &[u8; 256]) {
    for alpha in mask {
        *alpha = coverage[*alpha as usize];
    }
}

impl TextMeasurer for std::cell::RefCell<FontSystem> {
    fn measure(&self, text: &str, style: &TextStyle) -> TextMetrics {
        self.borrow_mut().measure(text, style)
    }
}

fn font_metrics(face: &Face, size: f32) -> Option<(f32, f32)> {
    let font = SkrifaFontRef::from_index(face.blob.as_ref(), face.index).ok()?;
    let metrics = font.metrics(Size::new(size), skrifa::instance::LocationRef::default());
    Some((metrics.ascent.max(0.0), (-metrics.descent).max(0.0)))
}

fn grapheme_script(grapheme: &str) -> Option<fontique::Script> {
    let script = grapheme
        .chars()
        .map(|character| character.script())
        .find(|script| {
            !matches!(
                script,
                UnicodeScript::Common | UnicodeScript::Inherited | UnicodeScript::Unknown
            )
        })?;
    Some(fontique::Script::from_bytes(
        script.as_iso15924_tag().to_be_bytes(),
    ))
}

fn is_default_ignorable(character: char) -> bool {
    matches!(character, '\u{200c}' | '\u{200d}' | '\u{fe0e}' | '\u{fe0f}')
        || ('\u{e0100}'..='\u{e01ef}').contains(&character)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_style() -> TextStyle {
        TextStyle {
            family: FontFamily::Proportional,
            size: 18.0,
            weight: FontWeight::Normal,
            italic: false,
            strikethrough: false,
            link: false,
            initial_letter: None,
            baseline_shift: 0.0,
            letter_spacing: 0.0,
            bidi: crate::model::flow::BidiState::default(),
            bidi_boundary: None,
            shape_direction: crate::model::flow::BaseDirection::Auto,
        }
    }

    #[test]
    fn coverage_gamma_is_configurable() {
        assert_eq!(coverage_lut(1.0)[128], 128);
        assert!(coverage_lut(0.65)[128] > 128);
        assert!(coverage_lut(1.5)[128] < 128);
    }

    #[test]
    fn face_coverage_uses_canonical_composition() {
        assert_eq!(required_characters("o\u{308}"), vec!['ö']);
        assert_eq!(required_characters("❤️"), vec!['❤']);
    }

    #[test]
    fn coverage_is_applied_to_cached_mask_data() {
        let mut mask = [0, 64, 128, 255];
        let coverage = coverage_lut(0.65);
        apply_coverage(&mut mask, &coverage);
        assert_eq!(mask, [0, coverage[64], coverage[128], 255]);
    }

    #[test]
    fn fractional_positions_use_quarter_pixel_bins() {
        assert_eq!(
            raster_position(3.124, GlyphPositioning::Fractional),
            (3, SubpixelBin::Zero)
        );
        assert_eq!(
            raster_position(3.125, GlyphPositioning::Fractional),
            (3, SubpixelBin::One)
        );
        assert_eq!(
            raster_position(3.5, GlyphPositioning::Fractional),
            (3, SubpixelBin::Two)
        );
        assert_eq!(
            raster_position(3.75, GlyphPositioning::Fractional),
            (3, SubpixelBin::Three)
        );
        assert_eq!(
            raster_position(3.875, GlyphPositioning::Fractional),
            (4, SubpixelBin::Zero)
        );
        assert_eq!(
            raster_position(-0.25, GlyphPositioning::Fractional),
            (-1, SubpixelBin::Three)
        );
        assert_eq!(
            raster_position(-0.1, GlyphPositioning::Fractional),
            (0, SubpixelBin::Zero)
        );
        assert_eq!(
            raster_position(3.9, GlyphPositioning::PixelSnapped),
            (3, SubpixelBin::Zero)
        );
    }

    #[test]
    fn small_lru_reuses_entries_and_evicts_the_oldest() {
        let mut cache = SmallLru::new(2);
        assert_eq!(*cache.get_or_insert_with(1, || 10), 10);
        assert_eq!(*cache.get_or_insert_with(2, || 20), 20);
        assert_eq!(
            *cache.get_or_insert_with(1, || panic!("entry should be reused")),
            10
        );
        assert_eq!(*cache.get_or_insert_with(3, || 30), 30);

        assert!(cache.entries.iter().any(|entry| entry.key == 1));
        assert!(cache.entries.iter().any(|entry| entry.key == 3));
        assert!(!cache.entries.iter().any(|entry| entry.key == 2));
    }

    #[test]
    fn shaper_data_is_reused_for_the_same_face() {
        let mut fonts = FontSystem::default();
        fonts.measure("flow", &body_style());
        let cached = fonts.shaper_data_cache.entries.len();
        assert!(cached > 0);

        fonts.measure("reader", &body_style());
        assert_eq!(fonts.shaper_data_cache.entries.len(), cached);
    }

    #[test]
    fn glyph_image_cache_evicts_the_least_recently_used_image() {
        fn key(glyph_id: u16) -> GlyphImageKey {
            GlyphImageKey {
                blob_id: 1,
                font_index: 0,
                glyph_id,
                ppem: 16.0_f32.to_bits(),
                x_bin: SubpixelBin::Zero,
                y_bin: SubpixelBin::Zero,
                foreground: [0, 0, 0],
                coverage: 1.0_f32.to_bits(),
                hinting: true,
            }
        }

        fn image() -> Option<Image> {
            let mut image = Image::new();
            image.data = vec![0; 16];
            Some(image)
        }

        let entry_bytes = glyph_image_bytes(&image());
        let mut cache = GlyphImageCache::new(entry_bytes * 2);
        cache.get_or_insert_with(key(1), image);
        cache.get_or_insert_with(key(2), image);
        cache.get_or_insert_with(key(1), || {
            panic!("a cached glyph must not be rendered again")
        });
        cache.get_or_insert_with(key(3), image);

        assert_eq!(cache.entries.len(), 2);
        assert!(cache.entries.contains(&key(1)));
        assert!(!cache.entries.contains(&key(2)));
        assert!(cache.entries.contains(&key(3)));
        assert!(cache.bytes <= cache.budget);
    }

    #[test]
    fn shaped_text_and_clusters_are_reused() {
        let mut fonts = FontSystem::default();
        let style = body_style();
        let first = fonts.shape("repeated text", &style, 1.0);
        let second = fonts.shape("repeated text", &style, 1.0);
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!first.clusters.is_empty());

        let scaled = fonts.shape("repeated text", &style, 2.0);
        assert!(!Arc::ptr_eq(&first, &scaled));
    }

    #[test]
    fn rendered_glyph_images_are_reused() {
        let mut fonts = FontSystem::default();
        let mut pixels = crate::pixels::PixelBuffer::new(300, 100, [255, 255, 255]).unwrap();
        fonts.draw(
            &mut pixels.surface_mut(),
            "repeated text",
            &body_style(),
            (10.0, 50.0),
            1.0,
            [0, 0, 0],
        );
        // Mark stored masks so a fresh render cannot masquerade as reuse.
        for (_, image) in fonts.glyph_image_cache.entries.iter_mut() {
            if let Some(byte) = image.as_mut().and_then(|image| image.data.first_mut()) {
                *byte ^= 1;
            }
        }
        let cached: Vec<_> = fonts
            .glyph_image_cache
            .entries
            .iter()
            .map(|(key, image)| (*key, image.as_ref().map(|image| image.data.clone())))
            .collect();
        assert!(!cached.is_empty());

        fonts.draw(
            &mut pixels.surface_mut(),
            "repeated text",
            &body_style(),
            (10.0, 50.0),
            1.0,
            [0, 0, 0],
        );
        assert_eq!(fonts.glyph_image_cache.entries.len(), cached.len());
        for (key, expected) in cached {
            let retained = fonts.glyph_image_cache.entries.peek(&key).unwrap();
            assert_eq!(
                retained.as_ref().map(|image| image.data.as_slice()),
                expected.as_deref()
            );
        }
    }

    #[test]
    fn cached_glyph_masks_follow_runtime_gamma_changes() {
        let settings = crate::settings::Settings::default();
        let original = settings.rasterization();
        let mut fonts = FontSystem::new(
            &settings.body_fonts,
            &settings.code_fonts,
            original.regular_gamma,
            original.emphasized_gamma,
            original.hinting,
            original.positioning,
        );
        let mut pixels = crate::pixels::PixelBuffer::new(300, 100, [255, 255, 255]).unwrap();
        fonts.draw(
            &mut pixels.surface_mut(),
            "coverage",
            &body_style(),
            (10.0, 50.0),
            1.0,
            [0, 0, 0],
        );
        let original_pixels = pixels.pixels().to_vec();
        let original_keys: std::collections::HashSet<_> = fonts
            .glyph_image_cache
            .entries
            .iter()
            .map(|(key, _)| *key)
            .collect();

        let changed = RasterizationOptions {
            regular_gamma: 1.5,
            ..original
        };
        fonts.set_rasterization(changed);
        pixels.resize_and_clear(300, 100, [255, 255, 255]).unwrap();
        fonts.draw(
            &mut pixels.surface_mut(),
            "coverage",
            &body_style(),
            (10.0, 50.0),
            1.0,
            [0, 0, 0],
        );
        let changed_keys: std::collections::HashSet<_> = fonts
            .glyph_image_cache
            .entries
            .iter()
            .map(|(key, _)| *key)
            .collect();
        assert!(original_keys.is_subset(&changed_keys));
        assert!(changed_keys.len() > original_keys.len());
        assert_ne!(pixels.pixels(), original_pixels);

        fonts.set_rasterization(original);
        pixels.resize_and_clear(300, 100, [255, 255, 255]).unwrap();
        fonts.draw(
            &mut pixels.surface_mut(),
            "coverage",
            &body_style(),
            (10.0, 50.0),
            1.0,
            [0, 0, 0],
        );
        let restored_keys: std::collections::HashSet<_> = fonts
            .glyph_image_cache
            .entries
            .iter()
            .map(|(key, _)| *key)
            .collect();
        assert_eq!(restored_keys, changed_keys);
        assert_eq!(pixels.pixels(), original_pixels);
    }

    #[test]
    fn shaped_clusters_preserve_combining_sequences() {
        let mut fonts = FontSystem::default();
        let clusters = fonts.clusters("e\u{301}x", &body_style());

        assert!(
            clusters
                .iter()
                .any(|cluster| cluster.start == 0 && cluster.end >= 3)
        );
        assert!(clusters.iter().all(|cluster| cluster.width >= 0.0));
    }

    #[test]
    fn explicit_bidi_direction_shapes_and_caches_visual_cluster_order() {
        let mut fonts = FontSystem::default();
        let ltr = body_style();
        let mut rtl = ltr.clone();
        rtl.bidi.direction = crate::model::flow::BaseDirection::RightToLeft;
        rtl.shape_direction = crate::model::flow::BaseDirection::RightToLeft;

        let ltr_clusters = fonts.clusters("abc", &ltr);
        let rtl_clusters = fonts.clusters("abc", &rtl);
        let cached_ltr_clusters = fonts.clusters("abc", &ltr);

        assert!(ltr_clusters.first().unwrap().start < ltr_clusters.last().unwrap().start);
        assert!(rtl_clusters.first().unwrap().start > rtl_clusters.last().unwrap().start);
        assert_eq!(
            cached_ltr_clusters.first().unwrap().start,
            ltr_clusters.first().unwrap().start
        );
    }

    #[test]
    fn bundled_web_fonts_shape_combining_marks_scripts_and_emoji() {
        let settings = crate::settings::Settings::default();
        let mut fonts = FontSystem::new(
            &["Charter".to_owned()],
            &["Fira Code".to_owned()],
            settings.regular_gamma,
            settings.emphasized_gamma,
            settings.font_hinting,
            settings.glyph_positioning(),
        );
        for bytes in [
            include_bytes!("../tests/fixtures/fonts/charter.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-emoji.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans-arabic.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans-hebrew.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans-devanagari.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/noto-sans-jp.ttf").as_slice(),
            include_bytes!("../tests/fixtures/fonts/fira-code.ttf").as_slice(),
        ] {
            assert!(fonts.register_font(FontResource::from_bytes(Arc::from(bytes))));
        }

        for text in [
            "A\u{30a}ngstro\u{308}m cafe\u{301}",
            "العَرَبِيَّةُ",
            "עברית",
            "देवनागरी",
            "Καλημέρα Здравствуй ∀x ∈ ℝ, ∫₀^∞",
            "你好世界。日本語の文章。",
            "😀 🚀 ❤️ 👍🏽 👩‍💻 👨‍👩‍👧‍👦 🏳️‍🌈",
        ] {
            let shaped = fonts.shape(text, &body_style(), 2.0);
            assert!(!shaped.segments.is_empty(), "no segments for {text}");
            assert!(
                shaped
                    .segments
                    .iter()
                    .flat_map(|segment| &segment.glyphs)
                    .all(|glyph| glyph.id != 0),
                "missing glyph while shaping {text}",
            );
        }
    }

    #[test]
    fn author_family_precedes_reader_and_registered_fallbacks_when_enabled() {
        let configured = vec!["Reader Serif".to_owned()];
        let families = query_families(
            &configured,
            &[],
            GenericFamily::Serif,
            Some("Publication Serif"),
        );
        assert_eq!(families[0], QueryFamily::Named("Publication Serif"));
        assert_eq!(families[1], QueryFamily::Named("Reader Serif"));
        assert_eq!(families[2], QueryFamily::Generic(GenericFamily::Serif));
    }

    #[test]
    fn fallback_text_does_not_move_the_style_baseline() {
        let settings = crate::settings::Settings::default();
        let mut fonts = FontSystem::new(
            &settings.body_fonts,
            &settings.code_fonts,
            settings.regular_gamma,
            settings.emphasized_gamma,
            settings.font_hinting,
            settings.glyph_positioning(),
        );
        let latin = fonts.measure("flow", &body_style());
        let fallback = fonts.measure("😀 नमस्ते العربية", &body_style());

        assert_eq!(latin.ascent, fallback.ascent);
        assert_eq!(latin.descent, fallback.descent);
    }
}
