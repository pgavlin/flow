pub use crate::images::{ImagePixels, ImageRequest};
#[cfg(test)]
use crate::pagination::{PageMap, paginate};
pub use crate::text::{FontResource, GlyphPositioning, RasterizationOptions};
use crate::{
    font_context::{FontContext, FontContextOptions, FontDescription, FontHost, FontSlot},
    images::ImageCache,
    layout::{
        self, ComposedDocument, ContinuousLayout, FontFamily, FontWeight, ImageMetrics,
        LayoutOptions, TableOverflowPolicy, TextMeasurer, TextMetrics, TextStyle,
    },
    model::TextRange,
    navigation::{NavigationKind, NavigationTarget},
    pagination::{AtomicFragment, BlockBoxFragment, PageFragment, PaginatedLayout, RuleFragment},
    pixels::{PaintResourceId, PaintTarget, PixelBuffer},
    selection::{Selection, TextPosition},
    settings::RenderSettings,
    syntax::SyntaxKind,
    text::FontSystem,
    theme::{Palette, Theme},
};
use anyhow::Result;
use std::{cell::RefCell, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;

use crate::model::flow::{FlowDocument, NodeKind, StyleContextKey};

pub const POINTS_TO_LOGICAL_PIXELS: f32 = 96.0 / 72.0;

fn image_paint_resource_id(resource: u64, width: u32, height: u32) -> PaintResourceId {
    let mut id = [0; 16];
    id[..8].copy_from_slice(&resource.to_le_bytes());
    id[8..12].copy_from_slice(&width.to_le_bytes());
    id[12..].copy_from_slice(&height.to_le_bytes());
    PaintResourceId(id)
}

#[cfg(feature = "telemetry")]
fn paint_target_kind<T: PaintTarget>() -> &'static str {
    let name = std::any::type_name::<T>();
    if name.contains("PixelSurface") {
        "pixels"
    } else {
        "other"
    }
}

pub struct Renderer {
    pub(crate) settings: RenderSettings,
    layout_revision: u64,
    interactive_overflow: bool,
    environment_key: [u8; 32],
    pub(crate) fonts: FontSlot,
    font_data: Vec<FontResource>,
    images: RefCell<ImageCache>,
}

pub struct ViewRender {
    pub surface: PixelBuffer,
    pub layout: ContinuousLayout,
    pub content_x: f32,
    pub content_height: f32,
}

pub struct PageRender {
    pub surface: PixelBuffer,
    pub layout: ContinuousLayout,
    pub pages: PaginatedLayout,
    pub page_index: usize,
    pub content_x: f32,
}

#[derive(Clone, Copy)]
pub struct PageSelection {
    pub requested_page: usize,
    pub text_anchor: Option<usize>,
}

#[derive(Clone, Copy, Default)]
pub struct InteractionState<'a> {
    /// The semantic flow link target under the pointer.
    pub hovered_target: Option<&'a NavigationTarget>,
    pub focused: Option<&'a NavigationTarget>,
    pub search_matches: &'a [TextRange],
    pub active_match: Option<TextRange>,
    pub selection: Option<Selection>,
    /// Interactive viewers opt in; static PNG/print callers retain their policy.
    pub overflow: Option<&'a crate::overflow::HorizontalOverflowState>,
}

impl<'a> InteractionState<'a> {
    fn semantic_focus(self) -> Option<&'a NavigationTarget> {
        self.focused.filter(|target| {
            // A keyboard-focused overflow owner has one viewport outline, not
            // an additional semantic decoration on every authored code line.
            self.overflow
                .and_then(|state| state.focused())
                .is_none_or(|owner| target.flow_node() != Some(owner))
        })
    }
}

#[derive(Clone, Copy)]
pub struct PaintOptions<'a> {
    pub scale: f32,
    pub interaction: InteractionState<'a>,
}

/// A logical rectangle in the complete continuous document surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocumentRegion {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl FontHost for Renderer {
    fn font_slot(&mut self) -> &mut FontSlot {
        &mut self.fonts
    }
}

impl Renderer {
    pub fn new(settings: impl Into<RenderSettings>) -> Self {
        let settings = settings.into();
        let fonts = FontSlot::new(
            &settings,
            Vec::new(),
            FontContextOptions::default().cache_bytes,
        );
        let mut renderer = Self {
            settings,
            layout_revision: 0,
            interactive_overflow: false,
            environment_key: [0; 32],
            fonts,
            font_data: Vec::new(),
            images: RefCell::new(ImageCache::default()),
        };
        renderer.update_environment_key();
        renderer
    }

    pub(crate) fn enable_interactive_overflow(&mut self) {
        self.interactive_overflow = true;
        self.layout_revision = self.layout_revision.wrapping_add(1);
        self.update_environment_key();
    }

