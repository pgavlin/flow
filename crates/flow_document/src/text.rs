use crate::layout::{FontFamily, FontWeight, TextMeasurer, TextMetrics, TextStyle};
use fontique::{
    Attributes, Blob, Collection, CollectionOptions, FamilyId, FontStyle,
    FontWeight as MatchWeight, GenericFamily, QueryFamily, QueryStatus, SourceCache,
};
use harfrust::{
    Direction, FontRef as HarfFontRef, ShapeOptions, ShapePlan, ShapePlanKey, ShaperData,
    UnicodeBuffer,
};
use lru::LruCache;
use std::collections::HashMap;
mod cache;
use cache::{CacheBudget, TextCache};
use skrifa::{FontRef as SkrifaFontRef, MetadataProvider, instance::Size};
use std::{mem::size_of, sync::Arc};
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
            entries: Vec::new(),
            epoch: 0,
            capacity,
        }
    }

    fn get_or_insert_with(&mut self, key: K, make_value: impl FnOnce() -> V) -> &mut V {
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
        &mut self.entries[index].value
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

    fn evict(&mut self, budget: &mut CacheBudget) -> bool {
        if let Some((_, image)) = self.entries.pop_lru() {
            let bytes = glyph_image_bytes(&image);
            self.bytes -= bytes;
            budget.release(bytes);
            budget.evictions += 1;
            true
        } else {
            false
        }
    }

    fn get_or_insert_with<'a>(
        &'a mut self,
        key: GlyphImageKey,
        budget: &mut CacheBudget,
        render: impl FnOnce() -> Option<Image>,
    ) -> GlyphImage<'a> {
        if self.entries.contains(&key) {
            return GlyphImage::Cached(self.entries.get(&key).expect("cached glyph"));
        }
        let image = render();
        let bytes = glyph_image_bytes(&image);
        if bytes > self.budget || bytes > budget.limit {
            return GlyphImage::Uncached(image);
        }
        while self.bytes + bytes > self.budget || bytes > budget.limit.saturating_sub(budget.used) {
            if !self.evict(budget) {
                return GlyphImage::Uncached(image);
            }
        }
        assert!(budget.reserve(bytes));
        self.bytes += bytes;
        self.entries.put(key, image);
        GlyphImage::Cached(self.entries.get(&key).expect("inserted glyph"))
    }
}

enum GlyphImage<'a> {
    Cached(&'a Option<Image>),
    Uncached(Option<Image>),
}
impl std::ops::Deref for GlyphImage<'_> {
    type Target = Option<Image>;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Cached(image) => image,
            Self::Uncached(image) => image,
        }
    }
}

