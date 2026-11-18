//! Session-owned forwarding of viewer mutations. No mutable viewer escapes.
use super::*;
use flow_document::{
    pixels::PaintTarget,
    text::{GlyphPositioning, RasterizationOptions},
    viewer::{PointerButtonInput, ViewerPosition},
};
use std::time::Duration;

macro_rules! viewing_operations {
    ($($name:ident($($argument:ident: $ty:ty),*) -> $result:ty;)+) => {
        impl ReadingSession {
            $(pub fn $name(&mut self, $($argument: $ty),*) -> $result {
                self.viewer.$name($($argument),*)
            })+
        }
    };
}

// These operations do not change document identity, typography, viewport
// dimensions, or intrinsic resource geometry.
viewing_operations! {
    tick(now: Duration) -> ();
    cancel_pending_interaction() -> ();
    request_redraw() -> ();
    wheel(delta: f32) -> ();
    scroll_by(delta: f32) -> bool;
    scroll_to(position: f32) -> ();
    set_continuous_scroll_position(position: f32) -> ();
    focus_adjacent(direction: isize, headings_only: bool) -> bool;
    clear_selection() -> ();
    restore_position(position: ViewerPosition) -> ();
    set_search_query(query: impl Into<String>) -> ();
    set_search_query_near(query: impl Into<String>, origin: Option<&ViewerPosition>) -> ();
    restore_stable_position(position: &StableTextPosition, same_revision: bool) -> bool;
    activate_link(destination: impl Into<String>) -> bool;
    trim_image_cache() -> ();
    extend_pointer_selection(position: flow_document::selection::TextPosition) -> bool;
    move_to_search_match(reverse: bool) -> bool;
    navigate_to_heading(identity: &flow_document::navigation::NavigationIdentity) -> bool;
    navigate_to_fragment(fragment: &str) -> bool;
    activate_focused() -> bool;
    copy_selection() -> bool;
}

// Font-dependent interactions borrow the same context as layout and painting.
macro_rules! font_viewing_operations {
    ($($name:ident($($argument:ident: $ty:ty),*) -> $result:ty;)+) => {
        impl ReadingSession {
            $(pub fn $name(&mut self, $($argument: $ty),*) -> $result {
                self.viewer.with_font_context(&mut self.font_context, |viewer| viewer.$name($($argument),*))
            })+
        }
    };
}
font_viewing_operations! {
    toggle_visual_selection() -> ();
    move_visual_selection(direction: isize, vertical: bool) -> ();
    pointer_button(input: PointerButtonInput, now: Duration) -> ();
    pointer_moved(position: Option<(f32, f32)>, now: Duration) -> ();
}

impl ReadingSession {
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn register_font(&mut self, data: Arc<[u8]>) -> bool {
        self.register_font_resource(FontResource::from_bytes(data))
    }

    pub fn set_font_size(&mut self, size: f32) {
        let mut settings = self.settings.clone();
        settings.font_size = size;
        self.set_settings(settings);
    }

    pub fn set_max_width(&mut self, width: f32) {
        let mut settings = self.settings.clone();
        settings.max_width = width;
        self.set_settings(settings);
    }

    pub fn set_dark_mode(&mut self, dark: bool) {
        let mut settings = self.settings.clone();
        settings.dark_mode = dark;
        self.set_settings(settings);
    }

    pub fn set_rasterization(&mut self, options: RasterizationOptions) {
        let mut settings = self.settings.clone();
        settings.regular_gamma = options.regular_gamma;
        settings.emphasized_gamma = options.emphasized_gamma;
        settings.font_hinting = options.hinting;
        settings.fractional_positioning = options.positioning == GlyphPositioning::Fractional;
        self.set_settings(settings);
    }

    pub fn prepare_continuous_scene(&mut self, width: u32, height: u32) -> bool {
        self.resize((width, height));
        self.viewer
            .with_font_context(&mut self.font_context, |viewer| {
                viewer.prepare_continuous_scene(width, height)
            })
    }

    pub fn prepare_pagination(&mut self, width: u32, height: u32) -> Option<(usize, usize)> {
        self.resize((width, height));
        self.viewer
            .with_font_context(&mut self.font_context, |viewer| {
                viewer.prepare_pagination(width, height)
            })
    }

    pub fn render_into<T: PaintTarget>(
        &mut self,
        target: &mut T,
        width: u32,
        height: u32,
        scale: f32,
    ) -> anyhow::Result<()> {
        self.resize((width, height));
        self.viewer
            .with_font_context(&mut self.font_context, |viewer| {
                viewer.render_into(target, width, height, scale)
            })
    }

    pub fn render_continuous_region_into<T: PaintTarget>(
        &mut self,
        target: &mut T,
        width: u32,
        y: f32,
        height: f32,
        scale: f32,
    ) -> anyhow::Result<()> {
        self.resize((width, self.viewer.viewport_size().1));
        self.viewer
            .with_font_context(&mut self.font_context, |viewer| {
                viewer.render_continuous_region_into(target, width, y, height, scale)
            })
    }

    pub fn set_settings(&mut self, settings: Settings) {
        if !self
            .settings
            .rendering
            .layout_equivalent(&settings.rendering)
            || self.settings.reading_mode != settings.reading_mode
        {
            self.clear_pagination();
        }
        self.settings = settings.clone();
        self.viewer.set_settings(settings);
    }

    pub fn resize(&mut self, size: (u32, u32)) {
        if self.viewer.viewport_size() != size {
            self.clear_pagination();
        }
        self.viewer.resize(size);
    }

    pub fn set_reading_mode(&mut self, mode: ReadingMode) {
        let mut settings = self.settings.clone();
        settings.reading_mode = mode;
        self.set_settings(settings);
    }

    pub fn register_font_resource(&mut self, resource: FontResource) -> bool {
        let mut fonts = self.font_resources.clone();
        fonts.push(resource);
        let mut scoped = fonts.clone();
        if let Some(active) = &self.active
            && let Some(document) = self
                .publications
                .get(active.key.publication)
                .and_then(|model| model.document(&active.key.document))
        {
            scoped.extend(document.fonts.iter().filter_map(|font| {
                self.fonts
                    .get(&(active.key.publication, font.clone()))
                    .cloned()
            }));
        }
        if !self
            .viewer
            .set_font_resources_with_context(scoped, &mut self.font_context)
        {
            return false;
        }
        self.font_resources = fonts;
        self.clear_pagination();
        true
    }

    pub fn prepare_active_artifacts(&mut self) -> anyhow::Result<()> {
        self.viewer
            .with_font_context(&mut self.font_context, |viewer| {
                viewer.prepare_active_artifacts()
            })
    }

    pub fn install_persistent_composition(
        &mut self,
        width: f32,
        composition: flow_document::layout::ComposedDocument,
    ) -> bool {
        self.clear_pagination();
        self.viewer
            .install_persistent_composition(width, composition)
    }

    pub fn install_persistent_pagination(
        &mut self,
        height: u32,
        pages: flow_document::pagination::PaginatedLayout,
    ) -> bool {
        self.clear_pagination();
        self.viewer.install_persistent_pagination(height, pages)
    }
}