    pub fn settings(&self) -> &RenderSettings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: RenderSettings) {
        if !self.settings.layout_equivalent(&settings) {
            self.layout_revision = self.layout_revision.wrapping_add(1);
        }
        if self.settings.body_fonts != settings.body_fonts
            || self.settings.code_fonts != settings.code_fonts
            || self.settings.author_fonts != settings.author_fonts
        {
            self.fonts.rebind(&settings, self.font_data.clone());
        } else {
            self.fonts.set_rasterization(settings.rasterization());
        }
        self.settings = settings;
    }

    /// Replaces a scoped font environment transactionally. Source font family
    /// names cannot leak from a previously active publication.
    pub fn set_font_resources(&mut self, resources: Vec<FontResource>) -> bool {
        if self
            .font_data
            .iter()
            .map(FontResource::digest)
            .eq(resources.iter().map(FontResource::digest))
        {
            return true;
        }
        let mut fonts = FontSlot::new(&self.settings, resources.clone(), self.fonts.limit);
        if !fonts.validate() {
            return false;
        }
        self.fonts.replace(fonts);
        self.font_data = resources;
        self.update_environment_key();
        true
    }

    /// Run one complete operation using an explicitly borrowed font owner.
    /// The backend returns to the context on success or unwinding.
    pub fn with_font_context<R>(
        &mut self,
        context: &mut FontContext,
        operation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        context.scope(self, operation)
    }

    pub fn set_font_resources_with_context(
        &mut self,
        resources: Vec<FontResource>,
        context: &mut FontContext,
    ) -> bool {
        if self
            .font_data
            .iter()
            .map(FontResource::digest)
            .eq(resources.iter().map(FontResource::digest))
        {
            return true;
        }
        let description = FontDescription::new(&self.settings, resources.clone());
        if !context.validate(&description) {
            return false;
        }
        self.fonts.rebind(&self.settings, resources.clone());
        self.font_data = resources;
        self.update_environment_key();
        true
    }

    pub fn set_resource_namespace(&mut self, namespace: Option<String>) {
        if self.images.get_mut().namespace != namespace {
            self.images.get_mut().namespace = namespace;
            self.update_environment_key();
        }
    }

    fn update_environment_key(&mut self) {
        let mut digest = blake3::Hasher::new();
        digest.update(b"flow-layout-environment-v1\0");
        digest.update(&[u8::from(self.interactive_overflow)]);
        digest.update(&[u8::from(self.images.get_mut().namespace.is_some())]);
        let namespace = self
            .images
            .get_mut()
            .namespace
            .as_deref()
            .unwrap_or_default();
        digest.update(&(namespace.len() as u64).to_le_bytes());
        digest.update(namespace.as_bytes());
        for font in &self.font_data {
            digest.update(&font.digest());
        }
        self.environment_key = *digest.finalize().as_bytes();
    }

    pub(crate) fn environment_key(&self) -> [u8; 32] {
        self.environment_key
    }

    pub fn register_font(&mut self, data: Arc<[u8]>) -> bool {
        self.register_font_resource(FontResource::from_bytes(data))
    }

    pub fn register_font_resource(&mut self, resource: FontResource) -> bool {
        let mut resources = self.font_data.clone();
        resources.push(resource);
        if !self.set_font_resources(resources) {
            return false;
        }
        self.layout_revision = self.layout_revision.wrapping_add(1);
        true
    }

    pub fn register_image(
        &mut self,
        url: &str,
        resource_base: Option<&str>,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> bool {
        let Some(pixels) = ImagePixels::from_rgba(width, height, rgba) else {
            return false;
        };
        self.register_image_pixels(url, resource_base, pixels)
    }

    pub fn register_image_pixels(
        &mut self,
        url: &str,
        resource_base: Option<&str>,
        pixels: ImagePixels,
    ) -> bool {
        let request = self.images.borrow().request(url, resource_base);
        if !self.images.borrow_mut().register(request, pixels) {
            return false;
        }
        self.layout_revision = self.layout_revision.wrapping_add(1);
        true
    }

    pub fn provide_image_metadata(
        &mut self,
        request: ImageRequest,
        width: u32,
        height: u32,
    ) -> bool {
        if !self
            .images
            .borrow_mut()
            .provide_metadata(request, width, height)
        {
            return false;
        }
        self.layout_revision = self.layout_revision.wrapping_add(1);
        true
    }

    pub fn provide_image_rgba(
        &mut self,
        request: ImageRequest,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> bool {
        let Some(pixels) = ImagePixels::from_rgba(width, height, rgba) else {
            return false;
        };
        self.provide_image_pixels(request, pixels)
    }

    pub fn provide_image_pixels(&mut self, request: ImageRequest, pixels: ImagePixels) -> bool {
        let Some(dimensions_changed) = self.images.borrow_mut().provide_pixels(request, pixels)
        else {
            return false;
        };
        if dimensions_changed {
            self.layout_revision = self.layout_revision.wrapping_add(1);
        }
        true
    }

    pub fn provide_image_bytes(&mut self, request: ImageRequest, bytes: &[u8]) -> bool {
        let Ok(mut image) = image::load_from_memory(bytes) else {
            return false;
        };
        if let (Some(width), Some(height)) = (request.desired_width, request.desired_height)
            && (image.width(), image.height()) != (width, height)
        {
            image = image.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
        }
        let Some(dimensions_changed) = self
            .images
            .borrow_mut()
            .provide_pixels(request, ImagePixels::from(image.to_rgba8()))
        else {
            return false;
        };
        if dimensions_changed {
            self.layout_revision = self.layout_revision.wrapping_add(1);
        }
        true
    }

    pub fn fail_image_metadata(&self, request: &ImageRequest) {
        self.images.borrow_mut().fail_metadata(request);
    }

    pub fn fail_image_pixels(&self, request: &ImageRequest) {
        self.images.borrow_mut().fail_pixels(request);
    }

    pub fn retry_image(&self, request: &ImageRequest) {
        self.images.borrow_mut().retry(request);
    }

    pub fn trim_image_cache(&self) {
        self.images.borrow_mut().trim();
    }

    pub(crate) fn discover_image_metadata(&self, document: &FlowDocument) {
        let mut images = self.images.borrow_mut();
        for id in std::iter::successors(Some(document.root()), |id| document.following(*id)) {
            let Some(node) = document.get_node(id) else {
                continue;
            };
            let NodeKind::Image(image) = &node.kind else {
                continue;
            };
            if let Some(resource) = document.get_resource(image.resource) {
                let request = images.request(&resource.identity, document.resource_base());
                if let Some(size) = image.intrinsic_size
                    && images.hint_dimensions(
                        request,
                        size.width.get().ceil() as u32,
                        size.height.get().ceil() as u32,
                    )
                {
                    continue;
                }
                images.measure(&resource.identity, document.resource_base());
            }
        }
    }

    pub(crate) fn take_image_metadata_requests(&self) -> Vec<ImageRequest> {
        self.images.borrow_mut().take_metadata_requests()
    }

    pub(crate) fn take_image_pixel_requests(&self) -> Vec<ImageRequest> {
        self.images.borrow_mut().take_pixel_requests()
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        if self.settings.font_size.to_bits() != font_size.to_bits() {
            self.settings.font_size = font_size;
            self.layout_revision = self.layout_revision.wrapping_add(1);
        }
    }

    pub fn set_line_spacing(&mut self, line_spacing: f32) {
        if self.settings.theme.line_spacing.to_bits() != line_spacing.to_bits() {
            self.settings.theme.line_spacing = line_spacing;
            self.layout_revision = self.layout_revision.wrapping_add(1);
        }
    }

    pub fn set_max_width(&mut self, max_width: f32) {
        if self.settings.max_width.to_bits() != max_width.to_bits() {
            self.settings.max_width = max_width;
            self.layout_revision = self.layout_revision.wrapping_add(1);
        }
    }

    pub(crate) fn layout_revision(&self) -> u64 {
        self.layout_revision
    }

    #[doc(hidden)]
    pub fn persistent_layout_digest(&self) -> [u8; 32] {
        Self::persistent_layout_digest_for(&self.settings, &self.font_data)
    }

    #[doc(hidden)]
    pub fn persistent_layout_digest_for(
        settings: &RenderSettings,
        fonts: &[FontResource],
    ) -> [u8; 32] {
        let mut digest = blake3::Hasher::new();
        // Layout algorithm changes must invalidate persisted composition/page artifacts.
        digest.update(b"flow-layout-dependencies-v3\0");
        let mut settings = settings.clone();
        settings.regular_gamma = crate::settings::DEFAULT_REGULAR_GAMMA;
        settings.emphasized_gamma = crate::settings::DEFAULT_EMPHASIZED_GAMMA;
        settings.dark_mode = false;

        settings.theme.light = crate::theme::Palette::light();
        settings.theme.dark = crate::theme::Palette::dark();
        if let Ok(settings) = serde_json::to_vec(&settings) {
            digest.update(&settings);
        }
        for font in fonts {
            digest.update(&font.digest());
        }
        *digest.finalize().as_bytes()
    }

    pub fn set_dark_mode(&mut self, dark_mode: bool) {
        self.settings.dark_mode = dark_mode;
    }

    pub(crate) fn palette(&self) -> Palette {
        if self.settings.dark_mode {
            self.settings.theme.dark
        } else {
            self.settings.theme.light
        }
    }

    pub fn set_rasterization(&mut self, options: RasterizationOptions) {
        self.settings.regular_gamma = options.regular_gamma;
        self.settings.emphasized_gamma = options.emphasized_gamma;
        self.settings.font_hinting = options.hinting;
        self.settings.fractional_positioning = options.positioning == GlyphPositioning::Fractional;
        self.fonts.set_rasterization(options);
    }

    pub fn render_viewport(
        &self,
        document: &FlowDocument,
        width: u32,
        height: u32,
        scroll_y: f32,
        scale: f32,
        interaction: InteractionState<'_>,
    ) -> Result<ViewRender> {
        let content_width = self.content_width(width as f32);
        let layout = self.prepare_flow_layout(document, content_width);
        let (surface, content_x, content_height) =
            self.paint_viewport(&layout, width, height, scroll_y, scale, interaction)?;
        Ok(ViewRender {
            surface,
            content_height,
            layout,
            content_x,
        })
    }

    pub fn render_page(
        &self,
        document: &FlowDocument,
        viewport: (u32, u32),
        requested_page: usize,
        text_anchor: Option<usize>,
        scale: f32,
        interaction: InteractionState<'_>,
    ) -> Result<PageRender> {
        let content_width = self.content_width(viewport.0 as f32);
        let composed = self.compose_document(document, content_width);
        let layout = composed.continuous_layout();
        let pages = self.paginate_composed_document(&composed, viewport.1)?;
        let selection = PageSelection {
            requested_page,
            text_anchor,
        };
        let mut surface = create_surface(
            viewport.0 as f32,
            viewport.1 as f32,
            scale,
            self.palette().background,
        )?;
        let (page_index, content_x) = self.paint_paginated_page_into(
            &mut surface.surface_mut(),
            &composed,
            &pages,
            viewport,
            selection,
            PaintOptions { scale, interaction },
        )?;
        Ok(PageRender {
            surface,
            layout,
            pages,
            page_index,
            content_x,
        })
    }

    pub fn effective_content_width(&self, viewport_width: u32) -> f32 {
        self.content_width(viewport_width as f32)
    }

    pub fn compose_document(
        &self,
        document: &FlowDocument,
        content_width: f32,
    ) -> ComposedDocument {
        self.compose_flow_document(document, content_width)
    }

    pub fn place_composed_document(&self, document: ComposedDocument) -> ContinuousLayout {
        let _span = crate::telemetry::phase_span!(
            "flow.layout.continuous-place",
            blocks = document.blocks.len() as u64,
        );
        document.into_continuous_layout()
    }

    pub fn prepare_layout(&self, document: &FlowDocument, content_width: f32) -> ContinuousLayout {
        self.prepare_flow_layout(document, content_width)
    }

    pub fn paginated_body_height(&self, height: u32) -> f32 {
        let top = self.settings.theme.page_margin;
        let bottom = top;
        (height as f32 - top - bottom).max(1.0)
    }

    pub fn paginate_composed_document(
        &self,
        document: &ComposedDocument,
        height: u32,
    ) -> Result<PaginatedLayout> {
        let _span =
            crate::telemetry::phase_span!("flow.layout.paginate", viewport_height = height as u64,);
        let input = crate::pagination::build_page_items(document);
        let plan = crate::pagination::break_page_items(&input, self.paginated_body_height(height))
            .map_err(|error| anyhow::anyhow!("page breaking failed: {error:?}"))?;
        crate::pagination::materialize_paginated_layout(document, &input, &plan)
            .map_err(|error| anyhow::anyhow!("page materialization failed: {error:?}"))
    }

    #[cfg(test)]
    pub fn paginate_layout(&self, layout: &ContinuousLayout, height: u32) -> PageMap {
        paginate(layout, self.paginated_body_height(height))
    }

    pub fn paint_viewport(
        &self,
        layout: &ContinuousLayout,
        width: u32,
        height: u32,
        scroll_y: f32,
        scale: f32,
        interaction: InteractionState<'_>,
    ) -> Result<(PixelBuffer, f32, f32)> {
        let mut surface = create_surface(
            width as f32,
            height as f32,
            scale,
            self.palette().background,
        )?;
        let (content_x, content_height) = self.paint_viewport_into(
            &mut surface.surface_mut(),
            layout,
            (width, height),
            scroll_y,
            PaintOptions { scale, interaction },
        )?;
        Ok((surface, content_x, content_height))
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_flow_viewport(
        &self,
        layout: &ContinuousLayout,
        width: u32,
        height: u32,
        scroll_y: f32,
        scale: f32,
        interaction: InteractionState<'_>,
        hovered: Option<&NavigationTarget>,
    ) -> Result<(PixelBuffer, f32, f32)> {
        let mut surface = create_surface(
            width as f32,
            height as f32,
            scale,
            self.palette().background,
        )?;
        let margin = self.settings.theme.page_margin;
        let content_x = ((width as f32 - layout.width) / 2.0).max(margin);
        prepare_surface(
            &mut surface.surface_mut(),
            width as f32,
            height as f32,
            scale,
            self.palette().background,
        )?;
        self.draw_document(
            &mut surface.surface_mut(),
            layout,
            content_x,
            margin - scroll_y,
            height as f32,
            PaintOptions { scale, interaction },
            hovered,
        );
        Ok((surface, content_x, layout.height + margin * 2.0))
    }

    pub fn paint_viewport_into<T: PaintTarget>(
        &self,
        surface: &mut T,
        layout: &ContinuousLayout,
        viewport: (u32, u32),
        scroll_y: f32,
        options: PaintOptions<'_>,
    ) -> Result<(f32, f32)> {
        let (width, height) = viewport;
        let content_x = self.paint_document_region_into(
            surface,
            layout,
            width as f32,
            DocumentRegion {
                x: 0.0,
                y: scroll_y,
                width: width as f32,
                height: height as f32,
            },
            options,
        )?;
        let margin = self.settings.theme.page_margin;
        Ok((content_x, layout.height + margin * 2.0))
    }

    /// Paints a clipped region of the continuous document without changing any
    /// application scroll state. `scene_width` is the width used to center the
    /// document; `region` is expressed in complete-document logical pixels.
    pub fn paint_document_region_into<T: PaintTarget>(
        &self,
        surface: &mut T,
        layout: &ContinuousLayout,
        scene_width: f32,
        region: DocumentRegion,
        options: PaintOptions<'_>,
    ) -> Result<f32> {
        let hovered = options.interaction.hovered_target;
        self.paint_document_region_into_with_flow(
            surface,
            layout,
            scene_width,
            region,
            options,
            hovered,
        )
    }

    #[cfg(test)]
    pub(crate) fn paint_flow_document_region_into<T: PaintTarget>(
        &self,
        surface: &mut T,
        layout: &ContinuousLayout,
        scene_width: f32,
        region: DocumentRegion,
        options: PaintOptions<'_>,
        hovered: Option<&NavigationTarget>,
    ) -> Result<f32> {
        self.paint_document_region_into_with_flow(
            surface,
            layout,
            scene_width,
            region,
            options,
            hovered,
        )
    }

    fn paint_document_region_into_with_flow<T: PaintTarget>(
        &self,
        surface: &mut T,
        layout: &ContinuousLayout,
        scene_width: f32,
        region: DocumentRegion,
        options: PaintOptions<'_>,
        hovered: Option<&NavigationTarget>,
    ) -> Result<f32> {
        let _span = crate::telemetry::phase_span!(
            "flow.paint.continuous",
            viewport_width = region.width as u64,
            viewport_height = region.height as u64,
            scale = options.scale as f64,
            target = paint_target_kind::<T>(),
        );
        let PaintOptions { scale, interaction } = options;
        let margin = self.settings.theme.page_margin;
        let content_x = ((scene_width - layout.width) / 2.0).max(margin);
        prepare_surface(
            surface,
            region.width,
            region.height,
            scale,
            self.palette().background,
        )?;
        self.draw_document(
            surface,
            layout,
            content_x - region.x,
            margin - region.y,
            region.height,
            PaintOptions { scale, interaction },
            hovered,
        );
        Ok(content_x)
    }

    #[cfg(test)]
    pub fn paint_page(
        &self,
        layout: &mut ContinuousLayout,
        pages: &PageMap,
        viewport: (u32, u32),
        selection: PageSelection,
        scale: f32,
        interaction: InteractionState<'_>,
    ) -> Result<(PixelBuffer, usize, f32)> {
        let mut surface = create_surface(
            viewport.0 as f32,
            viewport.1 as f32,
            scale,
            self.palette().background,
        )?;
        let (page_index, content_x) = self.paint_page_into(
            &mut surface.surface_mut(),
            layout,
            pages,
            viewport,
            selection,
            PaintOptions { scale, interaction },
        )?;
        Ok((surface, page_index, content_x))
    }

    #[cfg(test)]
    pub fn paint_page_into<T: PaintTarget>(
        &self,
        surface: &mut T,
        layout: &mut ContinuousLayout,
        pages: &PageMap,
        viewport: (u32, u32),
        selection: PageSelection,
        options: PaintOptions<'_>,
    ) -> Result<(usize, f32)> {
        let hovered = options.interaction.hovered_target;
        self.paint_page_into_with_flow(
            surface, layout, pages, viewport, selection, options, hovered,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_flow_page_into<T: PaintTarget>(
        &self,
        surface: &mut T,
        layout: &mut ContinuousLayout,
        pages: &PageMap,
        viewport: (u32, u32),
        selection: PageSelection,
        options: PaintOptions<'_>,
        hovered: Option<&NavigationTarget>,
    ) -> Result<(usize, f32)> {
        self.paint_page_into_with_flow(
            surface, layout, pages, viewport, selection, options, hovered,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn paint_page_into_with_flow<T: PaintTarget>(
        &self,
        surface: &mut T,
        layout: &mut ContinuousLayout,
        pages: &PageMap,
        viewport: (u32, u32),
        selection: PageSelection,
        options: PaintOptions<'_>,
        hovered: Option<&NavigationTarget>,
    ) -> Result<(usize, f32)> {
        let _span = crate::telemetry::phase_span!(
            "flow.paint.page",
            viewport_width = viewport.0 as u64,
            viewport_height = viewport.1 as u64,
            requested_page = selection.requested_page as u64,
            page_count = pages.pages.len() as u64,
            scale = options.scale as f64,
            target = paint_target_kind::<T>(),
        );
        let (width, height) = viewport;
        let PaintOptions { scale, interaction } = options;
        let margin = self.settings.theme.page_margin;

        let content_x = ((width as f32 - layout.width) / 2.0).max(margin);
        let page_index = selection
            .text_anchor
            .map(|anchor| pages.page_for_text(layout, anchor))
            .unwrap_or(selection.requested_page)
            .min(pages.pages.len().saturating_sub(1));
        let page = &pages.pages[page_index];
        let original_atomic_sizes = page
            .scaled_atomics
            .iter()
            .filter_map(|&(block_index, object_index, object_scale)| {
                let object = layout
                    .blocks
                    .get_mut(block_index)?
                    .atomic_objects
                    .get_mut(object_index)?;
                let original = (block_index, object_index, object.width, object.height);
                object.resize(object.width * object_scale, object.height * object_scale);
                Some(original)
            })
            .collect::<Vec<_>>();

        let result = (|| {
            prepare_surface(
                surface,
                width as f32,
                height as f32,
                scale,
                self.palette().background,
            )?;
            let physical_width = surface.width();
            let body_height = physical_dimension(pages.body_height, scale, "height")?;
            let body_y = (margin * scale).round() as i32;
            let repeated_header_height = page
                .repeated_table_header
                .map_or(0.0, |header| header.height);
            if let Some(header) = page.repeated_table_header {
                let header_height = physical_dimension(header.height.max(1.0), scale, "height")?;
                surface.with_viewport((0, body_y), physical_width, header_height, |surface| {
                    self.draw_repeated_table_header(
                        surface,
                        layout,
                        header.block,
                        content_x,
                        header.height.max(1.0),
                        PaintOptions { scale, interaction },
                        hovered,
                    );
                });
            }
            let available_content_height = (pages.body_height - repeated_header_height).max(1.0);
            let drawn_content_height = (page.end_y - page.start_y)
                .min(available_content_height)
                .max(1.0);
            let content_height = physical_dimension(drawn_content_height, scale, "height")?;
            let content_y = body_y + (repeated_header_height * scale).round() as i32;
            surface.with_viewport((0, content_y), physical_width, content_height, |surface| {
                self.draw_document(
                    surface,
                    layout,
                    content_x,
                    -page.start_y,
                    drawn_content_height,
                    PaintOptions { scale, interaction },
                    hovered,
                );
            });
            surface.with_viewport((0, body_y), physical_width, body_height, |surface| {
                if let Some(header) = page.repeated_table_header
                    && let Some(table) = layout.blocks[header.block].table.as_ref()
                {
                    surface.fill_rect(
                        (content_x + table.x) * scale,
                        repeated_header_height * scale,
                        table.width * scale,
                        (self.settings.theme.table_light_rule_thickness * scale).max(1.0),
                        self.palette().table_rule_light,
                    );
                }
            });
            Result::<()>::Ok(())
        })();

        for (block_index, object_index, width, height) in original_atomic_sizes {
            if let Some(object) = layout
                .blocks
                .get_mut(block_index)
                .and_then(|block| block.atomic_objects.get_mut(object_index))
            {
                object.resize(width, height);
            }
        }

        result?;
        Ok((page_index, content_x))
    }

    pub fn paint_paginated_page_into<T: PaintTarget>(
        &self,
        surface: &mut T,
        document: &ComposedDocument,
        layout: &PaginatedLayout,
        viewport: (u32, u32),
        selection: PageSelection,
        options: PaintOptions<'_>,
    ) -> Result<(usize, f32)> {
        let _span = crate::telemetry::phase_span!(
            "flow.paint.page-fragments",
            viewport_width = viewport.0 as u64,
            viewport_height = viewport.1 as u64,
            requested_page = selection.requested_page as u64,
            page_count = layout.pages.len() as u64,
            scale = options.scale as f64,
            target = paint_target_kind::<T>(),
        );
        let (width, height) = viewport;
        let margin = self.settings.theme.page_margin;

        let content_x = ((width as f32 - layout.width) / 2.0).max(margin);
        let page_index = selection
            .text_anchor
            .map(|anchor| layout.page_for_text(anchor))
            .unwrap_or(selection.requested_page)
            .min(layout.pages.len().saturating_sub(1));
        prepare_surface(
            surface,
            width as f32,
            height as f32,
            options.scale,
            self.palette().background,
        )?;
        let physical_width = surface.width();
        let page = &layout.pages[page_index];
        let header_height = page
            .repeated_table_header
            .map_or(0.0, |header| header.height);
        // Match the page's content extent, not the full body: glyph ink can
        // extend beyond the final line box even when no later fragment exists.
        let drawn_height =
            (header_height + (page.end_y - page.start_y).max(1.0)).min(layout.body_height);
        let body_height = physical_dimension(drawn_height, options.scale, "height")?;
        let body_y = (margin * options.scale).round() as i32;
        surface.with_viewport((0, body_y), physical_width, body_height, |surface| {
            self.draw_paginated_page(surface, document, layout, page_index, content_x, options);
        });
        Ok((page_index, content_x))
    }

    pub fn paint_error_into<T: PaintTarget>(
        &self,
        surface: &mut T,
        message: &str,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<()> {
        prepare_surface(
            surface,
            width as f32,
            height as f32,
            scale,
            self.palette().background,
        )?;
        let style = TextStyle {
            family: FontFamily::Proportional,
            size: self.settings.font_size * POINTS_TO_LOGICAL_PIXELS,
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
        };
        let metrics = self.fonts.measure(message, &style);
        let palette = self.palette();
        let margin = self.settings.theme.page_margin;
        self.fonts.borrow_mut().draw(
            surface,
            message,
            &style,
            (margin, margin + metrics.ascent),
            scale,
            palette.error,
        );
        Ok(())
    }

    pub fn text_position_at(
        &self,
        layout: &ContinuousLayout,
        x: f32,
        y: f32,
    ) -> Option<TextPosition> {
        if let Some(object) = layout
            .atomic_at(x, y)
            .filter(|object| object.range.start != object.range.end)
        {
            return Some(object.position_at(x));
        }
        let block = layout
            .blocks
            .iter()
            .filter(|block| !block.lines.is_empty())
            .min_by(|left, right| {
                distance_to_interval(y, left.y, left.y + left.height)
                    .total_cmp(&distance_to_interval(y, right.y, right.y + right.height))
            })?;
        if y < block.y || y >= block.y + block.height {
            return None;
        }
        let line = block.lines.iter().min_by(|left, right| {
            distance_to_interval(y, left.y, left.y + left.height)
                .total_cmp(&distance_to_interval(y, right.y, right.y + right.height))
                .then_with(|| {
                    distance_to_line_runs(x, left).total_cmp(&distance_to_line_runs(x, right))
                })
        })?;
        let run =
            line.runs.iter().min_by(|left, right| {
                distance_to_interval(x, left.x, left.x + left.width)
                    .total_cmp(&distance_to_interval(x, right.x, right.x + right.width))
            })?;
        if task_marker(&run.text).is_some() || run.range.start == run.range.end {
            return Some(TextPosition::new(line.range.start));
        }
        let mut fonts = self.fonts.borrow_mut();
        Some(position_in_run(&mut fonts, run, x))
    }

    pub fn text_position_at_paginated(
        &self,
        document: &ComposedDocument,
        layout: &PaginatedLayout,
        page_index: usize,
        x: f32,
        y: f32,
    ) -> Option<TextPosition> {
        let page = layout.pages.get(page_index)?;
        let fragments = &layout.fragments[page.fragments.as_range()];
        for fragment in fragments {
            let PageFragment::Atomic(fragment) = fragment else {
                continue;
            };
            if fragment.range.is_empty()
                || x < fragment.x
                || x >= fragment.x + fragment.width
                || y < fragment.y
                || y >= fragment.y + fragment.height
            {
                continue;
            }
            let object = document
                .blocks
                .get(fragment.block)?
                .layout
                .atomic_objects
                .get(fragment.atomic)?;
            let source_x = object.x + (x - fragment.x) / fragment.scale.max(f32::EPSILON);
            return Some(object.position_at(source_x));
        }
        let (line_fragment, block) = fragments
            .iter()
            .filter_map(|fragment| {
                let PageFragment::Lines(fragment) = fragment else {
                    return None;
                };
                let block = document.blocks.get(fragment.block)?;
                Some((fragment, block))
            })
            .min_by(|(left, _), (right, _)| {
                distance_to_interval(y, left.y, left.y + left.height)
                    .total_cmp(&distance_to_interval(y, right.y, right.y + right.height))
            })?;
        if y < line_fragment.y || y >= line_fragment.y + line_fragment.height {
            return None;
        }
        let first = block.layout.lines.get(line_fragment.line_start)?;
        let y_offset = line_fragment.y - first.y;
        let line = block.layout.lines[line_fragment.line_start..line_fragment.line_end]
            .iter()
            .min_by(|left, right| {
                distance_to_interval(y, y_offset + left.y, y_offset + left.y + left.height)
                    .total_cmp(&distance_to_interval(
                        y,
                        y_offset + right.y,
                        y_offset + right.y + right.height,
                    ))
                    .then_with(|| {
                        distance_to_line_runs(x, left).total_cmp(&distance_to_line_runs(x, right))
                    })
            })?;
        let run =
            line.runs.iter().min_by(|left, right| {
                distance_to_interval(x, left.x, left.x + left.width)
                    .total_cmp(&distance_to_interval(x, right.x, right.x + right.width))
            })?;
        if task_marker(&run.text).is_some() || run.range.is_empty() {
            return Some(TextPosition::new(line.range.start));
        }
        let mut fonts = self.fonts.borrow_mut();
        Some(position_in_run(&mut fonts, run, x))
    }

    pub fn adjacent_text_position(
        &self,
        layout: &ContinuousLayout,
        position: TextPosition,
        direction: isize,
    ) -> Option<TextPosition> {
        let position = layout.resolve_atomic_position(position);
        let mut fonts = self.fonts.borrow_mut();
        let mut visual = Vec::new();
        for line in layout.blocks.iter().flat_map(|block| &block.lines) {
            for run in &line.runs {
                if task_marker(&run.text).is_some() || run.range.start == run.range.end {
                    continue;
                }
                let leading = TextPosition {
                    offset: run.range.start,
                    affinity: crate::model::Affinity::Downstream,
                };
                let trailing = TextPosition {
                    offset: run.range.end,
                    affinity: crate::model::Affinity::Upstream,
                };
                visual.push((
                    line.y,
                    x_for_source(&mut fonts, run, run.range.start),
                    leading,
                ));
                if run.range.end.saturating_sub(run.range.start) == run.text.len() {
                    visual.extend(run.text.grapheme_indices(true).skip(1).map(|(offset, _)| {
                        let offset = run.range.start + offset;
                        (
                            line.y,
                            x_for_source(&mut fonts, run, offset),
                            TextPosition::new(offset),
                        )
                    }));
                }
                visual.push((
                    line.y,
                    x_for_source(&mut fonts, run, run.range.end),
                    trailing,
                ));
            }
        }
        for object in layout.atomic_objects() {
            if object.range.start == object.range.end {
                continue;
            }
            visual.push((
                object.y,
                object.leading_x,
                TextPosition {
                    offset: object.range.start,
                    affinity: crate::model::Affinity::Downstream,
                },
            ));
            visual.push((
                object.y,
                object.trailing_x,
                TextPosition {
                    offset: object.range.end,
                    affinity: crate::model::Affinity::Upstream,
                },
            ));
        }
        drop(fonts);
        visual.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
        });
        visual.dedup_by(|left, right| {
            left.2.offset == right.2.offset && left.0 == right.0 && left.1 == right.1
        });
        let index = visual
            .iter()
            .position(|candidate| candidate.2 == position)
            .or_else(|| {
                let x = self.caret_x(layout, position)?;
                visual
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| candidate.2.offset == position.offset)
                    .min_by(|(_, left), (_, right)| {
                        (left.1 - x).abs().total_cmp(&(right.1 - x).abs())
                    })
                    .map(|(index, _)| index)
            })?;
        let next = if direction.is_negative() {
            index.saturating_sub(1)
        } else {
            (index + 1).min(visual.len().saturating_sub(1))
        };
        visual.get(next).map(|entry| entry.2)
    }

    pub fn caret_x(&self, layout: &ContinuousLayout, position: TextPosition) -> Option<f32> {
        if let Some(x) = layout
            .atomic_objects()
            .find_map(|object| object.caret_x(position))
        {
            return Some(x);
        }
        let runs = layout
            .blocks
            .iter()
            .flat_map(|block| &block.lines)
            .flat_map(|line| &line.runs);
        let run = runs
            .clone()
            .find(|run| match position.affinity {
                crate::model::Affinity::Downstream => {
                    run.range.start == position.offset
                        || run.range.start < position.offset && position.offset < run.range.end
                }
                crate::model::Affinity::Upstream => {
                    run.range.end == position.offset
                        || run.range.start < position.offset && position.offset < run.range.end
                }
            })
            .or_else(|| {
                runs.clone().find(|run| {
                    run.range.contains(position.offset) || run.range.end == position.offset
                })
            })?;
        let mut fonts = self.fonts.borrow_mut();
        Some(x_for_source(&mut fonts, run, position.offset))
    }

    pub fn vertical_text_position(
        &self,
        layout: &ContinuousLayout,
        position: TextPosition,
        direction: isize,
        preferred_x: f32,
    ) -> Option<TextPosition> {
        enum VerticalRow<'a> {
            Line(&'a layout::LineLayout),
            Atomic(&'a layout::AtomicLayout),
        }
        let all_lines = layout
            .blocks
            .iter()
            .flat_map(|block| &block.lines)
            .collect::<Vec<_>>();
        let mut rows = all_lines
            .iter()
            .copied()
            .filter(|line| {
                !line.runs.is_empty()
                    || layout.atomic_objects().any(|object| {
                        object.y < line.y + line.height && object.y + object.height > line.y
                    })
            })
            .map(VerticalRow::Line)
            .collect::<Vec<_>>();
        rows.extend(
            layout
                .atomic_objects()
                .filter(|object| {
                    !all_lines.iter().any(|line| {
                        object.y < line.y + line.height && object.y + object.height > line.y
                    })
                })
                .map(VerticalRow::Atomic),
        );
        rows.sort_by(|left, right| {
            let y = |row: &VerticalRow<'_>| match row {
                VerticalRow::Line(line) => line.y,
                VerticalRow::Atomic(object) => object.y,
            };
            y(left).total_cmp(&y(right))
        });
        let position = layout.resolve_atomic_position(position);
        let current = rows
            .iter()
            .position(|row| match row {
                VerticalRow::Atomic(object) => object.resolve_position(position).is_some(),
                VerticalRow::Line(_) => false,
            })
            .or_else(|| {
                rows.iter().position(|row| match row {
                    VerticalRow::Line(line) => {
                        line.range.contains(position.offset) || line.range.end == position.offset
                    }
                    VerticalRow::Atomic(_) => false,
                })
            })?;
        let next = current
            .saturating_add_signed(direction)
            .min(rows.len().saturating_sub(1));
        let VerticalRow::Line(line) = rows[next] else {
            let VerticalRow::Atomic(object) = rows[next] else {
                unreachable!()
            };
            return Some(object.position_at(preferred_x));
        };
        let run = line.runs.iter().min_by(|left, right| {
            distance_to_interval(preferred_x, left.x, left.x + left.width).total_cmp(
                &distance_to_interval(preferred_x, right.x, right.x + right.width),
            )
        });
        let object = layout
            .atomic_objects()
            .filter(|object| object.y < line.y + line.height && object.y + object.height > line.y)
            .min_by(|left, right| {
                distance_to_interval(preferred_x, left.x, left.x + left.width).total_cmp(
                    &distance_to_interval(preferred_x, right.x, right.x + right.width),
                )
            });
        if let Some(object) = object
            && run.is_none_or(|run| {
                distance_to_interval(preferred_x, object.x, object.x + object.width)
                    <= distance_to_interval(preferred_x, run.x, run.x + run.width)
            })
        {
            return Some(object.position_at(preferred_x));
        }
        let mut fonts = self.fonts.borrow_mut();
        run.map(|run| position_in_run(&mut fonts, run, preferred_x))
    }

    /// Renders the complete continuous document into host-readable pixels.
    /// The host chooses any output encoding and storage.
    pub fn render_full_document(&self, document: &FlowDocument, scale: f32) -> Result<PixelBuffer> {
        let content_width = self.settings.max_width * POINTS_TO_LOGICAL_PIXELS;
        let layout = self.prepare_flow_layout(document, content_width);
        let margin = self.settings.theme.page_margin;
        let logical_width = content_width + margin * 2.0;
        let logical_height = layout.height + margin * 2.0;
        let mut surface = create_surface(
            logical_width,
            logical_height,
            scale,
            self.palette().background,
        )?;
        self.draw_document(
            &mut surface.surface_mut(),
            &layout,
            margin,
            margin,
            logical_height,
            PaintOptions {
                scale,
                interaction: InteractionState::default(),
            },
            None,
        );
        Ok(surface)
    }

    fn content_width(&self, viewport_width: f32) -> f32 {
        let margin = self.settings.theme.page_margin;
        (viewport_width - margin * 2.0).clamp(
            self.settings.theme.minimum_content_width,
            self.settings.max_width * POINTS_TO_LOGICAL_PIXELS,
        )
    }

    fn layout_options(&self, width: f32) -> LayoutOptions {
        LayoutOptions {
            width,
            body_size: self.settings.font_size * POINTS_TO_LOGICAL_PIXELS,
            line_spacing: self.settings.theme.line_spacing,
            block_spacing: self.settings.theme.block_spacing,
            indent: self.settings.theme.list_indent,
            heading_scales: self.settings.theme.heading_scales,
            heading_tracking: self.settings.theme.heading_tracking,
            inline_code_scale: self.settings.theme.inline_code_scale,
            code_block_scale: self.settings.theme.code_block_scale,
            superscript_scale: self.settings.theme.superscript_scale,
            superscript_shift: self.settings.theme.superscript_shift,
            table_horizontal_padding: self.settings.theme.table_horizontal_padding,
            table_vertical_padding: self.settings.theme.table_vertical_padding,
            table_min_column_width: self.settings.theme.table_min_column_width,
            table_overflow: if self.interactive_overflow {
                TableOverflowPolicy::Overflow
            } else {
                TableOverflowPolicy::EmergencyFit
            },
            minimum_line_width: self.settings.theme.minimum_line_width,
            space_stretch: self.settings.theme.space_stretch,
            space_shrink: self.settings.theme.space_shrink,
            hyphenation: self.settings.theme.hyphenation,
            hyphen_penalty: self.settings.theme.hyphen_penalty,
            code_block_indent: self.settings.theme.code_block_indent,
            nested_code_block_indent: self.settings.theme.nested_code_block_indent,
            quote_bar_offset: self.settings.theme.quote_bar_offset,
            quote_italic: self.settings.theme.quote_italic,
            table_header_bold: self.settings.theme.table_header_bold,
        }
    }

    pub(crate) fn prepare_flow_layout(
        &self,
        document: &FlowDocument,
        width: f32,
    ) -> ContinuousLayout {
        let composed = self.compose_flow_document(document, width);
        self.place_composed_document(composed)
    }

    pub(crate) fn compose_flow_document(
        &self,
        document: &FlowDocument,
        width: f32,
    ) -> ComposedDocument {
        let _span = crate::telemetry::phase_span!(
            "flow.layout.compose",
            canonical_bytes = document.text().len().as_usize() as u64,
            content_width = width as f64,
        );
        let mut fonts = self.fonts.borrow_mut();
        let measurer = RenderMeasurer {
            fonts: RefCell::new(&mut *fonts),
            images: &self.images,
            base: document.resource_base(),
        };
        let styles = document.resolve_styles(StyleContextKey::new(
            self.settings.theme.quote_italic,
            self.settings.theme.table_header_bold,
        ));
        let composed = layout::compose_flow_document(
            document,
            &styles,
            &measurer,
            &self.layout_options(width),
        );
        crate::telemetry::phase_event!(
            "flow.phase.complete",
            blocks = composed.blocks.len() as u64,
            lines = composed
                .blocks
                .iter()
                .map(|block| block.layout.lines.len() as u64)
                .sum::<u64>(),
        );
        composed
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn draw_repeated_table_header<T: PaintTarget>(
        &self,
        surface: &mut T,
        document: &ContinuousLayout,
        block_index: usize,
        x_offset: f32,
        viewport_height: f32,
        options: PaintOptions<'_>,
        hovered: Option<&NavigationTarget>,
    ) {
        let Some(source_block) = document.blocks.get(block_index) else {
            return;
        };
        let Some(source_table) = source_block.table.as_ref() else {
            return;
        };
        if source_table.header_row_count == 0 {
            return;
        }
        let header_height: f32 = source_table
            .row_heights
            .iter()
            .take(source_table.header_row_count)
            .sum();
        let header_end = source_table.row_y + header_height;
        let mut block = source_block.clone();
        block.y = source_table.row_y;
        block.height = header_height;
        // A table nested in a list may carry its list marker as another line at
        // the header's y coordinate. Repeat only cell content, not that marker.
        block.lines.retain(|line| {
            line.y < header_end
                && line.y + line.height > source_table.row_y
                && line.runs.iter().any(|run| run.x >= source_table.x - 0.01)
        });
        block.atomic_objects.retain(|object| {
            object.y < header_end && object.y + object.height > source_table.row_y
        });
        if let Some(table) = block.table.as_mut() {
            table.rules.retain(|rule| rule.y <= header_end + 0.01);
            table.row_heights.truncate(table.header_row_count);
        }
        let header = ContinuousLayout::new(document.width, header_end, vec![block]);
        self.draw_document(
            surface,
            &header,
            x_offset,
            -source_table.row_y,
            viewport_height,
            options,
            hovered,
        );
    }

    fn draw_paginated_page<T: PaintTarget>(
        &self,
        surface: &mut T,
        document: &ComposedDocument,
        layout: &PaginatedLayout,
        page_index: usize,
        x_offset: f32,
        options: PaintOptions<'_>,
    ) {
        let Some(page) = layout.pages.get(page_index) else {
            return;
        };
        let fragments = &layout.fragments[page.fragments.as_range()];
        let PaintOptions { scale, interaction } = options;
        let palette = self.palette();
        let mut fonts = self.fonts.borrow_mut();

        for fragment in fragments {
            let PageFragment::QuoteBar(fragment) = fragment else {
                continue;
            };
            if self.settings.theme.quote_bar_width > 0.0 {
                surface.fill_rect(
                    (x_offset + fragment.x) * scale,
                    fragment.y * scale,
                    self.settings.theme.quote_bar_width * scale,
                    fragment.height * scale,
                    palette.quote_bar,
                );
            }
        }

        let placements = layout.horizontal_overflows(document, page_index);
        for fragment in fragments {
            let owner = crate::overflow::fragment_block(fragment)
                .and_then(|i| document.blocks.get(i))
                .and_then(|b| b.layout.node);
            let placement = placements.iter().find(|p| Some(p.owner) == owner).copied();
            crate::overflow::paint_content(
                surface,
                placement,
                interaction.overflow,
                x_offset,
                0.0,
                scale,
                |surface, x_offset| match fragment {
                    PageFragment::BlockBox(fragment) => {
                        let Some(block) = document.blocks.get(fragment.block) else {
                            return;
                        };
                        self.draw_page_block_box(surface, &block.layout, fragment, x_offset, scale);
                    }
                    PageFragment::Atomic(fragment) => {
                        let Some(block) = document.blocks.get(fragment.block) else {
                            return;
                        };
                        let Some(object) = block.layout.atomic_objects.get(fragment.atomic) else {
                            return;
                        };
                        self.draw_page_atomic(
                            surface,
                            &mut fonts,
                            &block.layout,
                            object,
                            fragment,
                            x_offset,
                            scale,
                            interaction,
                            palette,
                        );
                    }
                    PageFragment::Rule(fragment) => {
                        self.draw_page_rule(surface, fragment, x_offset, scale, palette);
                    }
                    PageFragment::TableRule(fragment) => {
                        let (thickness, color) = match fragment.kind {
                            layout::TableRuleKind::Strong => (
                                self.settings.theme.table_strong_rule_thickness,
                                palette.table_rule_strong,
                            ),
                            layout::TableRuleKind::Light => (
                                self.settings.theme.table_light_rule_thickness,
                                palette.table_rule_light,
                            ),
                        };
                        surface.fill_rect(
                            (x_offset + fragment.x) * scale,
                            fragment.y * scale,
                            fragment.width * scale,
                            (thickness * scale).max(1.0),
                            color,
                        );
                    }
                    PageFragment::TableQuoteBar(fragment) => {
                        if self.settings.theme.quote_bar_width > 0.0 {
                            surface.fill_rect(
                                (x_offset + fragment.x) * scale,
                                fragment.y * scale,
                                self.settings.theme.quote_bar_width * scale,
                                fragment.height * scale,
                                palette.quote_bar,
                            );
                        }
                    }
                    PageFragment::Lines(fragment) => {
                        let Some(block) = document.blocks.get(fragment.block) else {
                            return;
                        };
                        let Some(first) = block.layout.lines.get(fragment.line_start) else {
                            return;
                        };
                        let y_offset = fragment.y - first.y;
                        for line in &block.layout.lines[fragment.line_start..fragment.line_end] {
                            self.draw_page_line(
                                surface,
                                &mut fonts,
                                line,
                                x_offset,
                                y_offset,
                                scale,
                                interaction,
                                palette,
                            );
                        }
                    }
                    PageFragment::QuoteBar(_) | PageFragment::TableSlice(_) => {}
                },
            );
        }
        if let Some(state) = interaction.overflow {
            for placement in &placements {
                crate::overflow::paint_affordance(
                    surface,
                    *placement,
                    state,
                    (x_offset, 0.0),
                    scale,
                    palette.muted,
                );
            }
        }

        if let Some(target) = interaction.semantic_focus() {
            let mut previous_block = None;
            for fragment in fragments {
                let (block_index, block_y) = match fragment {
                    PageFragment::Lines(fragment) => {
                        let Some(block) = document.blocks.get(fragment.block) else {
                            continue;
                        };
                        let Some(line) = block.layout.lines.get(fragment.line_start) else {
                            continue;
                        };
                        (fragment.block, fragment.y - line.y)
                    }
                    PageFragment::Atomic(fragment) => {
                        let Some(block) = document.blocks.get(fragment.block) else {
                            continue;
                        };
                        let Some(object) = block.layout.atomic_objects.get(fragment.atomic) else {
                            continue;
                        };
                        (fragment.block, fragment.y - object.y)
                    }
                    PageFragment::Rule(fragment) => {
                        let Some(block) = document.blocks.get(fragment.block) else {
                            continue;
                        };
                        (fragment.block, fragment.y - block.layout.height * 0.5)
                    }
                    PageFragment::BlockBox(_)
                    | PageFragment::QuoteBar(_)
                    | PageFragment::TableSlice(_)
                    | PageFragment::TableRule(_)
                    | PageFragment::TableQuoteBar(_) => continue,
                };
                if previous_block == Some(block_index) {
                    continue;
                }
                previous_block = Some(block_index);
                if let Some(block) = document.blocks.get(block_index) {
                    let placement = placements
                        .iter()
                        .find(|p| Some(p.owner) == block.layout.node)
                        .copied();
                    crate::overflow::paint_content(
                        surface,
                        placement,
                        interaction.overflow,
                        x_offset,
                        0.0,
                        scale,
                        |surface, x_offset| {
                            draw_target_focus(
                                surface,
                                &block.layout,
                                target,
                                x_offset,
                                block_y,
                                scale,
                                palette,
                            );
                        },
                    );
                }
            }
        }
    }

    fn draw_page_block_box<T: PaintTarget>(
        &self,
        surface: &mut T,
        block: &layout::BlockLayout,
        fragment: &BlockBoxFragment,
        x_offset: f32,
        scale: f32,
    ) {
        let Some(box_layout) = &block.box_layout else {
            return;
        };
        let palette = self.palette();
        let x = (x_offset + fragment.x) * scale;
        let y = fragment.y * scale;
        let width = fragment.width * scale;
        let height = fragment.height * scale;
        let [top, right, bottom, left] = box_layout.borders;
        if fragment.draw_top && top.width > 0.0 {
            surface.fill_rect(
                x,
                y,
                width,
                (top.width * scale).max(1.0),
                top.color.unwrap_or(palette.foreground),
            );
        }
        if right.width > 0.0 {
            surface.fill_rect(
                x + width - (right.width * scale).max(1.0),
                y,
                (right.width * scale).max(1.0),
                height,
                right.color.unwrap_or(palette.foreground),
            );
        }
        if fragment.draw_bottom && bottom.width > 0.0 {
            surface.fill_rect(
                x,
                y + height - (bottom.width * scale).max(1.0),
                width,
                (bottom.width * scale).max(1.0),
                bottom.color.unwrap_or(palette.foreground),
            );
        }
        if left.width > 0.0 {
            surface.fill_rect(
                x,
                y,
                (left.width * scale).max(1.0),
                height,
                left.color.unwrap_or(palette.foreground),
            );
        }
    }

    fn draw_page_rule<T: PaintTarget>(
        &self,
        surface: &mut T,
        fragment: &RuleFragment,
        x_offset: f32,
        scale: f32,
        palette: Palette,
    ) {
        surface.fill_rect(
            (x_offset + fragment.x) * scale,
            fragment.y * scale,
            fragment.width * scale,
            (self.settings.theme.rule_thickness * scale).max(1.0),
            palette.horizontal_rule,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_page_atomic<T: PaintTarget>(
        &self,
        surface: &mut T,
        fonts: &mut FontSystem,
        block: &layout::BlockLayout,
        object: &layout::AtomicLayout,
        fragment: &AtomicFragment,
        x_offset: f32,
        scale: f32,
        interaction: InteractionState<'_>,
        palette: Palette,
    ) {
        let thematic = matches!(object.kind, layout::AtomicLayoutKind::ThematicBreak);
        let mut painted = thematic;
        if thematic && !block.rule {
            surface.fill_rect(
                (x_offset + fragment.x) * scale,
                (fragment.y + fragment.height / 2.0) * scale,
                fragment.width * scale,
                (self.settings.theme.rule_thickness * scale).max(1.0),
                palette.horizontal_rule,
            );
        }
        if let layout::AtomicLayoutKind::Image { key, .. } = &object.kind {
            let width = (fragment.width * scale).round().max(1.0) as u32;
            let height = (fragment.height * scale).round().max(1.0) as u32;
            if let Some(pixels) = self.images.borrow_mut().scaled_pixels(key, width, height) {
                surface.blend_pixels_resource(
                    image_paint_resource_id(pixels.resource_id(), width, height),
                    (
                        ((x_offset + fragment.x) * scale).round() as i32,
                        (fragment.y * scale).round() as i32,
                    ),
                    width,
                    height,
                    pixels.stride(),
                    pixels.format(),
                    pixels.pixels(),
                );
                painted = true;
            }
        }
        if !painted {
            let thickness = scale.max(1.0);
            let x = (x_offset + fragment.x) * scale;
            let y = fragment.y * scale;
            let width = fragment.width * scale;
            let height = fragment.height * scale;
            surface.fill_rect(x, y, width, thickness, palette.muted);
            surface.fill_rect(x, y + height - thickness, width, thickness, palette.muted);
            surface.fill_rect(x, y, thickness, height, palette.muted);
            surface.fill_rect(x + width - thickness, y, thickness, height, palette.muted);
            let label = match &object.kind {
                layout::AtomicLayoutKind::Image { alt, .. } => alt.as_str(),
                layout::AtomicLayoutKind::Placeholder { label } => label.as_str(),
                layout::AtomicLayoutKind::ThematicBreak => "",
            };
            if !label.is_empty() {
                let size = (self.settings.font_size * POINTS_TO_LOGICAL_PIXELS)
                    .min((fragment.height * 0.5).max(1.0));
                let style = TextStyle {
                    family: FontFamily::Proportional,
                    size,
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
                };
                let clip_x = x.floor() as i32;
                let clip_y = y.floor() as i32;
                let clip_width = width.ceil().max(1.0) as u32;
                let clip_height = height.ceil().max(1.0) as u32;
                let local_x = (x - clip_x as f32) / scale + size * 0.5;
                let local_y = (y - clip_y as f32) / scale + fragment.height * 0.5 + size * 0.35;
                surface.with_viewport((clip_x, clip_y), clip_width, clip_height, |surface| {
                    fonts.draw(
                        surface,
                        label,
                        &style,
                        (local_x, local_y),
                        scale,
                        palette.muted,
                    );
                });
            }
        }
        let search_color = interaction
            .search_matches
            .iter()
            .find(|range| object.range.start < range.end && range.start < object.range.end)
            .map(|_| {
                if interaction.active_match.is_some_and(|range| {
                    object.range.start < range.end && range.start < object.range.end
                }) {
                    palette.border
                } else {
                    palette.key_background
                }
            });
        let selection_color = interaction.selection.and_then(|selection| {
            let range = selection.range();
            (range.start < object.range.end && object.range.start < range.end)
                .then(|| mix_color(palette.background, palette.link, 0.45))
        });
        let hover_color = interaction
            .hovered_target
            .is_some_and(|target| {
                target.flow_owns_node(object.node)
                    && target.flow_destination() == object.flow_link.as_ref()
            })
            .then_some(palette.link);
        if let Some(color) = selection_color.or(search_color).or(hover_color) {
            let thickness = (2.0 * scale).max(1.0);
            let x = (x_offset + fragment.x) * scale;
            let y = fragment.y * scale;
            let width = fragment.width * scale;
            let height = fragment.height * scale;
            surface.fill_rect(x, y, width, thickness, color);
            surface.fill_rect(x, y + height - thickness, width, thickness, color);
            surface.fill_rect(x, y, thickness, height, color);
            surface.fill_rect(x + width - thickness, y, thickness, height, color);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_page_line<T: PaintTarget>(
        &self,
        surface: &mut T,
        fonts: &mut FontSystem,
        line: &layout::LineLayout,
        x_offset: f32,
        y_offset: f32,
        scale: f32,
        interaction: InteractionState<'_>,
        palette: Palette,
    ) {
        for run in &line.runs {
            if interaction
                .search_matches
                .iter()
                .any(|source| run.range.start < source.end && source.start < run.range.end)
            {
                let color = if interaction.active_match.is_some_and(|source| {
                    run.range.start < source.end && source.start < run.range.end
                }) {
                    palette.border
                } else {
                    palette.key_background
                };
                surface.fill_rect(
                    (x_offset + run.x - 1.0) * scale,
                    (y_offset + line.y) * scale,
                    (run.width + 2.0) * scale,
                    line.height * scale,
                    color,
                );
            }
        }
        if let Some(selection) = interaction.selection {
            let selected = selection.range();
            if selected.start < selected.end {
                let selection_color = mix_color(palette.background, palette.link, 0.28);
                for run in &line.runs {
                    let start = run.range.start.max(selected.start);
                    let end = run.range.end.min(selected.end);
                    if start >= end || task_marker(&run.text).is_some() {
                        continue;
                    }
                    let start_x = x_for_source(fonts, run, start);
                    let end_x = x_for_source(fonts, run, end);
                    surface.fill_rect(
                        (x_offset + start_x.min(end_x)) * scale,
                        (y_offset + line.y) * scale,
                        (end_x - start_x).abs().max(1.0) * scale,
                        line.height * scale,
                        selection_color,
                    );
                }
                for runs in line.runs.windows(2) {
                    let [left, right] = runs else {
                        continue;
                    };
                    let gap_start = left.x + left.width;
                    if gap_start < right.x
                        && selected.start < right.range.start
                        && left.range.end < selected.end
                    {
                        surface.fill_rect(
                            (x_offset + gap_start) * scale,
                            (y_offset + line.y) * scale,
                            (right.x - gap_start) * scale,
                            line.height * scale,
                            selection_color,
                        );
                    }
                }
            }
        }
        for (run_index, run) in line.runs.iter().enumerate() {
            let color = if run.style.link || run.generated_action.is_some() {
                palette.link
            } else {
                syntax_color(run.syntax, palette)
            };
            let baseline = y_offset + line.y + line.baseline - run.style.baseline_shift;
            if let Some(checked) = task_marker(&run.text) {
                draw_task_checkbox(
                    surface,
                    (x_offset + run.x, baseline),
                    run.style.size,
                    scale,
                    checked,
                    palette,
                    &self.settings.theme,
                );
                continue;
            }
            fonts.draw(
                surface,
                &run.text,
                &run.style,
                (x_offset + run.x, baseline),
                scale,
                color,
            );
            if run.style.strikethrough {
                surface.fill_rect(
                    (x_offset + run.x) * scale,
                    (baseline - run.style.size * self.settings.theme.strikethrough_offset) * scale,
                    run.width * scale,
                    (self.settings.theme.decoration_thickness * scale).max(1.0),
                    color,
                );
            }
            let hovered_generated = interaction.hovered_target.is_some_and(|target| {
                run.generated_action.is_some()
                    && target.generated_action() == run.generated_action.as_ref()
            });
            let hovered_flow_link = interaction.hovered_target.is_some_and(|target| {
                run.range.start < target.range.end
                    && target.range.start < run.range.end
                    && target.flow_destination() == run.flow_link.as_ref()
            });
            if hovered_flow_link || hovered_generated {
                let underline_width = line
                    .runs
                    .get(run_index + 1)
                    .filter(|next| {
                        hovered_flow_link && next.flow_link == run.flow_link
                            || hovered_generated && next.generated_action == run.generated_action
                    })
                    .map_or(run.width, |next| next.x - run.x);
                surface.fill_rect(
                    (x_offset + run.x) * scale,
                    (baseline + run.style.size * self.settings.theme.underline_offset) * scale,
                    underline_width * scale,
                    (self.settings.theme.decoration_thickness * scale).max(1.0),
                    color,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_document<T: PaintTarget>(
        &self,
        surface: &mut T,
        document: &ContinuousLayout,
        x_offset: f32,
        y_offset: f32,
        viewport_height: f32,
        options: PaintOptions<'_>,
        hovered_flow: Option<&NavigationTarget>,
    ) {
        let PaintOptions { scale, interaction } = options;
        let palette = self.palette();
        let mut fonts = self.fonts.borrow_mut();
        for bar in &document.quote_bars {
            if self.settings.theme.quote_bar_width == 0.0
                || !vertically_visible(y_offset + bar.y, bar.height, viewport_height)
            {
                continue;
            }
            surface.fill_rect(
                (x_offset + bar.x) * scale,
                (y_offset + bar.y) * scale,
                self.settings.theme.quote_bar_width * scale,
                bar.height * scale,
                palette.quote_bar,
            );
        }
        for block in &document.blocks {
            if !vertically_visible(y_offset + block.y, block.height, viewport_height) {
                continue;
            }
            let placement = crate::overflow::block_placement(block, document.width);
            crate::overflow::paint_content(
                surface,
                placement,
                interaction.overflow,
                x_offset,
                y_offset,
                scale,
                |surface, x_offset| {
                    if let Some(box_layout) = &block.box_layout {
                        let x = (x_offset + box_layout.x) * scale;
                        let y = (y_offset + block.y) * scale;
                        let width = box_layout.width * scale;
                        let height = block.height * scale;
                        let [top, right, bottom, left] = box_layout.borders;
                        if top.width > 0.0 {
                            surface.fill_rect(
                                x,
                                y,
                                width,
                                (top.width * scale).max(1.0),
                                top.color.unwrap_or(palette.foreground),
                            );
                        }
                        if right.width > 0.0 {
                            surface.fill_rect(
                                x + width - (right.width * scale).max(1.0),
                                y,
                                (right.width * scale).max(1.0),
                                height,
                                right.color.unwrap_or(palette.foreground),
                            );
                        }
                        if bottom.width > 0.0 {
                            surface.fill_rect(
                                x,
                                y + height - (bottom.width * scale).max(1.0),
                                width,
                                (bottom.width * scale).max(1.0),
                                bottom.color.unwrap_or(palette.foreground),
                            );
                        }
                        if left.width > 0.0 {
                            surface.fill_rect(
                                x,
                                y,
                                (left.width * scale).max(1.0),
                                height,
                                left.color.unwrap_or(palette.foreground),
                            );
                        }
                    }
                    for object in &block.atomic_objects {
                        if !vertically_visible(y_offset + object.y, object.height, viewport_height)
                        {
                            continue;
                        }
                        let thematic =
                            matches!(object.kind, layout::AtomicLayoutKind::ThematicBreak);
                        let mut painted = thematic;
                        if thematic && !block.rule {
                            surface.fill_rect(
                                (x_offset + object.x) * scale,
                                (y_offset + object.y + object.height / 2.0) * scale,
                                object.width * scale,
                                (self.settings.theme.rule_thickness * scale).max(1.0),
                                palette.horizontal_rule,
                            );
                        }
                        if let layout::AtomicLayoutKind::Image { key, .. } = &object.kind {
                            let width = (object.width * scale).round().max(1.0) as u32;
                            let height = (object.height * scale).round().max(1.0) as u32;
                            if let Some(pixels) =
                                self.images.borrow_mut().scaled_pixels(key, width, height)
                            {
                                surface.blend_pixels_resource(
                                    image_paint_resource_id(pixels.resource_id(), width, height),
                                    (
                                        ((x_offset + object.x) * scale).round() as i32,
                                        ((y_offset + object.y) * scale).round() as i32,
                                    ),
                                    width,
                                    height,
                                    pixels.stride(),
                                    pixels.format(),
                                    pixels.pixels(),
                                );
                                painted = true;
                            }
                        }
                        if !painted {
                            let thickness = scale.max(1.0);
                            let x = (x_offset + object.x) * scale;
                            let y = (y_offset + object.y) * scale;
                            let width = object.width * scale;
                            let height = object.height * scale;
                            surface.fill_rect(x, y, width, thickness, palette.muted);
                            surface.fill_rect(
                                x,
                                y + height - thickness,
                                width,
                                thickness,
                                palette.muted,
                            );
                            surface.fill_rect(x, y, thickness, height, palette.muted);
                            surface.fill_rect(
                                x + width - thickness,
                                y,
                                thickness,
                                height,
                                palette.muted,
                            );
                            let label = match &object.kind {
                                layout::AtomicLayoutKind::Image { alt, .. } => alt.as_str(),
                                layout::AtomicLayoutKind::Placeholder { label } => label.as_str(),
                                layout::AtomicLayoutKind::ThematicBreak => "",
                            };
                            if !label.is_empty() {
                                let size = (self.settings.font_size * POINTS_TO_LOGICAL_PIXELS)
                                    .min((object.height * 0.5).max(1.0));
                                let style = TextStyle {
                                    family: FontFamily::Proportional,
                                    size,
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
                                };
                                let clip_x = x.floor() as i32;
                                let clip_y = y.floor() as i32;
                                let clip_width = width.ceil().max(1.0) as u32;
                                let clip_height = height.ceil().max(1.0) as u32;
                                let local_x = (x - clip_x as f32) / scale + size * 0.5;
                                let local_y =
                                    (y - clip_y as f32) / scale + object.height * 0.5 + size * 0.35;
                                surface.with_viewport(
                                    (clip_x, clip_y),
                                    clip_width,
                                    clip_height,
                                    |surface| {
                                        fonts.draw(
                                            surface,
                                            label,
                                            &style,
                                            (local_x, local_y),
                                            scale,
                                            palette.muted,
                                        );
                                    },
                                );
                            }
                        }
                        let search_color = interaction
                            .search_matches
                            .iter()
                            .find(|range| {
                                object.range.start < range.end && range.start < object.range.end
                            })
                            .map(|_| {
                                if interaction.active_match.is_some_and(|range| {
                                    object.range.start < range.end && range.start < object.range.end
                                }) {
                                    palette.border
                                } else {
                                    palette.key_background
                                }
                            });
                        let selection_color = interaction.selection.and_then(|selection| {
                            let range = selection.range();
                            (range.start < object.range.end && object.range.start < range.end)
                                .then(|| mix_color(palette.background, palette.link, 0.45))
                        });
                        let hover_color = hovered_flow
                            .is_some_and(|target| {
                                target.flow_owns_node(object.node)
                                    && target.flow_destination() == object.flow_link.as_ref()
                            })
                            .then_some(palette.link);
                        if let Some(color) = selection_color.or(search_color).or(hover_color) {
                            let thickness = (2.0 * scale).max(1.0);
                            let x = (x_offset + object.x) * scale;
                            let y = (y_offset + object.y) * scale;
                            let width = object.width * scale;
                            let height = object.height * scale;
                            surface.fill_rect(x, y, width, thickness, color);
                            surface.fill_rect(x, y + height - thickness, width, thickness, color);
                            surface.fill_rect(x, y, thickness, height, color);
                            surface.fill_rect(x + width - thickness, y, thickness, height, color);
                        }
                    }
                    if let Some(table) = &block.table {
                        for quote in &table.quote_bars {
                            surface.fill_rect(
                                (x_offset + quote.x) * scale,
                                (y_offset + quote.y) * scale,
                                self.settings.theme.quote_bar_width * scale,
                                quote.height * scale,
                                palette.quote_bar,
                            );
                        }
                        for rule in &table.rules {
                            let (thickness, color) = match rule.kind {
                                layout::TableRuleKind::Strong => (
                                    self.settings.theme.table_strong_rule_thickness,
                                    palette.table_rule_strong,
                                ),
                                layout::TableRuleKind::Light => (
                                    self.settings.theme.table_light_rule_thickness,
                                    palette.table_rule_light,
                                ),
                            };
                            surface.fill_rect(
                                (x_offset + rule.x) * scale,
                                (y_offset + rule.y) * scale,
                                rule.width * scale,
                                (thickness * scale).max(1.0),
                                color,
                            );
                        }
                    }
                    if block.rule {
                        surface.fill_rect(
                            (x_offset + block.rule_x) * scale,
                            (y_offset + block.y + block.height / 2.0) * scale,
                            block.rule_width * scale,
                            (self.settings.theme.rule_thickness * scale).max(1.0),
                            palette.horizontal_rule,
                        );
                    }
                    for line in &block.lines {
                        if !vertically_visible(y_offset + line.y, line.height, viewport_height) {
                            continue;
                        }
                        for run in &line.runs {
                            if interaction.search_matches.iter().any(|source| {
                                run.range.start < source.end && source.start < run.range.end
                            }) {
                                let color = if interaction.active_match.is_some_and(|source| {
                                    run.range.start < source.end && source.start < run.range.end
                                }) {
                                    palette.border
                                } else {
                                    palette.key_background
                                };
                                surface.fill_rect(
                                    (x_offset + run.x - 1.0) * scale,
                                    (y_offset + line.y) * scale,
                                    (run.width + 2.0) * scale,
                                    line.height * scale,
                                    color,
                                );
                            }
                        }
                        if let Some(selection) = interaction.selection {
                            let selected = selection.range();
                            if selected.start < selected.end {
                                let selection_color =
                                    mix_color(palette.background, palette.link, 0.28);
                                for run in &line.runs {
                                    let start = run.range.start.max(selected.start);
                                    let end = run.range.end.min(selected.end);
                                    if start >= end || task_marker(&run.text).is_some() {
                                        continue;
                                    }
                                    let start_x = x_for_source(&mut fonts, run, start);
                                    let end_x = x_for_source(&mut fonts, run, end);
                                    surface.fill_rect(
                                        (x_offset + start_x.min(end_x)) * scale,
                                        (y_offset + line.y) * scale,
                                        (end_x - start_x).abs().max(1.0) * scale,
                                        line.height * scale,
                                        selection_color,
                                    );
                                }
                                for runs in line.runs.windows(2) {
                                    let [left, right] = runs else {
                                        continue;
                                    };
                                    let gap_start = left.x + left.width;
                                    if gap_start < right.x
                                        && selected.start < right.range.start
                                        && left.range.end < selected.end
                                    {
                                        surface.fill_rect(
                                            (x_offset + gap_start) * scale,
                                            (y_offset + line.y) * scale,
                                            (right.x - gap_start) * scale,
                                            line.height * scale,
                                            selection_color,
                                        );
                                    }
                                }
                            }
                        }
                        for (run_index, run) in line.runs.iter().enumerate() {
                            let color = if run.style.link || run.generated_action.is_some() {
                                palette.link
                            } else {
                                syntax_color(run.syntax, palette)
                            };
                            let baseline =
                                y_offset + line.y + line.baseline - run.style.baseline_shift;
                            if let Some(checked) = task_marker(&run.text) {
                                draw_task_checkbox(
                                    surface,
                                    (x_offset + run.x, baseline),
                                    run.style.size,
                                    scale,
                                    checked,
                                    palette,
                                    &self.settings.theme,
                                );
                                continue;
                            }
                            fonts.draw(
                                surface,
                                &run.text,
                                &run.style,
                                (x_offset + run.x, baseline),
                                scale,
                                color,
                            );
                            if run.style.strikethrough {
                                surface.fill_rect(
                                    (x_offset + run.x) * scale,
                                    (baseline
                                        - run.style.size
                                            * self.settings.theme.strikethrough_offset)
                                        * scale,
                                    run.width * scale,
                                    (self.settings.theme.decoration_thickness * scale).max(1.0),
                                    color,
                                );
                            }
                            let hovered_generated = hovered_flow.is_some_and(|target| {
                                run.generated_action.is_some()
                                    && target.generated_action() == run.generated_action.as_ref()
                            });
                            let hovered_flow_link = hovered_flow.is_some_and(|target| {
                                run.range.start < target.range.end
                                    && target.range.start < run.range.end
                                    && target.flow_destination() == run.flow_link.as_ref()
                            });
                            if hovered_flow_link || hovered_generated {
                                let underline_width = line
                                    .runs
                                    .get(run_index + 1)
                                    .filter(|next| {
                                        hovered_flow_link && next.flow_link == run.flow_link
                                            || hovered_generated
                                                && next.generated_action == run.generated_action
                                    })
                                    .map_or(run.width, |next| next.x - run.x);
                                surface.fill_rect(
                                    (x_offset + run.x) * scale,
                                    (baseline
                                        + run.style.size * self.settings.theme.underline_offset)
                                        * scale,
                                    underline_width * scale,
                                    (self.settings.theme.decoration_thickness * scale).max(1.0),
                                    color,
                                );
                            }
                        }
                    }
                    if let Some(target) = interaction.semantic_focus() {
                        draw_target_focus(
                            surface, block, target, x_offset, y_offset, scale, palette,
                        );
                    }
                },
            );
            if let (Some(placement), Some(state)) = (placement, interaction.overflow) {
                crate::overflow::paint_affordance(
                    surface,
                    placement,
                    state,
                    (x_offset, y_offset),
                    scale,
                    palette.muted,
                );
            }
        }
    }
}

fn ranges_interact(left: TextRange, right: TextRange) -> bool {
    left.start < right.end && right.start < left.end
        || left.start == left.end && right.start <= left.start && left.start <= right.end
        || right.start == right.end && left.start <= right.start && right.start <= left.end
}

fn vertically_visible(y: f32, height: f32, viewport_height: f32) -> bool {
    y + height > 0.0 && y < viewport_height
}

fn syntax_color(kind: SyntaxKind, palette: Palette) -> [u8; 3] {
    let luminance = u32::from(palette.background[0]) * 299
        + u32::from(palette.background[1]) * 587
        + u32::from(palette.background[2]) * 114;
    let dark = luminance < 128_000;
    match (dark, kind) {
        (_, SyntaxKind::Plain | SyntaxKind::Operator) => palette.foreground,
        (_, SyntaxKind::Comment) => palette.muted,
        (false, SyntaxKind::String) => [3, 102, 53],
        (false, SyntaxKind::Number | SyntaxKind::Constant) => [0, 92, 197],
        (false, SyntaxKind::Keyword) => [207, 34, 46],
        (false, SyntaxKind::Type | SyntaxKind::Function) => [111, 66, 193],
        (false, SyntaxKind::Attribute) => [149, 56, 0],
        (true, SyntaxKind::String) => [165, 214, 255],
        (true, SyntaxKind::Number | SyntaxKind::Constant) => [121, 192, 255],
        (true, SyntaxKind::Keyword) => [255, 123, 114],
        (true, SyntaxKind::Type | SyntaxKind::Function) => [210, 168, 255],
        (true, SyntaxKind::Attribute) => [255, 166, 87],
    }
}

fn mix_color(left: [u8; 3], right: [u8; 3], amount: f32) -> [u8; 3] {
    std::array::from_fn(|index| {
        (left[index] as f32 * (1.0 - amount) + right[index] as f32 * amount).round() as u8
    })
}

fn distance_to_interval(value: f32, start: f32, end: f32) -> f32 {
    if value < start {
        start - value
    } else if value > end {
        value - end
    } else {
        0.0
    }
}

fn distance_to_line_runs(x: f32, line: &layout::LineLayout) -> f32 {
    line.runs
        .iter()
        .map(|run| distance_to_interval(x, run.x, run.x + run.width))
        .min_by(f32::total_cmp)
        .unwrap_or(f32::INFINITY)
}

fn position_in_run(fonts: &mut FontSystem, run: &layout::RunLayout, x: f32) -> TextPosition {
    if run.range.end.saturating_sub(run.range.start) != run.text.len() {
        return TextPosition::new(if x < run.x + run.width / 2.0 {
            run.range.start
        } else {
            run.range.end
        });
    }
    let clusters = fonts.clusters(&run.text, &run.style);
    let rtl = clusters
        .first()
        .zip(clusters.last())
        .is_some_and(|(first, last)| first.start > last.start);
    if x <= run.x {
        return if rtl {
            TextPosition {
                offset: run.range.end,
                affinity: crate::model::Affinity::Upstream,
            }
        } else {
            TextPosition {
                offset: run.range.start,
                affinity: crate::model::Affinity::Downstream,
            }
        };
    }
    if x >= run.x + run.width {
        return if rtl {
            TextPosition {
                offset: run.range.start,
                affinity: crate::model::Affinity::Downstream,
            }
        } else {
            TextPosition {
                offset: run.range.end,
                affinity: crate::model::Affinity::Upstream,
            }
        };
    }
    let measured_width = clusters
        .iter()
        .map(|cluster| cluster.x + cluster.width)
        .fold(0.0_f32, f32::max)
        .max(f32::EPSILON);
    let scale = run.width / measured_width;
    for cluster in clusters {
        let start_x = run.x + cluster.x * scale;
        let end_x = start_x + cluster.width * scale;
        if x < end_x {
            let before_midpoint = x < (start_x + end_x) / 2.0;
            let offset = if before_midpoint == rtl {
                cluster.end
            } else {
                cluster.start
            };
            return TextPosition {
                offset: run.range.start + offset,
                affinity: if before_midpoint == rtl {
                    crate::model::Affinity::Upstream
                } else {
                    crate::model::Affinity::Downstream
                },
            };
        }
    }
    if rtl {
        TextPosition {
            offset: run.range.start,
            affinity: crate::model::Affinity::Downstream,
        }
    } else {
        TextPosition {
            offset: run.range.end,
            affinity: crate::model::Affinity::Upstream,
        }
    }
}

fn x_for_source(fonts: &mut FontSystem, run: &layout::RunLayout, offset: usize) -> f32 {
    if run.range.end.saturating_sub(run.range.start) != run.text.len() {
        return run.x;
    }
    let clusters = fonts.clusters(&run.text, &run.style);
    let rtl = clusters
        .first()
        .zip(clusters.last())
        .is_some_and(|(first, last)| first.start > last.start);
    if offset <= run.range.start {
        return if rtl { run.x + run.width } else { run.x };
    }
    if offset >= run.range.end {
        return if rtl { run.x } else { run.x + run.width };
    }
    let relative = offset - run.range.start;
    let measured_width = clusters
        .iter()
        .map(|cluster| cluster.x + cluster.width)
        .fold(0.0_f32, f32::max)
        .max(f32::EPSILON);
    let scale = run.width / measured_width;
    let cluster = clusters
        .iter()
        .find(|cluster| cluster.start <= relative && relative <= cluster.end)
        .or_else(|| {
            clusters
                .iter()
                .min_by_key(|cluster| cluster.start.abs_diff(relative))
        });
    cluster.map_or(run.x, |cluster| {
        let edge = if relative <= cluster.start {
            if rtl {
                cluster.x + cluster.width
            } else {
                cluster.x
            }
        } else if rtl {
            cluster.x
        } else {
            cluster.x + cluster.width
        };
        run.x + edge * scale
    })
}

fn draw_target_focus<T: PaintTarget>(
    surface: &mut T,
    block: &layout::BlockLayout,
    target: &NavigationTarget,
    x_offset: f32,
    y_offset: f32,
    scale: f32,
    palette: Palette,
) {
    if !ranges_interact(target.range, block.range) {
        return;
    }

    let mut rectangles = Vec::new();
    if let Some(action) = target.generated_action()
        && let Some(rectangle) = block.lines.iter().find_map(|line| {
            line.runs
                .iter()
                .find(|run| run.generated_action.as_ref() == Some(action))
                .map(|run| (run.x, line.y, run.width, line.height))
        })
    {
        rectangles.push(rectangle);
    }
    let flow_atomic = target.flow_node().and_then(|node| {
        block
            .atomic_objects
            .iter()
            .find(|atomic| atomic.node == Some(node))
    });
    if rectangles.is_empty() {
        if let Some(atomic) = flow_atomic {
            rectangles.push((atomic.x, atomic.y, atomic.width, atomic.height));
        } else {
            match target.kind {
                NavigationKind::Image => {}
                NavigationKind::Table => {
                    if let Some(table) = &block.table {
                        rectangles.push((table.x, block.y, table.width, block.height));
                    }
                }
                NavigationKind::Task => {
                    for line in &block.lines {
                        if let Some(run) = line
                            .runs
                            .iter()
                            .find(|run| task_marker(&run.text).is_some())
                        {
                            rectangles.push((run.x, line.y, run.width, line.height));
                            break;
                        }
                    }
                }
                _ => {
                    for line in &block.lines {
                        let matching: Vec<_> = line
                            .runs
                            .iter()
                            .filter(|run| {
                                run.range.start < target.range.end
                                    && target.range.start < run.range.end
                                    && target.flow_destination().is_none_or(|destination| {
                                        run.flow_link.as_ref() == Some(destination)
                                    })
                            })
                            .collect();
                        if let (Some(first), Some(last)) = (matching.first(), matching.last()) {
                            rectangles.push((
                                first.x,
                                line.y,
                                last.x + last.width - first.x,
                                line.height,
                            ));
                        }
                    }
                    if let Some(destination) = target.flow_destination() {
                        rectangles.extend(
                            block
                                .atomic_objects
                                .iter()
                                .filter(|atomic| {
                                    target.flow_owns_node(atomic.node)
                                        && atomic.flow_link.as_ref() == Some(destination)
                                })
                                .map(|atomic| (atomic.x, atomic.y, atomic.width, atomic.height)),
                        );
                    }
                }
            }
        }
    }

    let padding = 3.0;
    let thickness = scale.max(1.0);
    for (x, y, width, height) in rectangles {
        let left = (x_offset + x - padding) * scale;
        let top = (y_offset + y - padding) * scale;
        let width = (width + padding * 2.0) * scale;
        let height = (height + padding * 2.0) * scale;
        for (start, end) in [
            ((left, top), (left + width, top)),
            ((left + width, top), (left + width, top + height)),
            ((left + width, top + height), (left, top + height)),
            ((left, top + height), (left, top)),
        ] {
            surface.draw_line(start, end, thickness, palette.link);
        }
    }
}

fn task_marker(text: &str) -> Option<bool> {
    match text {
        "☑" => Some(true),
        "☐" => Some(false),
        _ => None,
    }
}

fn draw_task_checkbox<T: PaintTarget>(
    surface: &mut T,
    origin: (f32, f32),
    font_size: f32,
    scale: f32,
    checked: bool,
    palette: Palette,
    theme: &Theme,
) {
    let size = font_size * theme.task_checkbox_scale;
    let x = origin.0 * scale;
    // Sit the checkbox on the text baseline instead of centering its square
    // around it; the latter makes task markers look visibly too low.
    let y = (origin.1 - size) * scale;
    let size = size * scale;
    let border = (theme.task_checkbox_border * scale).max(1.0);
    surface.fill_rect(x, y, size, size, palette.foreground);
    if checked {
        let check = palette.background;
        surface.draw_line(
            (x + size * 0.22, y + size * 0.53),
            (x + size * 0.43, y + size * 0.74),
            (theme.task_checkbox_check_width * scale).max(1.0),
            check,
        );
        surface.draw_line(
            (x + size * 0.43, y + size * 0.74),
            (x + size * 0.80, y + size * 0.27),
            (theme.task_checkbox_check_width * scale).max(1.0),
            check,
        );
    } else {
        surface.fill_rect(
            x + border,
            y + border,
            size - border * 2.0,
            size - border * 2.0,
            palette.background,
        );
    }
}

struct RenderMeasurer<'a> {
    fonts: RefCell<&'a mut FontSystem>,
    images: &'a RefCell<ImageCache>,
    base: Option<&'a str>,
}

impl TextMeasurer for RenderMeasurer<'_> {
    fn ink_bottom(&self, text: &str, style: &TextStyle) -> f32 {
        self.fonts.borrow_mut().ink_bottom(text, style)
    }

    fn cap_height(&self, style: &TextStyle) -> f32 {
        self.fonts.borrow_mut().cap_height(style)
    }

    fn measure(&self, text: &str, style: &TextStyle) -> TextMetrics {
        self.fonts.borrow_mut().measure(text, style)
    }

    fn measure_image(&self, url: &str) -> Option<ImageMetrics> {
        self.images.borrow_mut().measure(url, self.base)
    }

    fn measure_image_resource(
        &self,
        identity: &str,
        resource_base: Option<&str>,
        intrinsic_size: Option<(f32, f32)>,
    ) -> ImageMetrics {
        // Node-specific dimensions remain authoritative even when another use
        // of this resource (or its decoded bitmap) has a different natural size.
        if let Some((width, height)) = intrinsic_size {
            let mut images = self.images.borrow_mut();
            let request = images.request(identity, resource_base);
            images.hint_dimensions(request.clone(), width.ceil() as u32, height.ceil() as u32);
            return ImageMetrics {
                key: request.key(),
                width,
                height,
            };
        }
        let measured = self.images.borrow_mut().measure(identity, resource_base);
        measured.unwrap_or_else(|| {
            let request = self.images.borrow().request(identity, resource_base);
            let (width, height) = intrinsic_size.unwrap_or((0.0, 0.0));
            ImageMetrics {
                key: request.key(),
                width,
                height,
            }
        })
    }
}

pub(crate) fn create_surface(
    logical_width: f32,
    logical_height: f32,
    scale: f32,
    background: [u8; 3],
) -> Result<PixelBuffer> {
    let (width, height) = render_surface_size(logical_width, logical_height, scale)?;
    PixelBuffer::new(width, height, background)
}

pub(crate) fn prepare_surface<T: PaintTarget>(
    surface: &mut T,
    logical_width: f32,
    logical_height: f32,
    scale: f32,
    background: [u8; 3],
) -> Result<()> {
    let (width, height) = render_surface_size(logical_width, logical_height, scale)?;
    if surface.width() != width || surface.height() != height {
        anyhow::bail!(
            "render surface is {}x{}, expected {width}x{height}",
            surface.width(),
            surface.height()
        );
    }
    surface.clear(background);
    Ok(())
}

pub fn render_surface_size(
    logical_width: f32,
    logical_height: f32,
    scale: f32,
) -> Result<(u32, u32)> {
    Ok((
        physical_dimension(logical_width, scale, "width")?,
        physical_dimension(logical_height, scale, "height")?,
    ))
}

fn physical_dimension(logical: f32, scale: f32, name: &str) -> Result<u32> {
    if !logical.is_finite() || !scale.is_finite() || logical <= 0.0 || scale <= 0.0 {
        anyhow::bail!("invalid render {name} or scale");
    }
    let physical = (logical * scale).ceil();
    if physical > i32::MAX as f32 {
        anyhow::bail!("render {name} is too large");
    }
    Ok(physical as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        formats::markdown::{MarkdownOptions, parse},
        model::flow::{FlowDocument, SourceRetention},
        navigation::NavigationIndex,
        pixels::test_support::{PaintCommand, RecordingPaintTarget},
    };
    use std::path::Path;

    fn region_has_non_background(
        surface: &PixelBuffer,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        background: [u8; 3],
    ) -> bool {
        let x0 = x.floor().max(0.0) as usize;
        let y0 = y.floor().max(0.0) as usize;
        let x1 = (x + width).ceil().min(surface.width() as f32) as usize;
        let y1 = (y + height).ceil().min(surface.height() as f32) as usize;
        let stride = surface.width() as usize * 4;
        (y0..y1).any(|row| {
            surface.pixels()[row * stride + x0 * 4..row * stride + x1 * 4]
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != [background[2], background[1], background[0]])
        })
    }

    #[test]
    fn resource_namespace_identity_distinguishes_empty_from_absent() {
        let mut renderer = Renderer::new(RenderSettings::default());
        let absent = renderer.environment_key();
        renderer.set_resource_namespace(Some(String::new()));
        assert_ne!(renderer.environment_key(), absent);
        renderer.set_resource_namespace(None);
        assert_eq!(renderer.environment_key(), absent);
    }

    fn flow_document(source: &str, resource_base: Option<&str>) -> FlowDocument {
        parse(
            source,
            MarkdownOptions {
                resource_base: resource_base.map(str::to_owned),
                identity: None,
                retention: SourceRetention::Discard,
            },
        )
        .unwrap()
        .document
    }

    #[test]
    fn line_spacing_changes_invalidate_layout_only_when_the_value_changes() {
        let mut renderer = Renderer::new(RenderSettings::default());
        let revision = renderer.layout_revision();
        let current = renderer.settings().theme.line_spacing;
        renderer.set_line_spacing(current);
        assert_eq!(renderer.layout_revision(), revision);

        renderer.set_line_spacing(current + 0.25);
        assert_eq!(renderer.layout_revision(), revision + 1);
        assert_eq!(renderer.settings().theme.line_spacing, current + 0.25);
    }

    fn fragment_layout(
        renderer: &Renderer,
        document: &FlowDocument,
        width: f32,
        viewport_height: u32,
    ) -> (ComposedDocument, ContinuousLayout, PageMap, PaginatedLayout) {
        let composed = renderer.compose_document(document, width);
        let continuous = composed.continuous_layout();
        let pages = renderer.paginate_layout(&continuous, viewport_height);
        let input = crate::pagination::build_page_items(&composed);
        let plan = crate::pagination::break_page_items(&input, pages.body_height).unwrap();
        let paginated =
            crate::pagination::materialize_paginated_layout(&composed, &input, &plan).unwrap();
        (composed, continuous, pages, paginated)
    }

    #[test]
    fn paginated_rendering_uses_page_fragments() {
        let viewer = include_str!("viewer.rs");
        assert!(!viewer.contains("paint_page_into("));
        assert!(!viewer.contains("paginate_layout("));
        assert!(!viewer.contains("paginated: Option<PaginatedLayout>"));

        let source = include_str!("render.rs");
        let render_page = source
            .split_once("    pub fn render_page(")
            .and_then(|(_, rest)| rest.split_once("    pub fn effective_content_width"))
            .map(|(body, _)| body)
            .expect("render_page source boundary remains available");
        assert!(!render_page.contains("paint_page_into("));
        assert!(!render_page.contains("paginate_layout("));
    }

    #[test]
    fn full_document_render_uses_content_height() {
        let document = flow_document("# Title\n\nA paragraph with enough text to render.", None);
        let renderer = Renderer::new(RenderSettings {
            font_size: 14.0,
            max_width: 468.0,
            ..RenderSettings::default()
        });
        let surface = renderer.render_full_document(&document, 1.0).unwrap();
        assert_eq!(surface.width(), 720);
        assert!(surface.height() > 96);
    }

    #[test]
    fn fragment_page_paint_matches_continuous_page_interior_at_fractional_scales() {
        let document = flow_document(
            "# Heading\n\nA paragraph with *emphasis*, a [link](next.md), and enough repeated text to wrap across several lines.\n\n> A quoted paragraph.\n\n---\n\nFinal paragraph.",
            None,
        );
        let renderer = Renderer::new(RenderSettings::default());
        let (composed, mut continuous, pages, paginated) =
            fragment_layout(&renderer, &document, 420.0, 260);
        assert_eq!(pages.pages.len(), paginated.pages.len());
        for scale in [0.85_f32, 1.0, 1.25] {
            for page in 0..pages.pages.len() {
                let mut continuous_pixels = PixelBuffer::new(
                    (500.0 * scale).ceil() as u32,
                    (260.0 * scale).ceil() as u32,
                    [255, 255, 255],
                )
                .unwrap();
                let mut fragments = PixelBuffer::new(
                    (500.0 * scale).ceil() as u32,
                    (260.0 * scale).ceil() as u32,
                    [255, 255, 255],
                )
                .unwrap();
                let selection = PageSelection {
                    requested_page: page,
                    text_anchor: None,
                };
                renderer
                    .paint_page_into(
                        &mut continuous_pixels.surface_mut(),
                        &mut continuous,
                        &pages,
                        (500, 260),
                        selection,
                        PaintOptions {
                            scale,
                            interaction: InteractionState::default(),
                        },
                    )
                    .unwrap();
                renderer
                    .paint_paginated_page_into(
                        &mut fragments.surface_mut(),
                        &composed,
                        &paginated,
                        (500, 260),
                        selection,
                        PaintOptions {
                            scale,
                            interaction: InteractionState::default(),
                        },
                    )
                    .unwrap();
                let inset = (3.0 * scale).ceil() as usize;
                let stride = continuous_pixels.width() as usize * 4;
                let body_start = (renderer.settings.theme.page_margin * scale).round() as usize;
                let body_end = ((renderer.settings.theme.page_margin + pages.body_height) * scale)
                    .round() as usize;
                for row in body_start + inset..body_end.saturating_sub(inset) {
                    let range = row * stride..(row + 1) * stride;
                    let continuous_row = &continuous_pixels.pixels()[range.clone()];
                    let fragment_row = &fragments.pixels()[range];
                    let difference = continuous_row
                        .iter()
                        .zip(fragment_row)
                        .position(|(left, right)| left != right);
                    assert!(
                        difference.is_none(),
                        "page {page} scale {scale} row {row} differs at byte {difference:?} away from the ownership boundary"
                    );
                }
            }
        }
    }

    #[test]
    fn fragment_page_traversal_is_independent_of_document_tail() {
        let settings = RenderSettings::default();

        let renderer = Renderer::new(settings);
        let prefix =
            "# Heading\n\nThe same first-page paragraph is repeated enough to wrap consistently. ";
        let short = flow_document(&prefix.repeat(8), None);
        let long = flow_document(
            &(prefix.repeat(8) + &"\n\nTail paragraph.".repeat(500)),
            None,
        );
        let (short_composed, _, _, short_pages) = fragment_layout(&renderer, &short, 420.0, 600);
        let (long_composed, _, _, long_pages) = fragment_layout(&renderer, &long, 420.0, 600);
        let mut short_target = RecordingPaintTarget::new(500, 600).unwrap();
        let mut long_target = RecordingPaintTarget::new(500, 600).unwrap();
        let selection = PageSelection {
            requested_page: 0,
            text_anchor: None,
        };
        renderer
            .paint_paginated_page_into(
                &mut short_target,
                &short_composed,
                &short_pages,
                (500, 600),
                selection,
                PaintOptions {
                    scale: 1.0,
                    interaction: InteractionState::default(),
                },
            )
            .unwrap();
        renderer
            .paint_paginated_page_into(
                &mut long_target,
                &long_composed,
                &long_pages,
                (500, 600),
                selection,
                PaintOptions {
                    scale: 1.0,
                    interaction: InteractionState::default(),
                },
            )
            .unwrap();
        assert_eq!(short_target.commands, long_target.commands);
        assert_eq!(short_target.buffer.pixels(), long_target.buffer.pixels());
        assert!(long_pages.pages.len() > short_pages.pages.len());
    }

    #[test]
    fn page_painter_uses_the_same_production_path_for_pixels_and_recording_targets() {
        let document = flow_document("# Heading\n\n- [x] task\n\n---\n\nA paragraph.", None);
        let renderer = Renderer::new(RenderSettings::default());
        let mut layout = renderer.prepare_layout(&document, 420.0);
        let pages = renderer.paginate_layout(&layout, 600);
        let selection = PageSelection {
            requested_page: 0,
            text_anchor: None,
        };
        let options = PaintOptions {
            scale: 1.0,
            interaction: InteractionState::default(),
        };
        let mut pixels = PixelBuffer::new(480, 600, [255, 255, 255]).unwrap();
        let pixel_result = renderer
            .paint_page_into(
                &mut pixels.surface_mut(),
                &mut layout,
                &pages,
                (480, 600),
                selection,
                options,
            )
            .unwrap();
        let mut recording = RecordingPaintTarget::new(480, 600).unwrap();
        let recorded_result = renderer
            .paint_page_into(
                &mut recording,
                &mut layout,
                &pages,
                (480, 600),
                selection,
                options,
            )
            .unwrap();

        assert_eq!(recorded_result, pixel_result);
        assert_eq!(recording.buffer.pixels(), pixels.pixels());
        assert!(matches!(
            recording.commands.first(),
            Some(PaintCommand::Clear(_))
        ));
        assert!(
            recording
                .commands
                .iter()
                .any(|command| matches!(command, PaintCommand::Viewport(..)))
        );
        assert!(recording.commands.iter().any(|command| matches!(
            command,
            PaintCommand::Rect(..)
                | PaintCommand::Line(..)
                | PaintCommand::Mask(..)
                | PaintCommand::Pixels(..)
        )));
    }

    #[test]
    fn renders_representative_markdown_through_flow_layout() {
        let document = flow_document(
            "# Heading\n\n> quoted text\n\n1. ordered\n2. [x] task\n\n```rust\nfn main() {}\n```\n\n---\n\n| Name | Count |\n| --- | ---: |\n| One | 1 |\n\n[linked text](https://example.com) and $x + y$.\n\nfootnote[^n]\n\n[^n]: note text",
            None,
        );
        let renderer = Renderer::new(RenderSettings::default());
        let layout = renderer.prepare_flow_layout(&document, 420.0);

        assert!(
            layout
                .blocks
                .iter()
                .any(|block| block.kind == layout::LayoutBlockKind::Heading)
        );
        assert!(
            layout
                .blocks
                .iter()
                .any(|block| block.kind == layout::LayoutBlockKind::Code)
        );
        assert!(
            layout
                .blocks
                .iter()
                .any(|block| block.kind == layout::LayoutBlockKind::Rule)
        );
        assert!(
            layout
                .blocks
                .iter()
                .any(|block| block.kind == layout::LayoutBlockKind::Table)
        );
        assert!(layout.atomic_objects().any(|object| matches!(
            object.kind,
            layout::AtomicLayoutKind::Placeholder { .. } | layout::AtomicLayoutKind::ThematicBreak
        )));
        assert!(
            layout
                .blocks
                .iter()
                .flat_map(|block| &block.lines)
                .any(|line| {
                    line.runs
                        .iter()
                        .any(|run| run.flow_link.is_some() && run.style.link)
                })
        );

        let painted = renderer
            .paint_viewport(
                &layout,
                520,
                (layout.height + renderer.settings.theme.page_margin * 2.0).ceil() as u32,
                0.0,
                1.0,
                InteractionState::default(),
            )
            .unwrap()
            .0;
        let background = renderer.palette().background;
        let content_x = ((520.0 - layout.width) / 2.0).max(renderer.settings.theme.page_margin);
        for block in &layout.blocks {
            assert!(
                region_has_non_background(
                    &painted,
                    content_x,
                    renderer.settings.theme.page_margin + block.y,
                    layout.width,
                    block.height.max(1.0),
                    background,
                ),
                "flow {:?} block did not paint",
                block.kind,
            );
        }
    }

    #[test]
    fn paginates_and_paints_real_flow_table_geometry() {
        let mut source = String::from("| Name | Value |\n| --- | ---: |\n");
        for index in 0..20 {
            source.push_str(&format!("| row {index} | $x_{index}$ |\n"));
        }
        let document = flow_document(&source, None);
        let renderer = Renderer::new(RenderSettings::default());
        let composed = renderer.compose_document(&document, 320.0);
        let pages = renderer.paginate_composed_document(&composed, 180).unwrap();

        assert!(pages.pages.len() > 1);
        assert!(
            pages
                .pages
                .iter()
                .skip(1)
                .any(|page| page.repeated_table_header.is_some())
        );
        assert!(
            composed
                .blocks
                .iter()
                .flat_map(|block| &block.layout.atomic_objects)
                .next()
                .is_some()
        );
        assert!(
            pages
                .fragments
                .iter()
                .any(|fragment| { matches!(fragment, PageFragment::TableSlice(_)) })
        );
        let header_line = pages
            .fragments
            .iter()
            .find_map(|fragment| match fragment {
                PageFragment::Lines(line) if line.replica => Some(line),
                _ => None,
            })
            .expect("continuation page repeats header lines explicitly");
        let source_line = &composed.blocks[header_line.block].layout.lines[header_line.line_start];
        let source_run = source_line.runs.first().expect("header line contains text");
        let hit = renderer
            .text_position_at_paginated(
                &composed,
                &pages,
                1,
                source_run.x + source_run.width / 2.0,
                header_line.y + header_line.height / 2.0,
            )
            .expect("repeated header remains hit-testable");
        assert!(source_run.range.start <= hit.offset && hit.offset <= source_run.range.end);

        let mut surface = create_surface(420.0, 180.0, 1.0, renderer.palette().background).unwrap();
        renderer
            .paint_paginated_page_into(
                &mut surface.surface_mut(),
                &composed,
                &pages,
                (420, 180),
                PageSelection {
                    requested_page: 1,
                    text_anchor: None,
                },
                PaintOptions {
                    scale: 1.0,
                    interaction: InteractionState::default(),
                },
            )
            .unwrap();
        let background = renderer.palette().background;
        assert!(
            surface
                .pixels()
                .chunks_exact(4)
                .any(|pixel| { pixel[..3] != [background[2], background[1], background[0]] })
        );
        let header = pages.pages[1].repeated_table_header.unwrap();
        let header_start = renderer.settings.theme.page_margin.floor() as usize;
        let header_end = (renderer.settings.theme.page_margin + header.height).ceil() as usize;
        let width = surface.width() as usize;
        assert!((header_start..header_end).any(|y| {
            surface.pixels()[y * width * 4..(y + 1) * width * 4]
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != [background[2], background[1], background[0]])
        }));
    }

    #[test]
    fn paints_atomic_placeholders_from_flow_geometry() {
        let document = flow_document("before $long expression$ after", None);
        let renderer = Renderer::new(RenderSettings::default());
        let layout = renderer.prepare_flow_layout(&document, 200.0);
        let object = &layout.blocks[0].atomic_objects[0];
        let y = object.y;
        let range = object.range;
        let painted = renderer
            .paint_viewport(&layout, 320, 160, 0.0, 1.0, InteractionState::default())
            .unwrap()
            .0;
        let content_x = ((320.0 - layout.width) / 2.0).max(renderer.settings.theme.page_margin);
        assert!(region_has_non_background(
            &painted,
            content_x + object.x,
            renderer.settings.theme.page_margin + object.y,
            object.width,
            object.height,
            renderer.palette().background,
        ));
        let leading = TextPosition {
            offset: range.start,
            affinity: crate::model::Affinity::Downstream,
        };
        let trailing = TextPosition {
            offset: range.end,
            affinity: crate::model::Affinity::Upstream,
        };
        assert_eq!(
            renderer.text_position_at(&layout, object.x + 1.0, y + 1.0),
            Some(leading)
        );
        assert_eq!(
            renderer.text_position_at(&layout, object.x + object.width - 1.0, y + 1.0),
            Some(trailing)
        );
        assert_eq!(renderer.caret_x(&layout, leading), Some(object.leading_x));
        assert_eq!(renderer.caret_x(&layout, trailing), Some(object.trailing_x));
        assert_eq!(
            renderer.adjacent_text_position(&layout, leading, 1),
            Some(trailing)
        );

        let selection = Selection {
            anchor: TextPosition::new(range.start + 1),
            active: TextPosition::new(range.start + 2),
        };
        let selected = renderer
            .paint_viewport(
                &layout,
                320,
                160,
                0.0,
                1.0,
                InteractionState {
                    selection: Some(selection),
                    ..InteractionState::default()
                },
            )
            .unwrap()
            .0;
        let search_range = TextRange {
            start: range.start + 1,
            end: range.start + 2,
        };
        let searched = renderer
            .paint_viewport(
                &layout,
                320,
                160,
                0.0,
                1.0,
                InteractionState {
                    search_matches: std::slice::from_ref(&search_range),
                    active_match: Some(search_range),
                    ..InteractionState::default()
                },
            )
            .unwrap()
            .0;

        assert_ne!(painted.pixels(), selected.pixels());
        assert_ne!(painted.pixels(), searched.pixels());
    }

    #[test]
    fn bidi_boundary_affinity_and_horizontal_movement_follow_visual_geometry() {
        let document = flow_document("abcאבג", None);
        let renderer = Renderer::new(RenderSettings::default());
        let layout = renderer.prepare_flow_layout(&document, 200.0);
        let runs = &layout.blocks[0].lines[0].runs;
        let offset = runs[0].range.end;
        let upstream = TextPosition {
            offset,
            affinity: crate::model::Affinity::Upstream,
        };
        let downstream = TextPosition {
            offset,
            affinity: crate::model::Affinity::Downstream,
        };
        let upstream_x = renderer.caret_x(&layout, upstream).unwrap();
        let downstream_x = renderer.caret_x(&layout, downstream).unwrap();

        assert_ne!(upstream_x, downstream_x);
        let right = renderer
            .adjacent_text_position(&layout, upstream, 1)
            .unwrap();
        assert!(renderer.caret_x(&layout, right).unwrap() >= upstream_x);
        assert_eq!(
            renderer.text_position_at(&layout, downstream_x, layout.blocks[0].lines[0].y + 1.0),
            Some(downstream)
        );
    }

    #[test]
    fn vertical_movement_includes_atomic_only_rows() {
        let document = flow_document("first\n\n---\n\nsecond", None);
        let renderer = Renderer::new(RenderSettings::default());
        let layout = renderer.prepare_flow_layout(&document, 200.0);
        let object = &layout.blocks[1].atomic_objects[0];
        assert!(matches!(
            object.kind,
            layout::AtomicLayoutKind::ThematicBreak
        ));
        let leading = TextPosition {
            offset: object.range.start,
            affinity: crate::model::Affinity::Downstream,
        };

        let up = renderer
            .vertical_text_position(&layout, leading, -1, 10.0)
            .unwrap();
        let down = renderer
            .vertical_text_position(&layout, leading, 1, 10.0)
            .unwrap();
        assert!(up.offset <= layout.blocks[0].range.end);
        assert!(down.offset >= layout.blocks[2].range.start);
    }

    #[test]
    fn discovers_flow_image_metadata_from_semantic_resources() {
        let document = flow_document(
            "![Local](images/chart.png)\n\n![Remote](https://example.com/chart.png)",
            Some("book/"),
        );
        let renderer = Renderer::new(RenderSettings::default());

        renderer.discover_image_metadata(&document);
        let requests = renderer.take_image_metadata_requests();

        assert_eq!(requests.len(), 2);
        assert!(requests.iter().any(|request| {
            request.url == "images/chart.png" && request.resource_base.as_deref() == Some("book/")
        }));
        assert!(requests.iter().any(|request| {
            request.url == "https://example.com/chart.png"
                && request.resource_base.as_deref() == Some("book/")
        }));
    }

    #[test]
    fn paints_atomic_images_with_resource_base_keys() {
        let document = flow_document("![red](images/red.png)", Some("book/"));
        let mut renderer = Renderer::new(RenderSettings::default());
        assert!(renderer.register_image(
            "images/red.png",
            Some("book/"),
            2,
            2,
            [255, 0, 0, 255].repeat(4),
        ));
        let layout = renderer.prepare_flow_layout(&document, 200.0);
        let key = ImageRequest::new("images/red.png", Some("book/".into())).key();
        assert!(matches!(
            &layout.blocks[0].atomic_objects[0].kind,
            layout::AtomicLayoutKind::Image { key: actual, .. } if actual == &key
        ));

        let painted = renderer
            .paint_viewport(&layout, 320, 160, 0.0, 1.0, InteractionState::default())
            .unwrap()
            .0;

        assert!(
            painted
                .pixels()
                .chunks_exact(4)
                .any(|pixel| pixel == [0, 0, 255, 255])
        );
    }

    #[test]
    fn vertical_visibility_rejects_content_outside_or_adjacent_to_the_surface() {
        assert!(vertically_visible(90.0, 20.0, 100.0));
        assert!(vertically_visible(-9.99, 10.0, 100.0));
        assert!(!vertically_visible(-10.0, 10.0, 100.0));
        assert!(!vertically_visible(100.0, 20.0, 100.0));
        assert!(!vertically_visible(-11.0, 10.0, 100.0));
    }

    #[test]
    fn syntax_colors_adapt_to_light_and_dark_palettes() {
        assert_eq!(
            syntax_color(SyntaxKind::Comment, Palette::light()),
            Palette::light().muted
        );
        assert_eq!(
            syntax_color(SyntaxKind::Comment, Palette::dark()),
            Palette::dark().muted
        );
        assert_ne!(
            syntax_color(SyntaxKind::Keyword, Palette::light()),
            Palette::light().foreground
        );
        assert_ne!(
            syntax_color(SyntaxKind::Keyword, Palette::light()),
            syntax_color(SyntaxKind::Keyword, Palette::dark())
        );
    }

    #[test]
    fn persistent_layout_digest_ignores_paint_only_settings() {
        let renderer = Renderer::new(RenderSettings::default());
        let expected = renderer.persistent_layout_digest();

        let mut paint_only = renderer.settings().clone();
        paint_only.dark_mode = !paint_only.dark_mode;
        paint_only.regular_gamma = 1.7;

        paint_only.theme.light.foreground = [17, 18, 19];
        let paint_only = Renderer::new(paint_only);
        assert_eq!(paint_only.persistent_layout_digest(), expected);

        let mut layout = renderer.settings().clone();
        layout.font_size += 1.0;
        let layout = Renderer::new(layout);
        assert_ne!(layout.persistent_layout_digest(), expected);
    }

    #[test]
    fn custom_palette_controls_the_render_surface() {
        let settings = RenderSettings {
            dark_mode: true,
            theme: Theme {
                dark: Palette {
                    background: [12, 34, 56],
                    ..Palette::dark()
                },
                ..Theme::default()
            },
            ..RenderSettings::default()
        };
        let renderer = Renderer::new(settings);
        let document = flow_document("Body", None);
        let surface = renderer.render_full_document(&document, 1.0).unwrap();

        assert_eq!(&surface.pixels()[..4], &[56, 34, 12, 255]);
    }

    #[test]
    fn paginated_body_uses_only_host_selected_margins() {
        let renderer = Renderer::new(RenderSettings {
            theme: Theme {
                page_margin: 20.0,
                ..Theme::default()
            },
            ..RenderSettings::default()
        });
        assert_eq!(renderer.effective_content_width(390), 350.0);
        assert_eq!(renderer.paginated_body_height(260), 220.0);

        let desktop = Renderer::new(RenderSettings::default());
        assert_eq!(desktop.paginated_body_height(260), 164.0);
    }

    #[test]
    fn paginated_render_paints_content_without_reader_chrome() {
        let document = flow_document(&"A synthetic paragraph for pagination. ".repeat(100), None);
        let renderer = Renderer::new(RenderSettings {
            max_width: 320.0,
            ..RenderSettings::default()
        });
        let page = renderer
            .render_page(
                &document,
                (480, 260),
                0,
                None,
                1.0,
                InteractionState::default(),
            )
            .unwrap();
        assert_eq!((page.surface.width(), page.surface.height()), (480, 260));
        assert!(page.pages.pages.len() > 1);
        assert_eq!(page.page_index, 0);
        // The host's bottom margin contains no engine-generated progress label.
        assert!(
            page.surface.pixels()[220 * 480 * 4..]
                .chunks_exact(4)
                .all(|pixel| pixel[..3] == [255, 255, 255])
        );
    }

    #[test]
    fn document_region_matches_the_equivalent_viewport() {
        let document = flow_document(
            &"A long paragraph whose lines cross several retained regions. ".repeat(80),
            None,
        );
        let renderer = Renderer::new(RenderSettings::default());
        let layout = renderer.prepare_layout(&document, renderer.effective_content_width(720));
        let mut viewport = PixelBuffer::new(1440, 480, [0; 3]).unwrap();
        renderer
            .paint_viewport_into(
                &mut viewport.surface_mut(),
                &layout,
                (720, 240),
                180.0,
                PaintOptions {
                    scale: 2.0,
                    interaction: InteractionState::default(),
                },
            )
            .unwrap();
        let mut region = PixelBuffer::new(1440, 480, [0; 3]).unwrap();
        renderer
            .paint_document_region_into(
                &mut region.surface_mut(),
                &layout,
                720.0,
                DocumentRegion {
                    x: 0.0,
                    y: 180.0,
                    width: 720.0,
                    height: 240.0,
                },
                PaintOptions {
                    scale: 2.0,
                    interaction: InteractionState::default(),
                },
            )
            .unwrap();

        assert_eq!(viewport.pixels(), region.pixels());
    }

    #[test]
    fn renders_keyboard_focus_for_a_semantic_target() {
        let document = flow_document("Read [the documentation](https://example.com).", None);
        let renderer = Renderer::new(RenderSettings::default());
        let layout = renderer.prepare_layout(&document, renderer.effective_content_width(720));
        let navigation = NavigationIndex::new(&document, &layout);
        let target = &navigation.targets()[0];
        let plain = renderer
            .render_viewport(&document, 720, 240, 0.0, 1.0, InteractionState::default())
            .unwrap();
        let focused = renderer
            .render_viewport(
                &document,
                720,
                240,
                0.0,
                1.0,
                InteractionState {
                    focused: Some(target),
                    ..InteractionState::default()
                },
            )
            .unwrap();

        assert_ne!(plain.surface.pixels(), focused.surface.pixels());
    }

    #[test]
    fn renders_flow_link_hover_and_focus_for_text_and_atomic_objects() {
        let document = flow_document(
            "[linked text ![chart](chart.png)](https://example.com) and [other link](https://example.com) [![](decorative-a.png)](https://decorative.example)[![](decorative-b.png)](https://decorative.example)",
            Some("book/"),
        );
        let renderer = Renderer::new(RenderSettings::default());
        let layout = renderer.prepare_flow_layout(&document, 420.0);
        let navigation = NavigationIndex::new(&document, &layout);
        let target = navigation
            .targets()
            .iter()
            .find(|target| {
                target.kind == NavigationKind::Link && target.label == "linked text chart"
            })
            .unwrap();
        let other = navigation
            .targets()
            .iter()
            .find(|target| target.kind == NavigationKind::Link && target.label == "other link")
            .unwrap();
        let decorative: Vec<_> = navigation
            .targets()
            .iter()
            .filter(|target| target.kind == NavigationKind::Link && target.label.is_empty())
            .collect();
        let [decorative, decorative_other] = decorative.as_slice() else {
            panic!("expected two decorative flow links");
        };
        assert_eq!(
            decorative.flow_destination(),
            decorative_other.flow_destination()
        );
        let destination = target.flow_destination().unwrap();
        assert_eq!(other.flow_destination(), Some(destination));

        let plain = renderer
            .paint_viewport(&layout, 520, 220, 0.0, 1.0, InteractionState::default())
            .unwrap()
            .0;
        let hover = |target| {
            renderer
                .paint_flow_viewport(
                    &layout,
                    520,
                    220,
                    0.0,
                    1.0,
                    InteractionState::default(),
                    Some(target),
                )
                .unwrap()
                .0
        };
        let hovered = hover(target);
        let other_hovered = hover(other);
        let focused = renderer
            .paint_flow_viewport(
                &layout,
                520,
                220,
                0.0,
                1.0,
                InteractionState {
                    focused: Some(target),
                    ..InteractionState::default()
                },
                None,
            )
            .unwrap()
            .0;

        assert_ne!(plain.pixels(), hovered.pixels());
        assert_ne!(hovered.pixels(), other_hovered.pixels());
        assert_ne!(plain.pixels(), focused.pixels());
        let public_hovered = renderer
            .render_viewport(
                &document,
                520,
                220,
                0.0,
                1.0,
                InteractionState {
                    hovered_target: Some(target),
                    ..InteractionState::default()
                },
            )
            .unwrap()
            .surface;
        assert_ne!(plain.pixels(), public_hovered.pixels());
        let decorative_interactive = renderer
            .paint_flow_viewport(
                &layout,
                520,
                220,
                0.0,
                1.0,
                InteractionState {
                    focused: Some(decorative),
                    ..InteractionState::default()
                },
                Some(decorative),
            )
            .unwrap()
            .0;
        let decorative_other_interactive = renderer
            .paint_flow_viewport(
                &layout,
                520,
                220,
                0.0,
                1.0,
                InteractionState::default(),
                Some(decorative_other),
            )
            .unwrap()
            .0;
        assert_ne!(plain.pixels(), decorative_interactive.pixels());
        assert_ne!(
            decorative_interactive.pixels(),
            decorative_other_interactive.pixels()
        );
        let decorative_atomics: Vec<_> = layout
            .atomic_objects()
            .filter(|atomic| atomic.range.start == atomic.range.end)
            .collect();
        assert_eq!(decorative_atomics.len(), 2);
        for (atomic, target) in decorative_atomics
            .into_iter()
            .zip([decorative, decorative_other])
        {
            assert!(std::ptr::eq(
                navigation
                    .flow_target_at_point(
                        &layout,
                        atomic.x + atomic.width / 2.0,
                        atomic.y + atomic.height / 2.0,
                    )
                    .unwrap(),
                *target,
            ));
        }
        assert!(
            layout
                .atomic_objects()
                .any(|atomic| atomic.flow_link.as_ref() == Some(destination))
        );

        let mut region = PixelBuffer::new(520, 220, [0; 3]).unwrap();
        renderer
            .paint_flow_document_region_into(
                &mut region.surface_mut(),
                &layout,
                520.0,
                DocumentRegion {
                    x: 0.0,
                    y: 0.0,
                    width: 520.0,
                    height: 220.0,
                },
                PaintOptions {
                    scale: 1.0,
                    interaction: InteractionState::default(),
                },
                Some(target),
            )
            .unwrap();
        assert_eq!(region.pixels(), hovered.pixels());

        let mut paginated = layout.clone();
        let pages = renderer.paginate_layout(&paginated, 220);
        let mut page = PixelBuffer::new(520, 220, [0; 3]).unwrap();
        renderer
            .paint_flow_page_into(
                &mut page.surface_mut(),
                &mut paginated,
                &pages,
                (520, 220),
                PageSelection {
                    requested_page: 0,
                    text_anchor: None,
                },
                PaintOptions {
                    scale: 1.0,
                    interaction: InteractionState::default(),
                },
                Some(target),
            )
            .unwrap();
        assert_ne!(plain.pixels(), page.pixels());
    }

    #[test]
    fn hit_tests_and_renders_text_selection() {
        let document = flow_document("Select this text.", None);
        let renderer = Renderer::new(RenderSettings::default());
        let plain = renderer
            .render_viewport(&document, 480, 200, 0.0, 1.0, InteractionState::default())
            .unwrap();
        let line = &plain.layout.blocks[0].lines[0];
        let run = &line.runs[0];
        let start = renderer
            .text_position_at(&plain.layout, run.x, line.y + line.height / 2.0)
            .unwrap();
        let end = TextPosition::new(run.range.end);
        let selected = renderer
            .render_viewport(
                &document,
                480,
                200,
                0.0,
                1.0,
                InteractionState {
                    selection: Some(Selection {
                        anchor: start,
                        active: end,
                    }),
                    ..InteractionState::default()
                },
            )
            .unwrap();

        assert_eq!(start.offset, run.range.start);
        assert_ne!(plain.surface.pixels(), selected.surface.pixels());
    }

    #[test]
    fn scales_host_supplied_image_bytes_relative_to_the_document() {
        let document_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fixture.md");
        let document = flow_document(
            "![Landscape](images/landscape.png)",
            document_path.parent().and_then(Path::to_str),
        );
        let mut renderer = Renderer::new(RenderSettings::default());
        assert!(
            renderer.provide_image_bytes(
                ImageRequest::new(
                    "images/landscape.png",
                    document_path
                        .parent()
                        .and_then(Path::to_str)
                        .map(str::to_owned)
                ),
                include_bytes!("../tests/fixtures/images/landscape.png"),
            )
        );
        let layout = renderer.prepare_flow_layout(&document, 624.0);
        let image = &layout.blocks[0].atomic_objects[0];

        assert_eq!(image.width, 624.0);
        assert_eq!(image.height, 312.0);
    }

    #[test]
    fn lays_out_a_frontend_registered_image() {
        let resource_base = "https://example.com/docs/";
        let document = flow_document("![Diagram](images/diagram.png)", Some(resource_base));
        let mut renderer = Renderer::new(RenderSettings::default());
        assert!(renderer.register_image(
            "images/diagram.png",
            Some(resource_base),
            4,
            2,
            vec![255; 4 * 2 * 4],
        ));
        let layout = renderer.prepare_flow_layout(&document, 624.0);
        let image = &layout.blocks[0].atomic_objects[0];

        assert_eq!((image.width, image.height), (4.0, 2.0));
    }

    #[test]
    fn rejects_invalid_frontend_resources() {
        let mut renderer = Renderer::new(RenderSettings::default());
        assert!(!renderer.register_font(Arc::from(Vec::<u8>::new())));
        assert!(!renderer.register_image("bad.png", None, 2, 2, vec![0; 3]));
    }

    #[test]
    fn blockquote_bar_spans_gaps_between_its_blocks() {
        let document = flow_document("> First paragraph.\n>\n> Second paragraph.", None);
        let renderer = Renderer::new(RenderSettings::default());
        let layout = renderer.prepare_flow_layout(&document, 468.0);
        assert_eq!(layout.blocks.len(), 2);

        let spans = &layout.quote_bars;
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].y, layout.blocks[0].y);
        assert_eq!(
            spans[0].y + spans[0].height,
            layout.blocks[1].y + layout.blocks[1].height
        );
        assert!(spans[0].height > layout.blocks[0].height + layout.blocks[1].height);
    }

    #[test]
    fn nested_blockquote_bar_stays_close_to_nested_content() {
        let document = flow_document("> Outer quote.\n>\n> > Nested quote.", None);
        let renderer = Renderer::new(RenderSettings::default());
        let layout = renderer.prepare_flow_layout(&document, 468.0);
        let spans = &layout.quote_bars;

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].x, 12.0);
        assert_eq!(spans[1].x, 40.0);
        let nested_x = layout.blocks[1].lines[0].runs[0].x;
        assert_eq!(nested_x - spans[1].x, 16.0);
    }

    #[test]
    fn scale_does_not_change_logical_text_metrics() {
        let renderer = Renderer::new(RenderSettings {
            font_size: 14.0,
            max_width: 468.0,
            ..RenderSettings::default()
        });
        let style = TextStyle {
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
        };
        let first = renderer.fonts.measure("flow", &style);
        let second = renderer.fonts.measure("flow", &style);
        assert!((first.width - second.width).abs() < 0.01);
    }
}