fn glyph_image_bytes(image: &Option<Image>) -> usize {
    size_of::<GlyphImageKey>()
        + size_of::<Option<Image>>()
        + image.as_ref().map_or(0, |image| image.data.capacity())
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

/// Diagnostic counters; never part of persistent artifact identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontSystemStats {
    pub registrations: u64,
    pub metric_hits: u64,
    pub metric_misses: u64,
    pub shaped_hits: u64,
    pub shaped_misses: u64,
    pub shaper_data_builds: u64,
    pub plan_hits: u64,
    pub plan_builds: u64,
    pub evictions: u64,
}
impl std::ops::AddAssign for FontSystemStats {
    fn add_assign(&mut self, other: Self) {
        self.registrations += other.registrations;
        self.metric_hits += other.metric_hits;
        self.metric_misses += other.metric_misses;
        self.shaped_hits += other.shaped_hits;
        self.shaped_misses += other.shaped_misses;
        self.shaper_data_builds += other.shaper_data_builds;
        self.plan_hits += other.plan_hits;
        self.plan_builds += other.plan_builds;
        self.evictions += other.evictions;
    }
}
struct FaceShaper {
    data: ShaperData,
    plans: Vec<ShapePlan>,
}
fn shaped_text_bytes(shaped: &ShapedText) -> usize {
    size_of::<ShapedText>()
        + shaped.clusters.capacity() * size_of::<ShapedCluster>()
        + shaped.segments.capacity() * size_of::<ShapedSegment>()
        + shaped
            .segments
            .iter()
            .map(|segment| segment.glyphs.capacity() * size_of::<PositionedGlyph>())
            .sum::<usize>()
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
    metric_cache: TextCache<TextMetrics>,
    cache_budget: CacheBudget,
    stats: FontSystemStats,
    plan_capacity: usize,
    shaper_data_cache: SmallLru<ShaperDataKey, FaceShaper>,
    shaped_text_cache: TextCache<Arc<ShapedText>>,
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
            crate::test_fonts::fixture("charter.ttf"),
            crate::test_fonts::fixture("charter-bold.ttf"),
            crate::test_fonts::fixture("charter-italic.ttf"),
            crate::test_fonts::fixture("charter-bold-italic.ttf"),
            crate::test_fonts::fixture("fira-code.ttf"),
            crate::test_fonts::fixture("noto-emoji.ttf"),
            crate::test_fonts::fixture("noto-sans.ttf"),
            crate::test_fonts::fixture("noto-sans-arabic.ttf"),
            crate::test_fonts::fixture("noto-sans-hebrew.ttf"),
            crate::test_fonts::fixture("noto-sans-devanagari.ttf"),
            crate::test_fonts::fixture("noto-sans-jp.ttf"),
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
            metric_cache: TextCache::default(),
            cache_budget: CacheBudget {
                limit: 32 * 1024 * 1024,
                ..CacheBudget::default()
            },
            stats: FontSystemStats::default(),
            plan_capacity: 8,
            // Match Parley's deliberately small per-font ShaperData cache.
            shaper_data_cache: SmallLru::new(16),
            shaped_text_cache: TextCache::default(),
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

    pub fn cache_bytes(&self) -> usize {
        self.cache_budget.used
    }
    pub(crate) fn cache_limit(&self) -> usize {
        self.cache_budget.limit
    }
    pub fn stats(&self) -> FontSystemStats {
        let mut stats = self.stats;
        stats.evictions = self.cache_budget.evictions;
        stats
    }
    pub(crate) fn take_stats(&mut self) -> FontSystemStats {
        let stats = self.stats();
        self.stats = FontSystemStats::default();
        self.cache_budget.evictions = 0;
        stats
    }
    pub(crate) fn set_cache_budget(&mut self, limit: usize) {
        self.cache_budget.limit = limit;
        self.glyph_image_cache.budget = limit;
        while self.cache_budget.used > limit {
            if self.glyph_image_cache.evict(&mut self.cache_budget) {
                continue;
            }
            if self.metric_cache.evict(&mut self.cache_budget) {
                continue;
            }
            if !self.shaped_text_cache.evict(&mut self.cache_budget) {
                break;
            }
        }
    }
    pub(crate) fn shrink_cache_storage(&mut self) {
        self.metric_cache.shrink_storage();
        self.shaped_text_cache.shrink_storage();
        let glyphs = &mut self.glyph_image_cache.entries;
        glyphs.resize(std::num::NonZeroUsize::new(glyphs.len().max(1)).unwrap());
        glyphs.resize(std::num::NonZeroUsize::MAX);
    }

    pub fn clear_caches(&mut self) {
        self.shaper_data_cache = SmallLru::new(16);
        self.metric_cache.clear(&mut self.cache_budget);
        self.shaped_text_cache.clear(&mut self.cache_budget);
        while self.glyph_image_cache.evict(&mut self.cache_budget) {}
    }
    pub(crate) fn set_plan_capacity(&mut self, capacity: usize) {
        if self.plan_capacity == capacity {
            return;
        }
        self.plan_capacity = capacity;
        for entry in &mut self.shaper_data_cache.entries {
            entry.value.plans.truncate(capacity);
            if entry.value.plans.capacity() > capacity {
                entry.value.plans.shrink_to_fit();
            }
        }
    }
    pub(crate) fn set_face_capacity(&mut self, capacity: usize) {
        let capacity = capacity.max(1);
        if self.shaper_data_cache.capacity != capacity {
            self.shaper_data_cache = SmallLru::new(capacity);
        }
    }
    pub(crate) fn cached_faces(&self) -> usize {
        self.shaper_data_cache.entries.len()
    }
    pub(crate) fn cached_plans(&self) -> usize {
        self.shaper_data_cache
            .entries
            .iter()
            .map(|entry| entry.value.plans.len())
            .sum()
    }

    pub(crate) fn rasterization(&self) -> RasterizationOptions {
        RasterizationOptions {
            regular_gamma: f32::from_bits(self.regular_coverage_key),
            emphasized_gamma: f32::from_bits(self.emphasized_coverage_key),
            hinting: self.hinting,
            positioning: self.positioning,
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
        self.stats.registrations += 1;
        self.clear_caches();
        self.shaper_data_cache = SmallLru::new(16);
        true
    }

    pub fn set_author_fonts(&mut self, enabled: bool) {
        if self.author_fonts != enabled {
            self.clear_caches();
        }
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
        if let Some(metrics) = self.metric_cache.get(text, style, 1.0) {
            self.stats.metric_hits += 1;
            return *metrics;
        }
        self.stats.metric_misses += 1;
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
        self.metric_cache
            .insert_new(text, style, 1.0, metrics, 0, &mut self.cache_budget);
        metrics
    }

    pub fn cap_height(&mut self, style: &TextStyle) -> f32 {
        self.select_face("M", style, Some(fontique::Script::from_bytes(*b"Latn")))
            .and_then(|face| font_cap_height(&face, style.size))
            .unwrap_or(style.size * 0.7)
    }

    pub fn ink_bottom(&mut self, text: &str, style: &TextStyle) -> f32 {
        let shaped = self.shape(text, style, 1.0);
        let mut bottom = f32::NEG_INFINITY;
        let mut pen_y = 0.0;
        for segment in &shaped.segments {
            let Ok(font) =
                SkrifaFontRef::from_index(segment.face.blob.as_ref(), segment.face.index)
            else {
                return self.measure(text, style).descent;
            };
            let metrics = font.glyph_metrics(
                Size::new(style.size),
                skrifa::instance::LocationRef::default(),
            );
            for glyph in &segment.glyphs {
                if let Some(bounds) = metrics.bounds(skrifa::GlyphId::new(u32::from(glyph.id))) {
                    bottom = bottom.max(pen_y - glyph.y_offset - bounds.y_min);
                }
                pen_y -= glyph.y_advance;
            }
        }
        if bottom.is_finite() {
            bottom
        } else {
            self.measure(text, style).descent
        }
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
                let image =
                    glyph_image_cache.get_or_insert_with(key, &mut self.cache_budget, || {
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
                if let Some(image) = image.as_ref() {
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
            self.stats.shaped_hits += 1;
            return Arc::clone(shaped);
        }
        self.stats.shaped_misses += 1;
        let shaped = Arc::new(self.shape_uncached(text, style, scale));
        self.shaped_text_cache.insert_new(
            text,
            style,
            scale,
            Arc::clone(&shaped),
            shaped_text_bytes(&shaped),
            &mut self.cache_budget,
        );
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

        let stats = &mut self.stats;
        let plan_capacity = self.plan_capacity;
        let mut segments: Vec<_> = runs
            .into_iter()
            .filter_map(|(face, text, source_offset)| {
                let font = HarfFontRef::from_index(face.blob.as_ref(), face.index).ok()?;
                let shaper_data = self.shaper_data_cache.get_or_insert_with(
                    ShaperDataKey {
                        blob_id: face.blob.id(),
                        index: face.index,
                    },
                    || {
                        stats.shaper_data_builds += 1;
                        FaceShaper {
                            data: ShaperData::new(&font),
                            plans: Vec::new(),
                        }
                    },
                );
                let shaper = shaper_data.data.shaper(&font).build();
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
                // We only guess (never explicitly set) the script. HarfRust's
                // getter returns UNKNOWN when guessing left the buffer's script
                // unset; preserve that None rather than changing buffer input.
                let script = buffer.script();
                let script = (script != harfrust::script::UNKNOWN).then_some(script);
                let language = buffer.language();
                let key = ShapePlanKey::new(script, buffer.direction()).language(language.as_ref());
                let plan = if plan_capacity == 0 {
                    stats.plan_builds += 1;
                    None
                } else {
                    if let Some(index) = shaper_data.plans.iter().position(|plan| key.matches(plan))
                    {
                        stats.plan_hits += 1;
                        if index + 1 != shaper_data.plans.len() {
                            let plan = shaper_data.plans.remove(index);
                            shaper_data.plans.push(plan);
                        }
                    } else {
                        stats.plan_builds += 1;
                        if shaper_data.plans.len() == plan_capacity {
                            shaper_data.plans.remove(0);
                        }
                        shaper_data.plans.push(ShapePlan::new(
                            &shaper,
                            buffer.direction(),
                            script,
                            language.as_ref(),
                            &[],
                        ));
                    }
                    Some(shaper_data.plans.last().expect("a plan was selected"))
                };
                let precision = 64.0;
                let shaped = shaper.shape(
                    buffer,
                    ShapeOptions::new()
                        .plan(plan)
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

    fn cap_height(&self, style: &TextStyle) -> f32 {
        self.borrow_mut().cap_height(style)
    }

    fn ink_bottom(&self, text: &str, style: &TextStyle) -> f32 {
        self.borrow_mut().ink_bottom(text, style)
    }
}

fn font_metrics(face: &Face, size: f32) -> Option<(f32, f32)> {
    let font = SkrifaFontRef::from_index(face.blob.as_ref(), face.index).ok()?;
    let metrics = font.metrics(Size::new(size), skrifa::instance::LocationRef::default());
    Some((metrics.ascent.max(0.0), (-metrics.descent).max(0.0)))
}

fn font_cap_height(face: &Face, size: f32) -> Option<f32> {
    let font = SkrifaFontRef::from_index(face.blob.as_ref(), face.index).ok()?;
    let location = skrifa::instance::LocationRef::default();
    let metrics = font.metrics(Size::new(size), location);
    metrics
        .cap_height
        .filter(|height| height.is_finite() && *height > 0.0)
        .or_else(|| {
            // Older fonts can omit OS/2 sCapHeight. Estimate from a flat-topped
            // uppercase glyph, not the ascender metric (which includes accents).
            let glyph = font.charmap().map('H')?;
            let bounds = font
                .glyph_metrics(Size::new(size), location)
                .bounds(glyph)?;
            (bounds.y_max.is_finite() && bounds.y_max > 0.0).then_some(bounds.y_max)
        })
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
    fn cap_height_uses_font_metrics_or_uppercase_bounds_and_scales() {
        let mut fonts = FontSystem::default();
        assert!(fonts.register_font(FontResource::from_bytes(Arc::from(
            crate::test_fonts::fixture("literata.ttf")
        ))));
        for family in ["Charter", "Literata"] {
            let mut style = body_style();
            style.family = FontFamily::Named(family.into());
            style.size = 20.0;
            let face = fonts
                .select_face("M", &style, Some(fontique::Script::from_bytes(*b"Latn")))
                .unwrap();
            let font = SkrifaFontRef::from_index(face.blob.as_ref(), face.index).unwrap();
            let location = skrifa::instance::LocationRef::default();
            let metrics = font.metrics(Size::new(style.size), location);
            let expected = metrics
                .cap_height
                .filter(|height| *height > 0.0)
                .unwrap_or_else(|| {
                    let glyph = font.charmap().map('H').unwrap();
                    font.glyph_metrics(Size::new(style.size), location)
                        .bounds(glyph)
                        .unwrap()
                        .y_max
                });
            let height = fonts.cap_height(&style);
            assert!((height - expected).abs() < 0.001);
            // Synthetic missing-metric variant: downgrade OS/2 to version 1,
            // which has no sCapHeight. Keep the glyph outlines unchanged.
            let mut bytes = face.blob.as_ref().to_vec();
            let tables = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
            let record = (0..tables)
                .map(|i| 12 + i * 16)
                .find(|i| &bytes[*i..*i + 4] == b"OS/2")
                .unwrap();
            let offset =
                u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap()) as usize;
            bytes[offset..offset + 2].copy_from_slice(&1u16.to_be_bytes());
            let mut no_metric = face.clone();
            no_metric.blob = Blob::new(Arc::new(bytes));
            let glyph = font.charmap().map('H').unwrap();
            let glyph_top = font
                .glyph_metrics(Size::new(style.size), location)
                .bounds(glyph)
                .unwrap()
                .y_max;
            assert!((font_cap_height(&no_metric, style.size).unwrap() - glyph_top).abs() < 0.001);
            assert!(height > 0.0 && height < fonts.measure("H", &style).ascent);
            style.size *= 2.0;
            assert!((fonts.cap_height(&style) - height * 2.0).abs() < 0.001);
        }
        // The public adapter must not use TextMeasurer's approximate default.
        let fonts = std::cell::RefCell::new(fonts);
        let style = body_style();
        let expected = fonts.borrow_mut().cap_height(&style);
        assert_eq!(TextMeasurer::cap_height(&fonts, &style), expected);
    }

    #[test]
    fn initial_ink_bottom_uses_glyph_bounds_not_font_descent() {
        let mut fonts = FontSystem::default();
        let mut style = body_style();
        style.size = 30.0;
        let capital_bottom = fonts.ink_bottom("F", &style);
        let descender_bottom = fonts.ink_bottom("g", &style);
        assert!(capital_bottom.abs() < 0.1);
        assert!(descender_bottom > 0.0);
        style.size *= 2.0;
        assert!((fonts.ink_bottom("g", &style) - descender_bottom * 2.0).abs() < 0.001);
        let fonts = std::cell::RefCell::new(fonts);
        let expected = fonts.borrow_mut().ink_bottom("g", &style);
        assert_eq!(TextMeasurer::ink_bottom(&fonts, "g", &style), expected);
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
        let mut budget = CacheBudget {
            limit: entry_bytes * 2,
            ..CacheBudget::default()
        };
        cache.get_or_insert_with(key(1), &mut budget, image);
        cache.get_or_insert_with(key(2), &mut budget, image);
        cache.get_or_insert_with(key(1), &mut budget, || {
            panic!("a cached glyph must not be rendered again")
        });
        cache.get_or_insert_with(key(3), &mut budget, image);

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
            crate::test_fonts::fixture("charter.ttf"),
            crate::test_fonts::fixture("noto-emoji.ttf"),
            crate::test_fonts::fixture("noto-sans-arabic.ttf"),
            crate::test_fonts::fixture("noto-sans-hebrew.ttf"),
            crate::test_fonts::fixture("noto-sans-devanagari.ttf"),
            crate::test_fonts::fixture("noto-sans.ttf"),
            crate::test_fonts::fixture("noto-sans-jp.ttf"),
            crate::test_fonts::fixture("fira-code.ttf"),
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
