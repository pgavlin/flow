//! Portable single-document viewing state.
//!
//! `DocumentViewer` coordinates a single flow document. Hosts supply documents,
//! normalized input, fonts, resources, and surfaces, and execute returned effects.
//! Application UI and platform lifecycle remain outside the engine.

pub use crate::render::{FontResource, ImagePixels, ImageRequest};

use crate::{
    layout::{ComposedDocument, ContinuousLayout, GeneratedAction},
    model::{
        TextRange,
        flow::{self, FlowDocument},
    },
    navigation::{NavigationIdentity, NavigationIndex, NavigationKind, local_flow_anchor},
    pagination::PaginatedLayout,
    pixels::PaintTarget,
    render::{DocumentRegion, InteractionState, PageSelection, PaintOptions, Renderer},
    search::SearchIndex,
    selection::{
        Selection, SelectionGranularity, StableFlowSelection, TextPosition, capture_flow_selection,
        recover_flow_selection, selected_text, selection_at,
    },
    settings::{ReadingMode, Settings},
    viewer_cache::{
        CompositionKey, ContinuousPlacementKey, LayoutArtifactCache, PaginationKey, SemanticKey,
    },
};
use std::{sync::Arc, time::Duration};

const POINTER_DRAG_THRESHOLD: f32 = 5.0;
const LINK_ACTIVATION_DELAY: Duration = Duration::from_millis(500);
const PAGE_WHEEL_THRESHOLD: f32 = 15.0;

#[derive(Clone)]
pub struct ViewerPosition {
    text_anchor: usize,
    focus: Option<NavigationIdentity>,
    search_match: Option<usize>,
}

use crate::position::StableTextPosition;

#[derive(Clone)]
enum ViewerActivation {
    Destination {
        destination: flow::Destination,
        relation: Option<flow::LinkRelation>,
        source: Option<Box<NavigationIdentity>>,
    },
    Generated(GeneratedAction),
}

#[derive(Clone)]
struct NoteReturnContext {
    definition: flow::AnchorId,
    position: ViewerPosition,
    focus: Option<NavigationIdentity>,
}

struct SelectionDrag {
    pub origin: (f32, f32),
    pub activation: Option<ViewerActivation>,
    pub moved: bool,
    pub granularity: crate::selection::SelectionGranularity,
    pub base: Selection,
}

struct PendingLinkActivation {
    activation: ViewerActivation,
    focus: Option<NavigationIdentity>,
    deadline: Duration,
}

/// Work the platform host must perform on behalf of a document viewer.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewerEffect {
    Redraw,
    SetClipboard(String),
    FollowLink(flow::Destination),
    RequestImageMetadata(ImageRequest),
    RequestImagePixels(ImageRequest),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointerButtonInput {
    pub pressed: bool,
    pub click_count: u8,
}

/// Revisions let an embedder observe state classes without turning every
/// change into a callback or effect.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ViewerRevisions {
    pub document: u64,
    pub layout: u64,
    pub paint: u64,
    pub position: u64,
    pub semantics: u64,
}

enum Action {
    Resize((u32, u32)),
    Tick(Duration),
    SetContinuousScrollPosition(f32),
    SetReadingMode(ReadingMode),
    ScrollBy(f32),
    Wheel(f32),
    ScrollTo(f32),
    FocusAdjacent {
        direction: isize,
        headings_only: bool,
    },
    ToggleVisualSelection,
    MoveVisualSelection {
        direction: isize,
        vertical: bool,
    },
    ClearSelection,
    RestorePosition(ViewerPosition),
    SetSearchQuery {
        query: String,
        origin: Option<ViewerPosition>,
    },
    MoveToSearchMatch {
        reverse: bool,
    },
    NavigateToHeading(NavigationIdentity),
    ActivateFocused,
    ActivateDestination(flow::Destination),
    Back,
    PointerMoved {
        position: Option<(f32, f32)>,
        now: Duration,
    },
    PointerButton {
        input: PointerButtonInput,
        now: Duration,
    },
    CopySelection,
}

#[derive(Clone, Copy, Debug, Default)]
struct ActionResult {
    value: bool,
}

/// Opaque state for a temporarily suspended document.
pub struct ViewerSession {
    document_name: String,
    document: Option<FlowDocument>,
    document_error: Option<String>,
    scroll_y: f32,
    current_page: usize,
    page_anchor: Option<usize>,
    pages: Arc<PaginatedLayout>,
    content_height: f32,
    composed: Arc<ComposedDocument>,
    layout: Arc<ContinuousLayout>,
    semantic_key: Option<SemanticKey>,
    composition_key: Option<CompositionKey>,
    continuous_key: Option<ContinuousPlacementKey>,
    pagination_key: Option<PaginationKey>,
    layout_x: f32,
    navigation: NavigationIndex,
    focused: Option<NavigationIdentity>,
    history: Vec<ViewerPosition>,
    note_return: Option<NoteReturnContext>,
    search_index: SearchIndex,
    confirmed_query: String,
    search_matches: Vec<TextRange>,
    active_match: Option<usize>,
    selection: Option<Selection>,
    selection_recovery: Option<StableFlowSelection>,
    visual_selection: bool,
    selection_preferred_x: Option<f32>,
}

impl ViewerSession {
    /// Read-only semantic input for publication-owned background work.
    #[doc(hidden)]
    pub fn document(&self) -> Option<&FlowDocument> {
        self.document.as_ref()
    }

    #[doc(hidden)]
    pub fn take_document(&mut self) -> Option<(String, FlowDocument, Option<SemanticKey>)> {
        self.document
            .take()
            .map(|document| (self.document_name.clone(), document, self.semantic_key))
    }
}

/// Platform-neutral state for displaying and interacting with one document.
///
/// Changes must go through viewer operations so retained state stays synchronized.
///
/// ```compile_fail
/// fn replace(viewer: &mut flow_document::viewer::DocumentViewer) {
///     viewer.document = None;
/// }
/// ```
///
/// ```compile_fail
/// fn change_renderer(viewer: &mut flow_document::viewer::DocumentViewer,
///                    renderer: flow_document::render::Renderer) {
///     viewer.renderer = renderer;
/// }
/// ```
pub struct DocumentViewer {
    document_name: String,
    document: Option<FlowDocument>,
    document_error: Option<String>,
    font_size_points: f32,
    max_width_points: f32,
    dark_mode: bool,
    renderer: Renderer,
    pointer_position: Option<(f32, f32)>,
    page_wheel_accumulator: f32,
    hovered_link: Option<NavigationIdentity>,
    size: (u32, u32),
    reading_mode: ReadingMode,
    scroll_y: f32,
    current_page: usize,
    page_anchor: Option<usize>,

    pages: Arc<PaginatedLayout>,
    content_height: f32,
    composed: Arc<ComposedDocument>,
    layout: Arc<ContinuousLayout>,
    semantic_key: Option<SemanticKey>,
    composition_key: Option<CompositionKey>,
    continuous_key: Option<ContinuousPlacementKey>,
    pagination_key: Option<PaginationKey>,
    artifact_cache: LayoutArtifactCache,
    next_semantic_key: u64,
    layout_x: f32,
    navigation: NavigationIndex,
    focused: Option<NavigationIdentity>,
    history: Vec<ViewerPosition>,
    note_return: Option<NoteReturnContext>,
    search_index: SearchIndex,
    confirmed_query: String,
    search_matches: Vec<TextRange>,
    active_match: Option<usize>,
    selection: Option<Selection>,
    selection_recovery: Option<StableFlowSelection>,
    visual_selection: bool,
    selection_preferred_x: Option<f32>,
    selection_drag: Option<SelectionDrag>,
    pending_link_activation: Option<PendingLinkActivation>,
    last_selection_page_turn: Option<Duration>,
    effects: Vec<ViewerEffect>,
    revisions: ViewerRevisions,
}

impl DocumentViewer {
    pub fn new(document_name: impl Into<String>, settings: Settings) -> Self {
        let font_size_points = settings.font_size;
        let max_width_points = settings.max_width;
        let dark_mode = settings.dark_mode;
        let reading_mode = settings.reading_mode;
        let page_margin = settings.theme.page_margin;
        Self {
            document_name: document_name.into(),
            document: None,
            document_error: None,
            font_size_points,
            max_width_points,
            dark_mode,
            renderer: Renderer::new(settings),
            pointer_position: None,
            page_wheel_accumulator: 0.0,
            hovered_link: None,
            size: (920, 760),
            reading_mode,
            scroll_y: 0.0,
            current_page: 0,
            page_anchor: None,

            pages: Arc::new(PaginatedLayout::default()),
            content_height: 0.0,
            composed: Arc::new(ComposedDocument::default()),
            layout: Arc::new(ContinuousLayout::default()),
            semantic_key: None,
            composition_key: None,
            continuous_key: None,
            pagination_key: None,
            artifact_cache: LayoutArtifactCache::default(),
            next_semantic_key: 1,
            layout_x: page_margin,
            navigation: NavigationIndex::default(),
            focused: None,
            history: Vec::new(),
            note_return: None,
            search_index: SearchIndex::default(),
            confirmed_query: String::new(),
            search_matches: Vec::new(),
            active_match: None,
            selection: None,
            selection_recovery: None,
            visual_selection: false,
            selection_preferred_x: None,
            selection_drag: None,
            pending_link_activation: None,
            last_selection_page_turn: None,
            effects: Vec::new(),
            revisions: ViewerRevisions::default(),
        }
    }

    fn current_composition_key(&self) -> Option<CompositionKey> {
        let key = self.composition_key?;
        (key.semantic == self.semantic_key?
            && key.renderer_revision == self.renderer.layout_revision()
            && key.environment == self.renderer.environment_key()
            && key.content_width == self.renderer.effective_content_width(self.size.0).to_bits())
        .then_some(key)
    }

    /// Returns the prepared continuous layout, if still valid.
    pub fn continuous_layout(&self) -> Option<&ContinuousLayout> {
        let composition = self.current_composition_key()?;
        (self.continuous_key == Some(ContinuousPlacementKey { composition }))
            .then_some(self.layout.as_ref())
    }

    /// Returns the prepared page layout, if still valid.
    pub fn paginated_layout(&self) -> Option<&PaginatedLayout> {
        let composition = self.current_composition_key()?;
        let body_height = self.renderer.paginated_body_height(self.size.1).to_bits();
        (self.pagination_key
            == Some(PaginationKey {
                composition,
                body_height,
            }))
        .then_some(self.pages.as_ref())
    }

    pub fn viewport_size(&self) -> (u32, u32) {
        self.size
    }

    #[doc(hidden)]
    pub fn diagnostic_state(&self) -> String {
        format!(
            "mode={:?} viewport={}x{} page={} pages={} scroll_y={:.2} anchor={:?}",
            self.reading_mode,
            self.size.0,
            self.size.1,
            self.current_page + 1,
            self.pages.pages.len(),
            self.scroll_y,
            self.page_anchor,
        )
    }

    pub fn reading_mode(&self) -> ReadingMode {
        self.reading_mode
    }

    pub fn title(&self) -> &str {
        &self.document_name
    }

    pub fn has_document(&self) -> bool {
        self.document.is_some() && self.document_error.is_none()
    }

    pub fn font_size(&self) -> f32 {
        self.font_size_points
    }

    pub fn max_width(&self) -> f32 {
        self.max_width_points
    }

    pub fn dark_mode(&self) -> bool {
        self.dark_mode
    }

    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    pub fn history_is_empty(&self) -> bool {
        self.history.is_empty()
    }

    pub fn cancel_pending_interaction(&mut self) {
        self.pending_link_activation = None;
    }

    pub fn visual_selection(&self) -> bool {
        self.visual_selection
    }

    pub fn active_search_match(&self) -> Option<usize> {
        self.active_match
    }

    pub fn search_query(&self) -> &str {
        &self.confirmed_query
    }

    pub fn outline(&self) -> crate::toc::TocTree {
        self.document
            .as_ref()
            .map(crate::toc::TocTree::new)
            .unwrap_or_default()
    }

    pub fn search_matches(&self) -> &[TextRange] {
        &self.search_matches
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn revisions(&self) -> ViewerRevisions {
        self.revisions
    }

    fn dispatch(&mut self, action: Action) -> ActionResult {
        let before_position = (
            self.reading_mode,
            self.scroll_y.to_bits(),
            self.current_page,
            self.page_anchor,
        );
        let before_semantics = (
            self.focused.clone(),
            self.selection,
            self.visual_selection,
            self.active_match,
            self.confirmed_query.clone(),
        );

        let result = match action {
            Action::Resize(size) => self.reduce_resize(size),
            Action::Tick(now) => self.reduce_tick(now),
            Action::SetContinuousScrollPosition(position) => {
                self.reduce_set_continuous_scroll_position(position)
            }
            Action::SetReadingMode(mode) => self.reduce_set_reading_mode(mode),
            Action::ScrollBy(delta) => self.reduce_scroll_by(delta),
            Action::Wheel(delta) => self.reduce_wheel(delta),
            Action::ScrollTo(position) => self.reduce_scroll_to(position),
            Action::FocusAdjacent {
                direction,
                headings_only,
            } => self.reduce_focus_adjacent(direction, headings_only),
            Action::ToggleVisualSelection => self.reduce_toggle_visual_selection(),
            Action::MoveVisualSelection {
                direction,
                vertical,
            } => self.reduce_move_visual_selection(direction, vertical),
            Action::ClearSelection => self.reduce_clear_selection(),
            Action::RestorePosition(position) => self.reduce_restore_position(position),
            Action::SetSearchQuery { query, origin } => {
                self.reduce_set_search_query(query, origin.as_ref())
            }
            Action::MoveToSearchMatch { reverse } => self.reduce_move_to_search_match(reverse),
            Action::NavigateToHeading(identity) => self.reduce_navigate_to_heading(&identity),
            Action::ActivateFocused => self.reduce_activate_focused(),
            Action::ActivateDestination(destination) => {
                self.reduce_activate_destination(&destination, None, None)
            }
            Action::Back => self.reduce_go_back(),
            Action::PointerMoved { position, now } => self.reduce_pointer_moved(position, now),
            Action::PointerButton { input, now } => self.reduce_pointer_button(input, now),
            Action::CopySelection => self.reduce_copy_selection(),
        };

        let after_position = (
            self.reading_mode,
            self.scroll_y.to_bits(),
            self.current_page,
            self.page_anchor,
        );
        if after_position != before_position {
            self.revisions.position = self.revisions.position.wrapping_add(1);
        }
        let after_semantics = (
            self.focused.clone(),
            self.selection,
            self.visual_selection,
            self.active_match,
            self.confirmed_query.clone(),
        );
        if after_semantics != before_semantics {
            self.revisions.semantics = self.revisions.semantics.wrapping_add(1);
        }
        result
    }

    pub fn take_effects(&mut self) -> Vec<ViewerEffect> {
        std::mem::take(&mut self.effects)
    }

    pub fn next_wakeup(&self) -> Option<Duration> {
        self.pending_link_activation
            .as_ref()
            .map(|pending| pending.deadline)
    }

    pub fn tick(&mut self, now: Duration) {
        self.dispatch(Action::Tick(now));
    }

    fn reduce_tick(&mut self, now: Duration) -> ActionResult {
        let ready = self
            .pending_link_activation
            .as_ref()
            .is_some_and(|pending| pending.deadline <= now);
        if !ready {
            return ActionResult::default();
        }
        let pending = self
            .pending_link_activation
            .take()
            .expect("ready link activation exists");
        if let Some(focus) = pending.focus {
            self.focused = Some(focus);
        }
        self.reduce_activation(&pending.activation)
    }

    pub fn set_font_resources(&mut self, resources: Vec<FontResource>) -> bool {
        if !self.renderer.set_font_resources(resources) {
            return false;
        }
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
        true
    }

    /// A cache namespace, not a source URL or a loader instruction.
    pub fn set_resource_namespace(&mut self, namespace: Option<String>) {
        self.renderer.set_resource_namespace(namespace);
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
    }

    pub fn register_font(&mut self, data: Arc<[u8]>) -> bool {
        self.register_font_resource(FontResource::from_bytes(data))
    }

    pub fn register_font_resource(&mut self, resource: FontResource) -> bool {
        if !self.renderer.register_font_resource(resource) {
            return false;
        }
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
        true
    }

    #[doc(hidden)]
    pub fn request_redraw(&mut self) {
        self.revisions.paint = self.revisions.paint.wrapping_add(1);
        if !self.effects.contains(&ViewerEffect::Redraw) {
            self.effects.push(ViewerEffect::Redraw);
        }
    }

    pub(crate) fn collect_image_requests(&mut self) {
        self.effects.extend(
            self.renderer
                .take_image_metadata_requests()
                .into_iter()
                .map(ViewerEffect::RequestImageMetadata),
        );
        self.effects.extend(
            self.renderer
                .take_image_pixel_requests()
                .into_iter()
                .map(ViewerEffect::RequestImagePixels),
        );
    }

    /// Replaces the displayed document while recovering a selection from its
    /// logical-text context where possible.
    pub fn replace_document(&mut self, result: Result<FlowDocument, String>) {
        self.pending_link_activation = None;
        self.capture_anchor();
        let recovery = self
            .document
            .as_ref()
            .zip(self.selection)
            .and_then(|(document, selection)| capture_flow_selection(document, selection))
            .or_else(|| self.selection_recovery.take());
        let should_recover = self.selection.is_some() || recovery.is_some();

        match result {
            Ok(document) => {
                let selection = recovery
                    .as_ref()
                    .and_then(|stable| recover_flow_selection(&document, stable));
                self.document = Some(document);
                self.document_error = None;
                self.semantic_key = Some(self.allocate_semantic_key());
                self.selection_recovery = None;
                if should_recover {
                    self.selection = selection;
                    if selection.is_none() {
                        self.visual_selection = false;
                        self.selection_preferred_x = None;
                    }
                }
            }
            Err(error) => {
                self.document = None;
                self.document_error = Some(error);
                self.semantic_key = None;
                self.selection_recovery = recovery;
            }
        }
        self.invalidate_active_artifacts();
        self.revisions.document = self.revisions.document.wrapping_add(1);
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.revisions.semantics = self.revisions.semantics.wrapping_add(1);
        self.rebuild_document_indexes();
        if let Some(document) = &self.document {
            self.renderer.discover_image_metadata(document);
        }
        self.collect_image_requests();
        self.request_redraw();
    }

    /// Replaces the active document with a temporary document and returns the
    /// previous document's opaque session for later restoration.
    pub fn suspend_with_document(
        &mut self,
        document_name: impl Into<String>,
        document: FlowDocument,
    ) -> ViewerSession {
        self.suspend_with_document_key(document_name, document, None)
    }

    #[doc(hidden)]
    pub fn suspend_with_document_key(
        &mut self,
        document_name: impl Into<String>,
        document: FlowDocument,
        semantic_key: Option<SemanticKey>,
    ) -> ViewerSession {
        self.capture_anchor();
        let suspended = self.take_session();
        self.document_name = document_name.into();
        self.document = Some(document);
        self.document_error = None;
        self.reset_session_state();
        self.semantic_key = Some(semantic_key.unwrap_or_else(|| self.allocate_semantic_key()));
        self.rebuild_document_indexes();
        if let Some(document) = &self.document {
            self.renderer.discover_image_metadata(document);
        }
        self.collect_image_requests();
        self.revisions.document = self.revisions.document.wrapping_add(1);
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.revisions.semantics = self.revisions.semantics.wrapping_add(1);
        self.request_redraw();
        suspended
    }

    /// Activates a suspended session and returns the session that was active.
    pub fn activate_session(&mut self, session: ViewerSession) -> ViewerSession {
        self.capture_anchor();
        let previous = self.take_session();
        self.install_session(session);
        if let Some(document) = &self.document {
            self.renderer.discover_image_metadata(document);
        }
        self.collect_image_requests();
        self.pending_link_activation = None;
        self.selection_drag = None;
        self.last_selection_page_turn = None;
        self.hovered_link = None;
        self.refresh_hover();
        self.revisions.document = self.revisions.document.wrapping_add(1);
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.revisions.semantics = self.revisions.semantics.wrapping_add(1);
        self.request_redraw();
        previous
    }

    fn take_session(&mut self) -> ViewerSession {
        ViewerSession {
            document_name: std::mem::take(&mut self.document_name),
            document: self.document.take(),
            document_error: self.document_error.take(),
            scroll_y: self.scroll_y,
            current_page: self.current_page,
            page_anchor: self.page_anchor.take(),
            pages: std::mem::take(&mut self.pages),
            content_height: self.content_height,
            composed: std::mem::take(&mut self.composed),
            layout: std::mem::take(&mut self.layout),
            semantic_key: self.semantic_key.take(),
            composition_key: self.composition_key.take(),
            continuous_key: self.continuous_key.take(),
            pagination_key: self.pagination_key.take(),
            layout_x: self.layout_x,
            navigation: std::mem::take(&mut self.navigation),
            focused: self.focused.take(),
            history: std::mem::take(&mut self.history),
            note_return: self.note_return.take(),
            search_index: std::mem::take(&mut self.search_index),
            confirmed_query: std::mem::take(&mut self.confirmed_query),
            search_matches: std::mem::take(&mut self.search_matches),
            active_match: self.active_match.take(),
            selection: self.selection.take(),
            selection_recovery: self.selection_recovery.take(),
            visual_selection: self.visual_selection,
            selection_preferred_x: self.selection_preferred_x,
        }
    }

    fn install_session(&mut self, session: ViewerSession) {
        self.document_name = session.document_name;
        self.document = session.document;
        self.document_error = session.document_error;
        self.scroll_y = session.scroll_y;
        self.current_page = session.current_page;
        self.page_anchor = session.page_anchor;
        self.pages = session.pages;
        self.content_height = session.content_height;
        self.composed = session.composed;
        self.layout = session.layout;
        self.semantic_key = session.semantic_key;
        self.composition_key = session.composition_key;
        self.continuous_key = session.continuous_key;
        self.pagination_key = session.pagination_key;
        self.layout_x = session.layout_x;
        self.navigation = session.navigation;
        self.focused = session.focused;
        self.history = session.history;
        self.note_return = session.note_return;
        self.search_index = session.search_index;
        self.confirmed_query = session.confirmed_query;
        self.search_matches = session.search_matches;
        self.active_match = session.active_match;
        self.selection = session.selection;
        self.selection_recovery = session.selection_recovery;
        self.visual_selection = session.visual_selection;
        self.selection_preferred_x = session.selection_preferred_x;
    }

    fn allocate_semantic_key(&mut self) -> SemanticKey {
        let key = SemanticKey(self.next_semantic_key);
        self.next_semantic_key = self.next_semantic_key.wrapping_add(1).max(1);
        key
    }

    fn invalidate_active_artifacts(&mut self) {
        self.composition_key = None;
        self.continuous_key = None;
        self.pagination_key = None;
        self.pages = Arc::new(PaginatedLayout::default());
    }

    fn reset_session_state(&mut self) {
        self.scroll_y = 0.0;
        self.current_page = 0;
        self.page_anchor = Some(0);
        self.pages = Arc::new(PaginatedLayout::default());
        self.content_height = 0.0;
        self.composed = Arc::new(ComposedDocument::default());
        self.layout = Arc::new(ContinuousLayout::default());
        self.semantic_key = None;
        self.composition_key = None;
        self.continuous_key = None;
        self.pagination_key = None;
        self.layout_x = self.renderer.settings().theme.page_margin;
        self.navigation = NavigationIndex::default();
        self.focused = None;
        self.history.clear();
        self.note_return = None;
        self.search_index = SearchIndex::default();
        self.confirmed_query.clear();
        self.search_matches.clear();
        self.active_match = None;
        self.selection = None;
        self.selection_recovery = None;
        self.visual_selection = false;
        self.selection_preferred_x = None;
    }

    fn rebuild_document_indexes(&mut self) {
        let Some(document) = &self.document else {
            self.navigation = NavigationIndex::default();
            self.search_index = SearchIndex::default();
            self.search_matches.clear();
            self.active_match = None;
            self.focused = None;
            return;
        };
        let maximum_source = document.text().len().as_usize();
        if self.selection.is_some_and(|selection| {
            selection.anchor.offset > maximum_source || selection.active.offset > maximum_source
        }) {
            self.selection = None;
            self.visual_selection = false;
            self.selection_preferred_x = None;
        }
        let navigation = NavigationIndex::new(document, &self.layout);
        if self
            .focused
            .as_ref()
            .is_some_and(|identity| navigation.target(identity).is_none())
        {
            self.focused = None;
        }
        self.navigation = navigation;
        self.search_index = SearchIndex::new(document);
        self.search_matches = self.search_index.find(&self.confirmed_query);
        self.active_match = self
            .active_match
            .filter(|index| *index < self.search_matches.len())
            .or_else(|| (!self.search_matches.is_empty()).then_some(0));
    }

    pub fn register_image(
        &mut self,
        url: &str,
        resource_base: Option<&str>,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> bool {
        self.provide_image_rgba(
            ImageRequest::new(url, resource_base.map(str::to_owned)),
            width,
            height,
            rgba,
        )
    }

    pub fn register_image_pixels(
        &mut self,
        url: &str,
        resource_base: Option<&str>,
        pixels: ImagePixels,
    ) -> bool {
        if !self
            .renderer
            .register_image_pixels(url, resource_base, pixels)
        {
            return false;
        }
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
        true
    }

    pub fn provide_image_metadata(
        &mut self,
        request: ImageRequest,
        width: u32,
        height: u32,
    ) -> bool {
        if !self.renderer.provide_image_metadata(request, width, height) {
            return false;
        }
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
        true
    }

    pub fn provide_image_rgba(
        &mut self,
        request: ImageRequest,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> bool {
        if !self
            .renderer
            .provide_image_rgba(request, width, height, rgba)
        {
            return false;
        }
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
        true
    }

    pub fn provide_image_pixels(&mut self, request: ImageRequest, pixels: ImagePixels) -> bool {
        if !self.renderer.provide_image_pixels(request, pixels) {
            return false;
        }
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
        true
    }

    pub fn provide_image_bytes(&mut self, request: ImageRequest, bytes: &[u8]) -> bool {
        if !self.renderer.provide_image_bytes(request, bytes) {
            return false;
        }
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
        true
    }

    pub fn fail_image_metadata(&mut self, request: &ImageRequest) {
        self.renderer.fail_image_metadata(request);
    }

    pub fn fail_image_pixels(&mut self, request: &ImageRequest) {
        self.renderer.fail_image_pixels(request);
    }

    pub fn retry_image(&mut self, request: &ImageRequest) {
        self.renderer.retry_image(request);
        if let Some(document) = &self.document {
            self.renderer.discover_image_metadata(document);
        }
        self.collect_image_requests();
        self.invalidate_active_artifacts();
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
    }

    pub fn trim_image_cache(&mut self) {
        self.renderer.trim_image_cache();
        self.request_redraw();
    }

    pub fn set_layout_artifact_cache_limit(&mut self, bytes: usize) {
        self.artifact_cache.set_limit(bytes);
    }

    pub fn resize(&mut self, size: (u32, u32)) {
        self.dispatch(Action::Resize(size));
    }

    fn reduce_resize(&mut self, size: (u32, u32)) -> ActionResult {
        if self.size == size {
            return ActionResult::default();
        }
        self.capture_anchor();
        self.size = size;
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
        ActionResult { value: true }
    }

    pub fn continuous_scroll_metrics(&self) -> Option<(f32, f32)> {
        (self.reading_mode == ReadingMode::Continuous && self.document.is_some())
            .then_some((self.scroll_y, self.content_height))
    }

    /// Returns the active immutable semantic document.
    pub fn active_document(&self) -> Option<&FlowDocument> {
        self.document
            .as_ref()
            .filter(|_| self.document_error.is_none())
    }

    #[doc(hidden)]
    pub fn persistent_content_width(&self) -> Option<f32> {
        self.active_document()?;
        Some(self.renderer.effective_content_width(self.size.0))
    }

    #[doc(hidden)]
    pub fn persistent_layout_digest(&self) -> [u8; 32] {
        self.renderer.persistent_layout_digest()
    }

    #[doc(hidden)]
    pub fn persistent_layout_is_cacheable(&self) -> bool {
        self.active_document().is_some_and(|document| {
            document.resources().all(|(_, resource)| {
                !resource
                    .media_type
                    .as_deref()
                    .is_some_and(|media_type| media_type.starts_with("image/"))
            }) && std::iter::successors(Some(document.root()), |node| document.following(*node))
                .all(|node| {
                    !matches!(
                        document.get_node(node).map(|node| &node.kind),
                        Some(crate::model::flow::NodeKind::Image(_))
                    )
                })
        })
    }

    #[doc(hidden)]
    pub fn install_persistent_composition(
        &mut self,
        content_width: f32,
        composition: ComposedDocument,
    ) -> bool {
        if composition.width.to_bits() != content_width.to_bits() {
            return false;
        }
        let Some(semantic) = self.semantic_key else {
            return false;
        };
        let key = CompositionKey {
            semantic,
            environment: self.renderer.environment_key(),
            renderer_revision: self.renderer.layout_revision(),
            content_width: content_width.to_bits(),
        };
        self.artifact_cache
            .insert_composition(key, Arc::new(composition));
        true
    }

    #[doc(hidden)]
    pub fn install_persistent_pagination(
        &mut self,
        viewport_height: u32,
        pages: PaginatedLayout,
    ) -> bool {
        let Some(composition) = self.composition_key else {
            return false;
        };
        let key = PaginationKey {
            composition,
            body_height: self
                .renderer
                .paginated_body_height(viewport_height)
                .to_bits(),
        };
        self.artifact_cache.insert_pagination(key, Arc::new(pages));
        true
    }

    #[doc(hidden)]
    pub fn prepare_active_artifacts(&mut self) -> anyhow::Result<()> {
        let Some(content_width) = self.persistent_content_width() else {
            return Ok(());
        };
        self.ensure_layout_artifacts(content_width);
        if self.reading_mode == ReadingMode::Paginated {
            self.rebuild_paginated_layout(self.size.1)?;
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn active_composition(&self) -> Option<&ComposedDocument> {
        self.current_composition_key()
            .map(|_| self.composed.as_ref())
    }

    #[doc(hidden)]
    pub fn active_pagination(&self) -> Option<&PaginatedLayout> {
        self.paginated_layout()
    }

    /// Builds pagination without painting, settles any pending text anchor, and
    /// returns one-based page metrics.
    /// Hosts use this to index independently loaded publication items with the
    /// same viewport and typography as the active viewer.
    pub fn prepare_pagination(&mut self, width: u32, height: u32) -> Option<(usize, usize)> {
        if self.reading_mode != ReadingMode::Paginated || self.document_error.is_some() {
            return None;
        }
        self.document.as_ref()?;
        self.size = (width.max(1), height.max(1));
        self.renderer.set_font_size(self.font_size_points);
        let content_width = self.renderer.effective_content_width(self.size.0);
        self.ensure_layout_artifacts(content_width)?;
        if let Err(error) = self.rebuild_paginated_layout(self.size.1) {
            self.document_error = Some(format!("pagination failed: {error}"));
            return None;
        }
        // Preparing pagination must settle the same pending text anchor as
        // painting, so a host can turn the next page before the first redraw.
        if let Some(anchor) = self.page_anchor.take() {
            self.current_page = self.pages.page_for_text(anchor);
        } else {
            self.current_page = self
                .current_page
                .min(self.pages.pages.len().saturating_sub(1));
        }
        self.paginated_page_metrics()
    }

    fn ensure_layout_artifacts(&mut self, content_width: f32) -> Option<()> {
        let document = self.document.as_ref()?;
        let composition_key = CompositionKey {
            semantic: self.semantic_key?,
            environment: self.renderer.environment_key(),
            renderer_revision: self.renderer.layout_revision(),
            content_width: content_width.to_bits(),
        };
        if self.composition_key == Some(composition_key)
            && self.continuous_key
                == Some(ContinuousPlacementKey {
                    composition: composition_key,
                })
        {
            return Some(());
        }

        let _span = crate::telemetry::phase_span!(
            "flow.viewer.layout",
            content_width = content_width as f64,
        );
        let composed = if let Some(composed) = self.artifact_cache.composition(composition_key) {
            composed
        } else {
            let composed = Arc::new(self.renderer.compose_document(document, content_width));
            self.artifact_cache
                .insert_composition(composition_key, Arc::clone(&composed));
            composed
        };
        let continuous_key = ContinuousPlacementKey {
            composition: composition_key,
        };
        let layout = if let Some(layout) = self.artifact_cache.continuous(continuous_key) {
            layout
        } else {
            let layout = Arc::new(composed.continuous_layout());
            self.artifact_cache
                .insert_continuous(continuous_key, Arc::clone(&layout));
            layout
        };
        self.composed = composed;
        self.layout = layout;
        self.navigation = NavigationIndex::new(document, &self.layout);
        self.composition_key = Some(composition_key);
        self.continuous_key = Some(continuous_key);
        self.pagination_key = None;
        self.pages = Arc::new(PaginatedLayout::default());
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        Some(())
    }

    fn rebuild_paginated_layout(&mut self, viewport_height: u32) -> anyhow::Result<()> {
        let composition = self
            .composition_key
            .ok_or_else(|| anyhow::anyhow!("composition is not prepared"))?;
        let key = PaginationKey {
            composition,
            body_height: self
                .renderer
                .paginated_body_height(viewport_height)
                .to_bits(),
        };
        if self.pagination_key == Some(key) {
            return Ok(());
        }
        self.pages = if let Some(pages) = self.artifact_cache.pagination(key) {
            pages
        } else {
            let pages = Arc::new(
                self.renderer
                    .paginate_composed_document(&self.composed, viewport_height)?,
            );
            self.artifact_cache
                .insert_pagination(key, Arc::clone(&pages));
            pages
        };
        self.pagination_key = Some(key);
        Ok(())
    }

    /// Returns the one-based current page and total page count in paginated mode.
    pub fn paginated_page_metrics(&self) -> Option<(usize, usize)> {
        (self.reading_mode == ReadingMode::Paginated && self.document.is_some()).then(|| {
            let total = self.pages.pages.len().max(1);
            (self.current_page.min(total - 1) + 1, total)
        })
    }

    pub fn set_continuous_scroll_position(&mut self, position: f32) {
        self.dispatch(Action::SetContinuousScrollPosition(position));
    }

    fn reduce_set_continuous_scroll_position(&mut self, position: f32) -> ActionResult {
        if !position.is_finite() || self.continuous_scroll_metrics().is_none() {
            return ActionResult::default();
        }
        let previous_position = self.scroll_y;
        let previous_hover = self.hovered_link.clone();
        self.scroll_y = position;
        self.clamp_scroll();
        self.refresh_hover();
        if self.hovered_link != previous_hover {
            self.request_redraw();
        }
        ActionResult {
            value: self.scroll_y != previous_position,
        }
    }

    pub fn render_settings(&self) -> &crate::settings::RenderSettings {
        self.renderer.settings()
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        if self.font_size_points.to_bits() == font_size.to_bits() {
            return;
        }
        self.capture_anchor();
        self.font_size_points = font_size;
        self.renderer.set_font_size(font_size);
        self.invalidate_active_artifacts();
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
    }

    pub fn set_max_width(&mut self, max_width: f32) {
        if self.max_width_points.to_bits() == max_width.to_bits() {
            return;
        }
        self.capture_anchor();
        self.max_width_points = max_width;
        self.renderer.set_max_width(max_width);
        self.invalidate_active_artifacts();
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
    }

    pub fn set_dark_mode(&mut self, dark_mode: bool) {
        if self.dark_mode == dark_mode {
            return;
        }
        self.dark_mode = dark_mode;
        self.renderer.set_dark_mode(dark_mode);
        self.request_redraw();
    }

    pub fn set_rasterization(&mut self, options: crate::text::RasterizationOptions) {
        self.renderer.set_rasterization(options);
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.request_redraw();
    }

    pub fn set_settings(&mut self, settings: Settings) {
        let layout_changed = !self
            .renderer
            .settings()
            .layout_equivalent(&settings.rendering)
            || self.reading_mode != settings.reading_mode;
        if layout_changed && self.page_anchor.is_none() {
            self.capture_anchor();
        }
        self.font_size_points = settings.font_size;
        self.max_width_points = settings.max_width;
        self.dark_mode = settings.dark_mode;
        self.reading_mode = settings.reading_mode;
        self.renderer.set_settings(settings.into());
        if layout_changed {
            self.invalidate_active_artifacts();
            self.revisions.layout = self.revisions.layout.wrapping_add(1);
        }
        self.hovered_link = None;
        self.request_redraw();
    }

    pub fn set_reading_mode(&mut self, mode: ReadingMode) {
        self.dispatch(Action::SetReadingMode(mode));
    }

    fn reduce_set_reading_mode(&mut self, mode: ReadingMode) -> ActionResult {
        if self.reading_mode == mode {
            return ActionResult::default();
        }
        self.capture_anchor();
        self.reading_mode = mode;
        self.revisions.layout = self.revisions.layout.wrapping_add(1);
        self.hovered_link = None;
        self.request_redraw();
        ActionResult { value: true }
    }

    pub fn wheel(&mut self, delta: f32) {
        self.dispatch(Action::Wheel(delta));
    }

    fn reduce_wheel(&mut self, delta: f32) -> ActionResult {
        if delta != 0.0 {
            self.pending_link_activation = None;
        }
        if self.reading_mode == ReadingMode::Paginated {
            self.page_wheel_accumulator += delta;
            if self.page_wheel_accumulator.abs() < PAGE_WHEEL_THRESHOLD {
                return ActionResult::default();
            }
            let direction = self.page_wheel_accumulator.signum();
            self.page_wheel_accumulator = 0.0;
            return self.reduce_scroll_by(direction);
        }
        self.reduce_scroll_by(delta)
    }

    pub fn scroll_by(&mut self, delta: f32) -> bool {
        self.dispatch(Action::ScrollBy(delta)).value
    }

    fn reduce_scroll_by(&mut self, delta: f32) -> ActionResult {
        if self.reading_mode == ReadingMode::Paginated {
            let last = self.pages.pages.len().saturating_sub(1);
            let next = self
                .current_page
                .saturating_add_signed(if delta < 0.0 { -1 } else { 1 })
                .min(last);
            if next != self.current_page {
                self.current_page = next;
                self.page_anchor = None;
                self.refresh_hover();
                self.request_redraw();
                return ActionResult { value: true };
            }
            return ActionResult::default();
        }
        let previous = self.scroll_y;
        self.scroll_y += delta;
        self.clamp_scroll();
        if self.scroll_y != previous {
            self.refresh_hover();
            self.request_redraw();
            return ActionResult { value: true };
        }
        ActionResult::default()
    }

    pub fn scroll_to(&mut self, position: f32) {
        self.dispatch(Action::ScrollTo(position));
    }

    fn reduce_scroll_to(&mut self, position: f32) -> ActionResult {
        if self.reading_mode == ReadingMode::Paginated {
            let next = if position == f32::MAX {
                self.pages.pages.len().saturating_sub(1)
            } else {
                self.pages.page_for_y(position.max(0.0))
            };
            if next != self.current_page {
                self.current_page = next;
                self.page_anchor = None;
                self.refresh_hover();
                self.request_redraw();
                return ActionResult { value: true };
            }
            return ActionResult::default();
        }
        let previous = self.scroll_y;
        self.scroll_y = position;
        self.clamp_scroll();
        if self.scroll_y != previous {
            self.refresh_hover();
            self.request_redraw();
            return ActionResult { value: true };
        }
        ActionResult::default()
    }

    pub fn focus_adjacent(&mut self, direction: isize, headings_only: bool) -> bool {
        self.dispatch(Action::FocusAdjacent {
            direction,
            headings_only,
        })
        .value
    }

    fn reduce_focus_adjacent(&mut self, direction: isize, headings_only: bool) -> ActionResult {
        let Some(target) = self
            .navigation
            .adjacent(self.focused.as_ref(), direction, headings_only)
            .cloned()
        else {
            return ActionResult::default();
        };
        self.focused = Some(target.identity());
        self.reveal_text(target.range);
        ActionResult { value: true }
    }

    pub fn toggle_visual_selection(&mut self) {
        self.dispatch(Action::ToggleVisualSelection);
    }

    fn reduce_toggle_visual_selection(&mut self) -> ActionResult {
        if self.visual_selection {
            self.visual_selection = false;
            self.request_redraw();
            return ActionResult { value: true };
        }
        if self.selection.is_none() {
            let offset = self
                .focused
                .as_ref()
                .and_then(|identity| self.navigation.target(identity))
                .map(|target| target.range.start)
                .or_else(|| {
                    self.active_match
                        .and_then(|index| self.search_matches.get(index))
                        .map(|source| source.start)
                })
                .or_else(|| self.position().map(|position| position.text_anchor));
            let Some(offset) = offset else {
                return ActionResult::default();
            };
            self.selection = Some(Selection::collapsed(TextPosition::new(offset)));
        }
        self.visual_selection = true;
        self.selection_preferred_x = self
            .selection
            .and_then(|selection| self.renderer.caret_x(&self.layout, selection.active));
        self.request_redraw();
        ActionResult { value: true }
    }

    pub fn move_visual_selection(&mut self, direction: isize, vertical: bool) {
        self.dispatch(Action::MoveVisualSelection {
            direction,
            vertical,
        });
    }

    fn reduce_move_visual_selection(&mut self, direction: isize, vertical: bool) -> ActionResult {
        if !self.visual_selection {
            return ActionResult::default();
        }
        let Some(selection) = &mut self.selection else {
            return ActionResult::default();
        };
        let next = if vertical {
            let preferred_x = self
                .selection_preferred_x
                .or_else(|| self.renderer.caret_x(&self.layout, selection.active));
            preferred_x.and_then(|x| {
                self.renderer
                    .vertical_text_position(&self.layout, selection.active, direction, x)
            })
        } else {
            self.renderer
                .adjacent_text_position(&self.layout, selection.active, direction)
        };
        let Some(next) = next else {
            return ActionResult::default();
        };
        selection.active = next;
        if !vertical {
            self.selection_preferred_x = self.renderer.caret_x(&self.layout, next);
        }
        self.reveal_text(TextRange {
            start: next.offset,
            end: next.offset.saturating_add(1),
        });
        ActionResult { value: true }
    }

    pub fn clear_selection(&mut self) {
        self.dispatch(Action::ClearSelection);
    }

    fn reduce_clear_selection(&mut self) -> ActionResult {
        if self.visual_selection {
            self.visual_selection = false;
        } else if self.selection.take().is_none() {
            return ActionResult::default();
        }
        self.selection_recovery = None;
        self.selection_preferred_x = None;
        self.request_redraw();
        ActionResult { value: true }
    }

    pub fn restore_position(&mut self, position: ViewerPosition) {
        self.dispatch(Action::RestorePosition(position));
    }

    fn reduce_restore_position(&mut self, position: ViewerPosition) -> ActionResult {
        self.focused = position
            .focus
            .filter(|identity| self.navigation.target(identity).is_some());
        self.active_match = position
            .search_match
            .filter(|index| *index < self.search_matches.len());
        if self.reading_mode == ReadingMode::Paginated {
            self.current_page = self.pages.page_for_text(position.text_anchor);
            self.page_anchor = Some(position.text_anchor);
        } else if let Some(y) = self.layout.y_for_text(position.text_anchor) {
            self.scroll_y = y;
            self.clamp_scroll();
        } else {
            self.page_anchor = Some(position.text_anchor);
        }
        self.refresh_hover();
        self.request_redraw();
        ActionResult { value: true }
    }

    pub fn set_search_query(&mut self, query: impl Into<String>) {
        self.set_search_query_near(query, None);
    }

    pub fn set_search_query_near(
        &mut self,
        query: impl Into<String>,
        origin: Option<&ViewerPosition>,
    ) {
        self.dispatch(Action::SetSearchQuery {
            query: query.into(),
            origin: origin.cloned(),
        });
    }

    fn reduce_set_search_query(
        &mut self,
        query: String,
        origin: Option<&ViewerPosition>,
    ) -> ActionResult {
        self.confirmed_query = query;
        self.search_matches = self.search_index.find(&self.confirmed_query);
        let anchor = origin.map_or(0, |position| position.text_anchor);
        self.active_match = if self.search_matches.is_empty() {
            None
        } else {
            Some(
                self.search_matches
                    .partition_point(|source| source.start < anchor)
                    .min(self.search_matches.len() - 1),
            )
        };
        if let Some(source) = self.search_matches.first().copied() {
            self.reveal_text(source);
        } else {
            self.request_redraw();
        }
        ActionResult { value: true }
    }

    pub fn move_to_search_match(&mut self, reverse: bool) -> bool {
        self.dispatch(Action::MoveToSearchMatch { reverse }).value
    }

    fn reduce_move_to_search_match(&mut self, reverse: bool) -> ActionResult {
        if self.search_matches.is_empty() {
            return ActionResult::default();
        }
        self.active_match = Some(match self.active_match {
            Some(0) if reverse => self.search_matches.len() - 1,
            Some(index) if reverse => index - 1,
            Some(index) => (index + 1) % self.search_matches.len(),
            None if reverse => self.search_matches.len() - 1,
            None => 0,
        });
        let source = self.search_matches[self.active_match.expect("active search match")];
        self.reveal_text(source);
        ActionResult { value: true }
    }

    pub fn navigate_to_heading(&mut self, identity: &NavigationIdentity) -> bool {
        self.dispatch(Action::NavigateToHeading(identity.clone()))
            .value
    }

    fn reduce_navigate_to_heading(&mut self, identity: &NavigationIdentity) -> ActionResult {
        let Some(target) = self.navigation.target(identity).cloned() else {
            return ActionResult::default();
        };
        if target.kind != NavigationKind::Heading {
            return ActionResult::default();
        }
        if let Some(location) = self.position() {
            self.history.push(location);
        }
        self.focused = Some(target.identity());
        self.reveal_text(target.range);
        ActionResult { value: true }
    }

    pub fn activate_focused(&mut self) -> bool {
        self.dispatch(Action::ActivateFocused).value
    }

    fn reduce_activate_focused(&mut self) -> ActionResult {
        let activation = self
            .focused
            .as_ref()
            .and_then(|identity| self.navigation.target(identity))
            .and_then(|target| {
                target
                    .generated_action()
                    .cloned()
                    .map(ViewerActivation::Generated)
                    .or_else(|| {
                        target.flow_destination().cloned().map(|destination| {
                            ViewerActivation::Destination {
                                destination,
                                relation: target.flow_relation().cloned(),
                                source: Some(Box::new(target.identity())),
                            }
                        })
                    })
            });
        let Some(activation) = activation else {
            return ActionResult::default();
        };
        self.reduce_activation(&activation)
    }

    pub fn activate_destination(&mut self, destination: flow::Destination) -> bool {
        self.dispatch(Action::ActivateDestination(destination))
            .value
    }

    pub fn activate_link(&mut self, destination: impl Into<String>) -> bool {
        self.dispatch(Action::ActivateDestination(destination_from_uri(
            destination.into(),
        )))
        .value
    }

    pub fn go_back(&mut self) -> bool {
        self.dispatch(Action::Back).value
    }

    fn reduce_go_back(&mut self) -> ActionResult {
        let Some(location) = self.history.pop() else {
            return ActionResult::default();
        };
        self.focused = location
            .focus
            .filter(|identity| self.navigation.target(identity).is_some());
        if self.reading_mode == ReadingMode::Paginated {
            self.current_page = self.pages.page_for_text(location.text_anchor);
            self.page_anchor = None;
        } else if let Some(y) = self.layout.y_for_text(location.text_anchor) {
            self.scroll_y = y;
            self.clamp_scroll();
        }
        self.refresh_hover();
        self.request_redraw();
        ActionResult { value: true }
    }

    pub fn active_heading(&self) -> Option<NavigationIdentity> {
        let anchor = match self.reading_mode {
            ReadingMode::Continuous => self.layout.text_offset_for_y(self.scroll_y),
            ReadingMode::Paginated => self
                .pages
                .pages
                .get(self.current_page)
                .map(|page| page.text_anchor),
        }?;
        let tree = self.outline();
        tree.section_for_text(anchor)
            .map(|index| tree.nodes()[index].identity.clone())
    }

    /// Prepares retained continuous layout without painting a viewport.
    pub fn prepare_continuous_scene(&mut self, width: u32, height: u32) -> bool {
        self.size = (width, height);
        if self.reading_mode != ReadingMode::Continuous || self.document.is_none() {
            return false;
        }
        self.renderer.set_font_size(self.font_size_points);
        let content_width = self.renderer.effective_content_width(width);
        if self.ensure_layout_artifacts(content_width).is_none() {
            return false;
        }
        let margin = self.renderer.settings().theme.page_margin;
        self.content_height = self.layout.height + margin * 2.0;
        if let Some(anchor) = self.page_anchor.take()
            && let Some(y) = self.layout.y_for_text(anchor)
        {
            self.scroll_y = y;
        }
        self.clamp_scroll();
        self.layout_x = ((width as f32 - self.layout.width) / 2.0).max(margin);
        true
    }

    pub fn render_continuous_region_into<T: PaintTarget>(
        &mut self,
        surface: &mut T,
        scene_width: u32,
        region_y: f32,
        region_height: f32,
        scale: f32,
    ) -> anyhow::Result<()> {
        if !self.prepare_continuous_scene(scene_width.max(1), self.size.1.max(1)) {
            anyhow::bail!("retained continuous scene is not available");
        }
        let interaction = self.interaction_state();
        let hovered = self
            .hovered_link
            .as_ref()
            .and_then(|identity| self.navigation.target(identity));
        self.layout_x = self.renderer.paint_document_region_into(
            surface,
            &self.layout,
            scene_width as f32,
            DocumentRegion {
                x: 0.0,
                y: region_y.max(0.0),
                width: scene_width as f32,
                height: region_height.max(1.0),
            },
            PaintOptions {
                scale,
                interaction: InteractionState {
                    hovered_target: hovered,
                    ..interaction
                },
            },
        )?;
        self.collect_image_requests();
        Ok(())
    }

    pub fn render_into<T: PaintTarget>(
        &mut self,
        surface: &mut T,
        width: u32,
        height: u32,
        scale: f32,
    ) -> anyhow::Result<()> {
        self.size = (width, height);
        self.renderer.set_font_size(self.font_size_points);
        if let Some(error) = self.document_error.clone() {
            self.content_height = height as f32;
            self.composed = Arc::new(ComposedDocument::default());
            self.layout = Arc::new(ContinuousLayout::default());
            self.pages = Arc::new(PaginatedLayout::default());
            self.invalidate_active_artifacts();
            self.renderer
                .paint_error_into(surface, &error, width, height, scale)?;
        } else if self.document.is_some() {
            let content_width = self.renderer.effective_content_width(width);
            self.ensure_layout_artifacts(content_width)
                .ok_or_else(|| anyhow::anyhow!("document layout is not available"))?;
            if self.reading_mode == ReadingMode::Paginated {
                self.rebuild_paginated_layout(height)?;
            }
            let focused = self
                .focused
                .as_ref()
                .and_then(|identity| self.navigation.target(identity));
            let active_match = self
                .active_match
                .and_then(|index| self.search_matches.get(index))
                .copied();
            let hovered = self
                .hovered_link
                .as_ref()
                .and_then(|identity| self.navigation.target(identity));
            let interaction = InteractionState {
                hovered_target: hovered,
                focused,
                search_matches: &self.search_matches,
                active_match,
                selection: self.selection,
            };
            if self.reading_mode == ReadingMode::Paginated {
                let selection = PageSelection {
                    requested_page: self.current_page,
                    text_anchor: self.page_anchor,
                };
                let (page_index, content_x) = self.renderer.paint_paginated_page_into(
                    surface,
                    &self.composed,
                    &self.pages,
                    (width, height),
                    selection,
                    PaintOptions { scale, interaction },
                )?;
                self.current_page = page_index;
                self.content_height = height as f32;
                self.scroll_y = 0.0;
                self.layout_x = content_x;
                self.page_anchor = None;
            } else {
                let anchor = self.page_anchor.take();
                self.content_height =
                    self.layout.height + self.renderer.settings().theme.page_margin * 2.0;
                if let Some(anchor) = anchor
                    && let Some(y) = self.layout.y_for_text(anchor)
                {
                    self.scroll_y = y;
                }
                let maximum_scroll = (self.content_height - self.size.1 as f32).max(0.0);
                self.scroll_y = self.scroll_y.clamp(0.0, maximum_scroll);
                let content_x = self.renderer.paint_document_region_into(
                    surface,
                    &self.layout,
                    width as f32,
                    DocumentRegion {
                        x: 0.0,
                        y: self.scroll_y,
                        width: width as f32,
                        height: height as f32,
                    },
                    PaintOptions { scale, interaction },
                )?;
                let content_height =
                    self.layout.height + self.renderer.settings().theme.page_margin * 2.0;
                self.content_height = content_height;
                self.layout_x = content_x;
            }
        } else {
            self.renderer
                .paint_error_into(surface, "document is empty", width, height, scale)?;
        }
        self.collect_image_requests();
        Ok(())
    }

    pub fn pointer_button(&mut self, input: PointerButtonInput, now: Duration) {
        self.dispatch(Action::PointerButton { input, now });
    }

    fn reduce_pointer_button(&mut self, input: PointerButtonInput, now: Duration) -> ActionResult {
        if input.pressed {
            self.begin_pointer_selection(input.click_count);
        } else {
            self.finish_pointer_selection(now);
        }
        ActionResult { value: true }
    }

    pub fn pointer_moved(&mut self, position: Option<(f32, f32)>, now: Duration) {
        self.dispatch(Action::PointerMoved { position, now });
    }

    fn reduce_pointer_moved(
        &mut self,
        position: Option<(f32, f32)>,
        now: Duration,
    ) -> ActionResult {
        let previous_position = self.pointer_position;
        self.pointer_position = position;
        let previous = self.hovered_link.clone();
        let mut selection_changed = false;
        if let (Some((pointer_x, pointer_y)), Some(drag)) = (position, &mut self.selection_drag)
            && !drag.moved
            && ((pointer_x - drag.origin.0).powi(2) + (pointer_y - drag.origin.1).powi(2)).sqrt()
                >= POINTER_DRAG_THRESHOLD
        {
            drag.moved = true;
        }
        if let Some((pointer_x, pointer_y)) = position
            && self.selection_drag.as_ref().is_some_and(|drag| drag.moved)
            && let Some(active) = self.text_position_at_pointer(pointer_x, pointer_y)
        {
            selection_changed |= self.extend_pointer_selection(active);
        }
        if let Some((pointer_x, pointer_y)) = position
            && self.selection_drag.as_ref().is_some_and(|drag| drag.moved)
            && self.reading_mode == ReadingMode::Continuous
        {
            let edge = 32.0;
            let delta = if pointer_y < edge {
                pointer_y - edge
            } else if pointer_y > self.size.1 as f32 - edge {
                pointer_y - (self.size.1 as f32 - edge)
            } else {
                0.0
            };
            if delta != 0.0 {
                let previous_scroll = self.scroll_y;
                self.scroll_y += delta.clamp(-24.0, 24.0);
                self.clamp_scroll();
                if self.scroll_y != previous_scroll {
                    selection_changed = true;
                    if let Some(active) = self.text_position_at_pointer(pointer_x, pointer_y) {
                        self.extend_pointer_selection(active);
                    }
                }
            }
        }
        if let Some((pointer_x, pointer_y)) = position
            && self.selection_drag.as_ref().is_some_and(|drag| drag.moved)
            && self.reading_mode == ReadingMode::Paginated
        {
            let edge = 32.0;
            let direction = if pointer_y < edge {
                Some(-1)
            } else if pointer_y > self.size.1 as f32 - edge {
                Some(1)
            } else {
                None
            };
            let ready = self.last_selection_page_turn.is_none_or(|last_turn| {
                now.saturating_sub(last_turn) >= Duration::from_millis(350)
            });
            if let Some(direction) = direction
                && ready
            {
                let last = self.pages.pages.len().saturating_sub(1);
                let next = self.current_page.saturating_add_signed(direction).min(last);
                if next != self.current_page {
                    self.current_page = next;
                    self.last_selection_page_turn = Some(now);
                    selection_changed = true;
                    let margin = self.renderer.settings().theme.page_margin;
                    let clamped_y =
                        pointer_y.clamp(margin + 1.0, self.size.1 as f32 - margin - 1.0);
                    if let Some(active) = self.text_position_at_pointer(pointer_x, clamped_y) {
                        self.extend_pointer_selection(active);
                    }
                }
            }
        }
        self.refresh_hover();
        if selection_changed || self.hovered_link != previous {
            self.request_redraw();
        }
        ActionResult {
            value: selection_changed
                || self.hovered_link != previous
                || self.pointer_position != previous_position,
        }
    }

    pub fn copy_selection(&mut self) -> bool {
        self.dispatch(Action::CopySelection).value
    }

    fn reduce_copy_selection(&mut self) -> ActionResult {
        let Some(selection) = self.selection else {
            return ActionResult::default();
        };
        let Some(document) = self.document.as_ref() else {
            return ActionResult::default();
        };
        let text = selected_text(document, selection).unwrap_or_default();
        if text.is_empty() {
            return ActionResult::default();
        }
        self.effects.push(ViewerEffect::SetClipboard(text));
        self.visual_selection = false;
        self.selection_preferred_x = None;
        self.request_redraw();
        ActionResult { value: true }
    }

    fn begin_pointer_selection(&mut self, click_count: u8) {
        self.pending_link_activation = None;
        let Some((pointer_x, pointer_y)) = self.pointer_position else {
            return;
        };
        let Some(position) = self.text_position_at_pointer(pointer_x, pointer_y) else {
            self.selection_recovery = None;
            self.selection = None;
            self.visual_selection = false;
            self.request_redraw();
            return;
        };
        let activation = self
            .navigation_target_at_pointer(pointer_x, pointer_y)
            .and_then(|target| {
                target
                    .generated_action()
                    .cloned()
                    .map(ViewerActivation::Generated)
                    .or_else(|| {
                        target.flow_destination().cloned().map(|destination| {
                            ViewerActivation::Destination {
                                destination,
                                relation: target.flow_relation().cloned(),
                                source: Some(Box::new(target.identity())),
                            }
                        })
                    })
            });
        let granularity = match click_count {
            0 | 1 => SelectionGranularity::Character,
            2 => SelectionGranularity::Word,
            _ => SelectionGranularity::Block,
        };
        let Some(base) = self
            .document
            .as_ref()
            .and_then(|document| selection_at(document, position, granularity))
        else {
            self.selection = None;
            self.selection_drag = None;
            self.request_redraw();
            return;
        };
        self.selection_recovery = None;
        self.selection = Some(base);
        self.visual_selection = false;
        self.selection_preferred_x = None;
        self.last_selection_page_turn = None;
        self.selection_drag = Some(SelectionDrag {
            origin: (pointer_x, pointer_y),
            activation,
            moved: false,
            granularity,
            base,
        });
    }

    fn finish_pointer_selection(&mut self, now: Duration) {
        let Some(drag) = self.selection_drag.take() else {
            return;
        };
        self.last_selection_page_turn = None;
        if !drag.moved && drag.granularity == SelectionGranularity::Character {
            self.selection = None;
            if let Some(activation) = drag.activation {
                self.defer_link_activation(activation, now);
            } else {
                self.request_redraw();
            }
        } else {
            self.request_redraw();
        }
    }

    #[doc(hidden)]
    pub fn extend_pointer_selection(&mut self, position: TextPosition) -> bool {
        let Some(drag) = self.selection_drag.as_ref() else {
            return false;
        };
        let next = match drag.granularity {
            SelectionGranularity::Character => Selection {
                anchor: drag.base.anchor,
                active: position,
            },
            granularity => {
                let Some(document) = self.document.as_ref() else {
                    return false;
                };
                let Some(unit) = selection_at(document, position, granularity) else {
                    return false;
                };
                let base = drag.base.range();
                let unit = unit.range();
                if unit.start < base.start {
                    Selection {
                        anchor: TextPosition::new(base.end),
                        active: TextPosition::new(unit.start),
                    }
                } else {
                    Selection {
                        anchor: TextPosition::new(base.start),
                        active: TextPosition::new(unit.end),
                    }
                }
            }
        };
        if self.selection == Some(next) {
            return false;
        }
        self.selection = Some(next);
        true
    }

    /// Reports whether a tap at viewport coordinates activates a document link
    /// or generated action. Platform tap zones use this to give content
    /// activation precedence over page turns.
    pub fn has_activation_at(&self, pointer_x: f32, pointer_y: f32) -> bool {
        self.navigation_target_at_pointer(pointer_x, pointer_y)
            .is_some_and(|target| {
                target.generated_action().is_some() || target.flow_destination().is_some()
            })
    }

    fn text_position_at_pointer(&self, pointer_x: f32, pointer_y: f32) -> Option<TextPosition> {
        if self.reading_mode == ReadingMode::Paginated {
            let margin = self.renderer.settings().theme.page_margin;
            if pointer_y < margin || pointer_y >= self.size.1 as f32 - margin {
                return None;
            }
            return self.renderer.text_position_at_paginated(
                &self.composed,
                &self.pages,
                self.current_page,
                pointer_x - self.layout_x,
                pointer_y - margin,
            );
        }
        let (document_x, document_y) = self.document_coordinates(pointer_x, pointer_y)?;
        self.renderer
            .text_position_at(&self.layout, document_x, document_y)
    }

    fn navigation_target_at_pointer(
        &self,
        pointer_x: f32,
        pointer_y: f32,
    ) -> Option<&crate::navigation::NavigationTarget> {
        if self.reading_mode == ReadingMode::Paginated {
            let margin = self.renderer.settings().theme.page_margin;
            if pointer_y < margin || pointer_y >= self.size.1 as f32 - margin {
                return None;
            }
            return self.navigation.flow_target_at_paginated_point(
                &self.composed,
                &self.pages,
                self.current_page,
                pointer_x - self.layout_x,
                pointer_y - margin,
            );
        }
        let (document_x, document_y) = self.document_coordinates(pointer_x, pointer_y)?;
        self.navigation
            .flow_target_at_point(&self.layout, document_x, document_y)
    }

    fn document_coordinates(&self, pointer_x: f32, pointer_y: f32) -> Option<(f32, f32)> {
        let margin = self.renderer.settings().theme.page_margin;
        if self.reading_mode == ReadingMode::Paginated {
            let page = self.pages.pages.get(self.current_page)?;
            if pointer_y < margin || pointer_y >= self.size.1 as f32 - margin {
                return None;
            }
            let document_y = page.document_y_at(pointer_y - margin)?;
            Some((pointer_x - self.layout_x, document_y))
        } else {
            Some((
                pointer_x - self.layout_x,
                pointer_y - margin + self.scroll_y,
            ))
        }
    }

    fn defer_link_activation(&mut self, activation: ViewerActivation, now: Duration) {
        let focus = self
            .pointer_position
            .and_then(|(pointer_x, pointer_y)| {
                self.navigation_target_at_pointer(pointer_x, pointer_y)
            })
            .map(|target| target.identity());
        self.pending_link_activation = Some(PendingLinkActivation {
            activation,
            focus,
            deadline: now.saturating_add(LINK_ACTIVATION_DELAY),
        });
        self.request_redraw();
    }

    fn reduce_activation(&mut self, activation: &ViewerActivation) -> ActionResult {
        match activation {
            ViewerActivation::Destination {
                destination,
                relation,
                source,
            } => {
                self.reduce_activate_destination(destination, relation.as_ref(), source.as_deref())
            }
            ViewerActivation::Generated(action) => self.reduce_generated_action(action),
        }
    }

    fn reduce_activate_destination(
        &mut self,
        destination: &flow::Destination,
        relation: Option<&flow::LinkRelation>,
        source: Option<&NavigationIdentity>,
    ) -> ActionResult {
        let local_anchor = local_flow_anchor(destination, &self.document_name);
        if let Some(anchor) = local_anchor {
            if let Some(location) = self.position() {
                if matches!(relation, Some(flow::LinkRelation::NoteReference { .. })) {
                    self.note_return = Some(NoteReturnContext {
                        definition: anchor.clone(),
                        position: location.clone(),
                        focus: source.cloned(),
                    });
                }
                self.history.push(location);
            }
            self.navigate_to_anchor(anchor);
        } else {
            self.effects
                .push(ViewerEffect::FollowLink(destination.clone()));
            self.request_redraw();
        }
        ActionResult { value: true }
    }

    fn reduce_generated_action(&mut self, action: &GeneratedAction) -> ActionResult {
        let GeneratedAction::NoteReturn {
            definition,
            fallback,
        } = action;
        let exact = self.note_return.as_ref().and_then(|context| {
            self.document
                .as_ref()
                .and_then(|document| document.anchor(&context.definition))
                .filter(|anchor| anchor.target.node == *definition)
                .map(|_| (context.position.clone(), context.focus.clone()))
        });
        if let Some((position, focus)) = exact {
            self.history.pop();
            let result = self.reduce_restore_position(position);
            if let Some(focus) = focus
                && let Some(target) = self.navigation.target(&focus).cloned()
            {
                self.focused = Some(target.identity());
                self.reveal_text(target.range);
            }
            return result;
        }
        self.reduce_activate_destination(&flow::Destination::Local(fallback.clone()), None, None)
    }

    /// Navigates to a local fragment without adding an in-document history entry.
    pub fn navigate_to_fragment(&mut self, fragment: &str) -> bool {
        if !fragment.is_empty()
            && self
                .document
                .as_ref()
                .and_then(|document| document.anchor(&flow::AnchorId::new(fragment)))
                .is_none()
        {
            return false;
        }
        self.navigate_to_anchor(&flow::AnchorId::new(fragment));
        true
    }

    fn navigate_to_anchor(&mut self, anchor: &flow::AnchorId) {
        if anchor.as_str().is_empty() {
            self.scroll_y = 0.0;
            self.current_page = 0;
            self.request_redraw();
            return;
        }
        let Some(target) = self
            .document
            .as_ref()
            .and_then(|document| document.anchor(anchor))
        else {
            return;
        };
        let navigation = self.navigation.flow_target_for_anchor(anchor);
        let document_y = navigation.and_then(|target| target.document_y).or_else(|| {
            self.layout
                .y_for_text(target.target.position.offset.as_usize())
        });
        if let Some(navigation) = navigation {
            self.focused = Some(navigation.identity());
        }
        let offset = target.target.position.offset.as_usize();
        if document_y.is_none() {
            self.page_anchor = Some(offset);
        }
        self.reveal_flow_position(offset, document_y);
    }

    fn interaction_state(&self) -> InteractionState<'_> {
        let focused = self
            .focused
            .as_ref()
            .and_then(|identity| self.navigation.target(identity));
        let active_match = self
            .active_match
            .and_then(|index| self.search_matches.get(index))
            .copied();
        let hovered_target = self
            .hovered_link
            .as_ref()
            .and_then(|identity| self.navigation.target(identity));
        InteractionState {
            hovered_target,
            focused,
            search_matches: &self.search_matches,
            active_match,
            selection: self.selection,
        }
    }

    pub fn position(&self) -> Option<ViewerPosition> {
        let text_anchor = match self.reading_mode {
            ReadingMode::Continuous => self.layout.text_offset_for_y(self.scroll_y),
            ReadingMode::Paginated => self
                .pages
                .pages
                .get(self.current_page)
                .map(|page| page.text_anchor),
        }?;
        Some(ViewerPosition {
            text_anchor,
            focus: self.focused.clone(),
            search_match: self.active_match,
        })
    }

    pub fn stable_position(&self) -> Option<StableTextPosition> {
        let offset = self.position()?.text_anchor;
        let text = self.document.as_ref()?.text().as_str();
        Some(StableTextPosition::capture(text, offset))
    }

    /// Restores a durable position. When the source revision changed, the
    /// canonical context must identify a recovery point; an unchanged revision
    /// may use the exact offset directly.
    pub fn restore_stable_position(
        &mut self,
        position: &StableTextPosition,
        same_revision: bool,
    ) -> bool {
        let Some(text) = self
            .document
            .as_ref()
            .map(|document| document.text().as_str())
        else {
            return false;
        };
        let Some(offset) = position.recover_offset(text, same_revision) else {
            return false;
        };
        self.restore_position(ViewerPosition {
            text_anchor: offset,
            focus: None,
            search_match: None,
        });
        true
    }

    fn capture_anchor(&mut self) {
        self.page_anchor = match self.reading_mode {
            ReadingMode::Continuous => self.layout.text_offset_for_y(self.scroll_y),
            ReadingMode::Paginated => self
                .pages
                .pages
                .get(self.current_page)
                .map(|page| page.text_anchor),
        };
    }

    fn clamp_scroll(&mut self) {
        let maximum = (self.content_height - self.size.1 as f32).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, maximum);
    }

    fn refresh_hover(&mut self) {
        self.hovered_link = self
            .pointer_position
            .and_then(|(pointer_x, pointer_y)| {
                self.navigation_target_at_pointer(pointer_x, pointer_y)
            })
            .map(|target| target.identity());
    }

    fn reveal_flow_position(&mut self, _offset: usize, document_y: Option<f32>) {
        let Some(y) = document_y else {
            self.request_redraw();
            return;
        };
        if self.reading_mode == ReadingMode::Paginated {
            self.current_page = self.pages.page_for_y(y);
            self.page_anchor = None;
        } else {
            let context = self.font_size_points * 2.0;
            self.scroll_y = (y - context).max(0.0);
            self.clamp_scroll();
        }
        self.refresh_hover();
        self.request_redraw();
    }

    fn reveal_text(&mut self, range: TextRange) {
        let Some(y) = self.layout.y_for_text(range.start) else {
            self.request_redraw();
            return;
        };
        if self.reading_mode == ReadingMode::Paginated {
            self.current_page = self.pages.page_for_text(range.start);
            self.page_anchor = None;
        } else {
            let margin = self.renderer.settings().theme.page_margin;
            let context = self.font_size_points * 2.0;
            let visible_top = self.scroll_y;
            let visible_bottom = self.scroll_y + (self.size.1 as f32 - margin * 2.0).max(1.0);
            let bottom = self
                .layout
                .blocks
                .iter()
                .find(|block| block.range.contains(range.start) || block.range.start == range.start)
                .map_or(y, |block| (block.y + block.height).min(y + context * 2.0));
            if y < visible_top + context {
                self.scroll_y = (y - context).max(0.0);
            } else if bottom > visible_bottom - context {
                self.scroll_y += bottom - (visible_bottom - context);
                self.clamp_scroll();
            }
        }
        self.refresh_hover();
        self.request_redraw();
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
        {
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn destination_from_uri(value: String) -> flow::Destination {
    if let Some(fragment) = value.strip_prefix('#') {
        return flow::Destination::Local(flow::AnchorId::new(percent_decode(fragment)));
    }
    if value.contains("://")
        || value.starts_with("mailto:")
        || value.starts_with("tel:")
        || value.starts_with("data:")
    {
        return flow::Destination::External(value);
    }
    let (path, anchor) = value.split_once('#').map_or_else(
        || (value.clone(), None),
        |(path, anchor)| {
            (
                path.to_owned(),
                Some(flow::AnchorId::new(percent_decode(anchor))),
            )
        },
    );
    flow::Destination::Document {
        document: flow::DocumentReference::Relative(path),
        anchor,
    }
}

#[cfg(test)]
fn flow_destination_uri(destination: &flow::Destination) -> String {
    use crate::navigation::{FlowDestinationActivation, flow_destination_activation};
    match flow_destination_activation(destination) {
        FlowDestinationActivation::Local(anchor) => format!("#{}", anchor.as_str()),
        FlowDestinationActivation::Relative { path, anchor } => append_flow_fragment(path, anchor),
        FlowDestinationActivation::PublicationItem { identity, anchor }
        | FlowDestinationActivation::Host { identity, anchor } => {
            append_flow_fragment(identity, anchor)
        }
        FlowDestinationActivation::External(uri) => uri.to_owned(),
    }
}

#[cfg(test)]
fn append_flow_fragment(path: &str, anchor: Option<&flow::AnchorId>) -> String {
    match anchor {
        Some(anchor) => format!("{path}#{}", anchor.as_str()),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        formats::markdown, navigation::NavigationTarget, pixels::PixelBuffer,
        render::render_surface_size,
    };

    fn parse_document(source: &str) -> FlowDocument {
        markdown::parse(source, Default::default())
            .unwrap()
            .document
    }

    trait TestRender {
        fn render(&mut self, width: u32, height: u32, scale: f32) -> anyhow::Result<PixelBuffer>;
        fn render_continuous_region_reusing(
            &mut self,
            _unused: PixelBuffer,
            scene_width: u32,
            region_y: f32,
            region_height: f32,
            scale: f32,
        ) -> anyhow::Result<PixelBuffer>;
    }

    impl TestRender for DocumentViewer {
        fn render(&mut self, width: u32, height: u32, scale: f32) -> anyhow::Result<PixelBuffer> {
            let (physical_width, physical_height) =
                render_surface_size(width as f32, height as f32, scale)?;
            let mut buffer = PixelBuffer::new(physical_width, physical_height, [0; 3])?;
            self.render_into(&mut buffer.surface_mut(), width, height, scale)?;
            Ok(buffer)
        }

        fn render_continuous_region_reusing(
            &mut self,
            _unused: PixelBuffer,
            scene_width: u32,
            region_y: f32,
            region_height: f32,
            scale: f32,
        ) -> anyhow::Result<PixelBuffer> {
            let (physical_width, physical_height) =
                render_surface_size(scene_width as f32, region_height, scale)?;
            let mut buffer = PixelBuffer::new(physical_width, physical_height, [0; 3])?;
            self.render_continuous_region_into(
                &mut buffer.surface_mut(),
                scene_width,
                region_y,
                region_height,
                scale,
            )?;
            Ok(buffer)
        }
    }

    fn viewer(source: &str) -> DocumentViewer {
        let settings = Settings::default();
        let mut viewer = DocumentViewer::new("document.md", settings);
        viewer.replace_document(Ok(parse_document(source)));
        viewer.render(480, 320, 1.0).unwrap();
        viewer.take_effects();
        viewer
    }

    #[test]
    fn viewer_renders_and_exposes_document_behavior_without_an_app_shell() {
        let mut viewer = viewer("# First\n\nalpha needle\n\n## Second\n\nbeta needle\n");

        let outline = viewer.outline();
        assert_eq!(outline.nodes().len(), 2);
        let second = outline.nodes()[1].identity.clone();
        assert!(viewer.navigate_to_heading(&second));

        viewer.set_search_query("needle");
        assert_eq!(viewer.search_matches().len(), 2);
        assert!(viewer.move_to_search_match(false));
        assert!(viewer.selection().is_none());
    }

    #[test]
    fn link_navigation_resolves_imported_anchors_and_preserves_cross_document_links() {
        let mut viewer = viewer("# First\n\ntext\n\n## Target\n\nmore");

        assert!(viewer.activate_link("#target"));
        let focused = viewer
            .focused
            .as_ref()
            .and_then(|identity| viewer.navigation.target(identity))
            .unwrap();
        assert_eq!(focused.label, "Target");

        assert!(viewer.activate_link("document.md#first"));
        let focused = viewer
            .focused
            .as_ref()
            .and_then(|identity| viewer.navigation.target(identity))
            .unwrap();
        assert_eq!(focused.label, "First");

        assert!(viewer.activate_link("other.md#target"));
        assert!(viewer.take_effects().iter().any(
            |effect| matches!(effect, ViewerEffect::FollowLink(destination) if flow_destination_uri(destination) == "other.md#target")
        ));
    }

    #[test]
    fn generated_note_returns_prefer_the_activating_reference_and_fall_back_to_the_first() {
        let source = "first[^note] then second[^note]\n\n[^note]: definition\n";
        let mut reader = viewer(source);
        let references: Vec<_> = reader
            .navigation
            .targets()
            .iter()
            .filter(|target| target.kind == NavigationKind::FootnoteReference)
            .map(NavigationTarget::identity)
            .collect();
        assert_eq!(references.len(), 2);
        let return_target = reader
            .navigation
            .targets()
            .iter()
            .find(|target| target.generated_action().is_some())
            .map(NavigationTarget::identity)
            .expect("referenced definition has generated return chrome");

        reader.focused = Some(references[1].clone());
        assert!(reader.activate_focused());
        let original = reader.suspend_with_document("temporary", parse_document("temporary\n"));
        let _temporary = reader.activate_session(original);
        reader.focused = Some(return_target.clone());
        assert!(reader.activate_focused());
        assert_eq!(reader.focused, Some(references[1].clone()));

        let mut direct = viewer(source);
        let direct_return = direct
            .navigation
            .targets()
            .iter()
            .find(|target| target.generated_action().is_some())
            .map(NavigationTarget::identity)
            .unwrap();
        let first_reference = direct
            .navigation
            .targets()
            .iter()
            .find(|target| target.kind == NavigationKind::FootnoteReference)
            .map(NavigationTarget::identity)
            .unwrap();
        direct.focused = Some(direct_return);
        assert!(direct.activate_focused());
        assert_eq!(direct.focused, Some(first_reference));

        let mut pointer = viewer(source);
        let (arrow_x, arrow_y) = pointer
            .layout
            .blocks
            .iter()
            .flat_map(|block| &block.lines)
            .find_map(|line| {
                line.runs
                    .iter()
                    .find(|run| run.generated_action.is_some())
                    .map(|run| (run.x + run.width / 2.0, line.y + line.height / 2.0))
            })
            .unwrap();
        let first_reference = pointer
            .navigation
            .targets()
            .iter()
            .find(|target| target.kind == NavigationKind::FootnoteReference)
            .map(NavigationTarget::identity)
            .unwrap();
        let margin = pointer.renderer.settings().theme.page_margin;
        pointer.pointer_moved(
            Some((pointer.layout_x + arrow_x, margin + arrow_y)),
            Duration::ZERO,
        );
        pointer.pointer_button(
            PointerButtonInput {
                pressed: true,
                click_count: 1,
            },
            Duration::ZERO,
        );
        pointer.pointer_button(
            PointerButtonInput {
                pressed: false,
                click_count: 1,
            },
            Duration::ZERO,
        );
        pointer.tick(LINK_ACTIVATION_DELAY);
        assert_eq!(pointer.focused, Some(first_reference));

        let mut mismatch = viewer("first[^a], second[^b].\n\n[^a]: alpha\n\n[^b]: beta\n");
        let mismatch_references: Vec<_> = mismatch
            .navigation
            .targets()
            .iter()
            .filter(|target| target.kind == NavigationKind::FootnoteReference)
            .map(NavigationTarget::identity)
            .collect();
        let mismatch_returns: Vec<_> = mismatch
            .navigation
            .targets()
            .iter()
            .filter(|target| target.generated_action().is_some())
            .map(NavigationTarget::identity)
            .collect();
        mismatch.focused = Some(mismatch_references[0].clone());
        assert!(mismatch.activate_focused());
        mismatch.focused = Some(mismatch_returns[1].clone());
        assert!(mismatch.activate_focused());
        assert_eq!(mismatch.focused, Some(mismatch_references[1].clone()));

        let orphan = viewer("[^orphan]: definition\n");
        assert!(
            orphan
                .navigation
                .targets()
                .iter()
                .all(|target| target.generated_action().is_none())
        );
    }

    #[test]
    fn pointer_activates_inline_and_atomic_flow_links() {
        fn click(viewer: &mut DocumentViewer, document_x: f32, document_y: f32) -> String {
            let margin = viewer.renderer.settings().theme.page_margin;
            viewer.pointer_moved(
                Some((viewer.layout_x + document_x, margin + document_y)),
                Duration::ZERO,
            );
            viewer.pointer_button(
                PointerButtonInput {
                    pressed: true,
                    click_count: 1,
                },
                Duration::ZERO,
            );
            viewer.pointer_button(
                PointerButtonInput {
                    pressed: false,
                    click_count: 1,
                },
                Duration::ZERO,
            );
            viewer.tick(LINK_ACTIVATION_DELAY);
            viewer
                .take_effects()
                .into_iter()
                .find_map(|effect| match effect {
                    ViewerEffect::FollowLink(destination) => {
                        Some(flow_destination_uri(&destination))
                    }
                    _ => None,
                })
                .expect("pointer link activation opens the destination")
        }

        let mut inline = viewer("[inline link](https://inline.example)");
        let line = &inline.layout.blocks[0].lines[0];
        let run = line
            .runs
            .iter()
            .find(|run| run.flow_link.is_some())
            .unwrap();
        let inline_point = (run.x + run.width / 2.0, line.y + line.height / 2.0);
        let margin = inline.renderer.settings().theme.page_margin;
        assert!(
            inline.has_activation_at(inline.layout_x + inline_point.0, margin + inline_point.1)
        );
        assert!(!inline.has_activation_at(0.0, 0.0));
        assert_eq!(
            click(&mut inline, inline_point.0, inline_point.1),
            "https://inline.example"
        );

        let mut atomic = viewer("[![atom](image.png)](https://atomic.example)");
        let object = atomic.layout.atomic_objects().next().unwrap();
        let atomic_point = (
            object.x + object.width / 2.0,
            object.y + object.height / 2.0,
        );
        assert_eq!(
            click(&mut atomic, atomic_point.0, atomic_point.1),
            "https://atomic.example"
        );
    }

    #[test]
    fn pending_restore_anchor_survives_setting_changes() {
        let mut viewer = viewer("# Heading\n\nalpha beta gamma delta\n");
        let offset = viewer
            .document
            .as_ref()
            .unwrap()
            .text()
            .as_str()
            .find("gamma")
            .unwrap();
        viewer.layout = Arc::new(ContinuousLayout::default());
        assert!(viewer.restore_stable_position(
            &StableTextPosition {
                text_offset: offset,
                context_before: String::new(),
                context_after: String::new(),
            },
            true,
        ));
        assert_eq!(viewer.page_anchor, Some(offset));

        viewer.set_settings(Settings::default());
        assert_eq!(viewer.page_anchor, Some(offset));

        viewer.set_reading_mode(ReadingMode::Paginated);
        viewer.restore_stable_position(
            &StableTextPosition {
                text_offset: offset,
                context_before: String::new(),
                context_after: String::new(),
            },
            true,
        );
        assert_eq!(viewer.page_anchor, Some(offset));
    }

    #[test]
    fn stable_positions_recover_from_shifted_canonical_context() {
        let position = StableTextPosition {
            text_offset: 6,
            context_before: "alpha ".into(),
            context_after: "beta".into(),
        };
        assert_eq!(
            position.recover_offset("intro alpha beta outro", false),
            Some(12)
        );
        assert_eq!(position.recover_offset("unrelated", false), None);
    }

    #[test]
    fn actions_update_only_the_revisions_they_change() {
        let source = format!(
            "# Heading\n\nalpha needle\n\n{}",
            "more text\n\n".repeat(80)
        );
        let mut viewer = viewer(&source);
        let initial = viewer.revisions();

        viewer.scroll_to(f32::MAX);
        let scrolled = viewer.revisions();
        assert!(scrolled.position > initial.position);
        viewer.scroll_to(f32::MAX);
        assert_eq!(viewer.revisions().position, scrolled.position);

        viewer.set_search_query("needle");
        assert!(viewer.revisions().semantics > scrolled.semantics);
    }

    #[test]
    fn opaque_sessions_restore_document_behavior() {
        let mut viewer = viewer("# Original\n\nalpha needle\n");
        viewer.set_search_query("needle");
        let original_position = viewer.position().unwrap();
        let temporary = parse_document("# Manual\n\nhelp\n");

        let original = viewer.suspend_with_document("manual", temporary);
        assert_eq!(viewer.title(), "manual");
        assert!(viewer.search_matches().is_empty());

        let _manual = viewer.activate_session(original);
        assert_eq!(viewer.title(), "document.md");
        assert_eq!(viewer.search_query(), "needle");
        viewer.restore_position(original_position);
        assert_eq!(viewer.active_search_match(), Some(0));
    }

    #[test]
    fn viewer_owns_pointer_selection_behavior() {
        let mut viewer = viewer("# Heading\n\nalpha beta\n");
        let offset = viewer
            .document
            .as_ref()
            .unwrap()
            .text()
            .as_str()
            .find("alpha")
            .unwrap();
        let (run_x, run_width, line_y, line_height) = viewer
            .layout
            .blocks
            .iter()
            .flat_map(|block| &block.lines)
            .flat_map(|line| {
                line.runs
                    .iter()
                    .map(move |run| (run.range, run.x, run.width, line.y, line.height))
            })
            .find_map(|(source, x, width, y, height)| {
                source.contains(offset).then_some((x, width, y, height))
            })
            .unwrap();
        let margin = viewer.renderer.settings().theme.page_margin;
        let pointer = (
            viewer.layout_x + run_x + run_width / 2.0,
            margin + line_y + line_height / 2.0 - viewer.scroll_y,
        );

        viewer.pointer_moved(Some(pointer), Duration::ZERO);
        viewer.pointer_button(
            PointerButtonInput {
                pressed: true,
                click_count: 2,
            },
            Duration::ZERO,
        );
        viewer.pointer_button(
            PointerButtonInput {
                pressed: false,
                click_count: 2,
            },
            Duration::ZERO,
        );

        assert_eq!(
            selected_text(
                viewer.document.as_ref().unwrap(),
                viewer.selection().unwrap()
            )
            .unwrap(),
            "alpha"
        );
    }

    #[test]
    fn preparing_pagination_settles_pending_mode_and_reflow_anchors_without_painting() {
        let mut viewer = DocumentViewer::new("synthetic", Settings::default());
        assert!(viewer.register_font(Arc::from(
            include_bytes!("../tests/fixtures/fonts/charter.ttf").as_slice()
        )));
        viewer.replace_document(Ok(parse_document(
            &"Synthetic paragraph with enough text to wrap onto several lines.\n\n".repeat(100),
        )));
        assert!(viewer.prepare_continuous_scene(400, 240));
        viewer.scroll_to(1000.0);
        let anchor = viewer.position().unwrap().text_anchor;
        assert!(anchor > 0);
        viewer.set_reading_mode(ReadingMode::Paginated);
        let (page, _) = viewer.prepare_pagination(400, 240).unwrap();
        assert!(page > 1);
        assert_eq!(page - 1, viewer.pages.page_for_text(anchor));
        viewer.resize((300, 180));
        let anchor = viewer.page_anchor.unwrap();
        let (page, _) = viewer.prepare_pagination(300, 180).unwrap();
        assert_eq!(page - 1, viewer.pages.page_for_text(anchor));
        let before = page;
        assert!(viewer.scroll_by(1.0));
        assert_eq!(viewer.paginated_page_metrics().unwrap().0, before + 1);
    }

    #[test]
    fn read_only_layout_queries_do_not_return_stale_geometry() {
        let mut viewer = DocumentViewer::new("synthetic", Settings::default());
        viewer.replace_document(Ok(parse_document("![Sample](sample.png)")));
        assert!(viewer.prepare_continuous_scene(640, 480));
        assert!(viewer.continuous_layout().is_some());
        assert!(viewer.provide_image_metadata(ImageRequest::new("sample.png", None), 4, 4));
        assert!(viewer.continuous_layout().is_none());
        assert!(viewer.prepare_continuous_scene(640, 480));
        viewer.resize((280, 480));
        assert!(viewer.continuous_layout().is_none());
        viewer.set_reading_mode(ReadingMode::Paginated);
        assert!(viewer.prepare_pagination(280, 480).is_some());
        assert!(viewer.paginated_layout().is_some());
        viewer.resize((280, 600));
        assert!(viewer.paginated_layout().is_none());
    }

    #[test]
    fn document_replacement_invalidates_retained_layout_and_indexes() {
        let mut viewer = DocumentViewer::new("synthetic", Settings::default());
        viewer.replace_document(Ok(parse_document("Old text.")));
        assert!(viewer.prepare_continuous_scene(640, 480));
        let before = viewer.revisions();
        viewer.replace_document(Ok(parse_document("New text.")));
        assert!(viewer.continuous_layout().is_none());
        assert!(viewer.paginated_layout().is_none());
        assert!(viewer.revisions().document > before.document);
        assert!(viewer.prepare_continuous_scene(640, 480));
        let visible: String = viewer
            .continuous_layout()
            .unwrap()
            .blocks
            .iter()
            .flat_map(|block| &block.lines)
            .flat_map(|line| &line.runs)
            .map(|run| run.text.as_str())
            .collect();
        assert_eq!(visible, "Newtext.");
        assert_eq!(
            viewer.active_document().unwrap().text().as_str(),
            "New text."
        );
    }

    #[test]
    fn narrow_real_font_layout_retains_text_around_a_hard_break() {
        let source = "A hard break follows this sentence.  \nThis text must begin on the next line. A soft source-code line ending should\ninstead behave like an ordinary space in rendered prose.";
        let expected: String = source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let settings = Settings {
            rendering: crate::settings::RenderSettings {
                font_size: 11.5,
                ..crate::settings::RenderSettings::default()
            },
            ..Settings::default()
        };
        let mut viewer = DocumentViewer::new("document.md", settings);
        assert!(viewer.register_font(Arc::from(
            include_bytes!("../tests/fixtures/fonts/charter.ttf").as_slice()
        )));
        viewer.replace_document(Ok(parse_document(source)));

        for width in [280, 320, 390, 480, 600] {
            assert!(viewer.prepare_continuous_scene(width, 600));
            let runs: Vec<_> = viewer.layout.blocks[0]
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .collect();
            let rendered: String = runs
                .iter()
                .filter(|run| run.range.start != run.range.end)
                .flat_map(|run| run.text.chars())
                .collect();
            assert_eq!(rendered, expected, "lost text at viewport width {width}");
            assert!(
                runs.iter().all(|run| {
                    // Only generated discretionary hyphens may protrude; the
                    // source text itself must still fit inside the measure.
                    let right = if run.range.is_empty() && run.text == "-" {
                        run.x
                    } else {
                        run.x + run.width
                    };
                    run.x.is_finite() && run.width.is_finite() && right <= viewer.layout.width + 0.1
                }),
                "positioned text outside the measure at viewport width {width}: {runs:?}"
            );
        }
    }

    #[test]
    fn artifact_cache_reuses_portrait_composition_and_pagination_after_landscape() {
        let mut settings = Settings::default();
        settings.reading_mode = ReadingMode::Paginated;
        let mut viewer = DocumentViewer::new("document.md", settings);
        viewer.replace_document(Ok(parse_document(
            &"cache variants paragraph. ".repeat(400),
        )));

        viewer.render(480, 720, 1.0).unwrap();
        let portrait_composition = Arc::clone(&viewer.composed);
        let portrait_continuous = Arc::clone(&viewer.layout);
        let portrait_pages = Arc::clone(&viewer.pages);
        viewer.render(720, 480, 1.0).unwrap();
        assert!(!Arc::ptr_eq(&portrait_composition, &viewer.composed));
        assert!(!Arc::ptr_eq(&portrait_continuous, &viewer.layout));
        assert!(!Arc::ptr_eq(&portrait_pages, &viewer.pages));
        viewer.render(480, 720, 1.0).unwrap();
        assert!(Arc::ptr_eq(&portrait_composition, &viewer.composed));
        assert!(Arc::ptr_eq(&portrait_continuous, &viewer.layout));
        assert!(Arc::ptr_eq(&portrait_pages, &viewer.pages));
    }

    #[test]
    fn paint_only_changes_do_not_rebuild_layout_artifacts() {
        let mut settings = Settings::default();
        settings.reading_mode = ReadingMode::Paginated;
        let mut viewer = DocumentViewer::new("document.md", settings);
        viewer.replace_document(Ok(parse_document("alpha beta gamma")));
        viewer.render(480, 720, 1.0).unwrap();
        let composition = Arc::clone(&viewer.composed);
        let continuous = Arc::clone(&viewer.layout);
        let pages = Arc::clone(&viewer.pages);

        viewer.set_dark_mode(true);
        viewer.render(480, 720, 1.0).unwrap();
        assert!(Arc::ptr_eq(&composition, &viewer.composed));
        assert!(Arc::ptr_eq(&continuous, &viewer.layout));
        assert!(Arc::ptr_eq(&pages, &viewer.pages));
        viewer.set_rasterization(crate::text::RasterizationOptions {
            regular_gamma: 0.8,
            emphasized_gamma: 0.7,
            hinting: false,
            positioning: crate::text::GlyphPositioning::Fractional,
        });
        viewer.render(480, 720, 1.0).unwrap();

        assert!(Arc::ptr_eq(&composition, &viewer.composed));
        assert!(Arc::ptr_eq(&continuous, &viewer.layout));
        assert!(Arc::ptr_eq(&pages, &viewer.pages));
    }

    #[test]
    fn typography_changes_use_distinct_composition_and_pagination_keys() {
        let mut settings = Settings::default();
        settings.reading_mode = ReadingMode::Paginated;
        let mut viewer = DocumentViewer::new("document.md", settings);
        viewer.replace_document(Ok(parse_document(&"typography paragraph. ".repeat(200))));
        viewer.render(480, 720, 1.0).unwrap();
        let initial_pages = viewer.pages.pages.len();
        let composition = Arc::clone(&viewer.composed);
        let continuous = Arc::clone(&viewer.layout);
        let pages = Arc::clone(&viewer.pages);

        viewer.set_font_size(viewer.font_size_points + 3.0);
        viewer.render(480, 720, 1.0).unwrap();

        assert!(!Arc::ptr_eq(&composition, &viewer.composed));
        assert!(!Arc::ptr_eq(&continuous, &viewer.layout));
        assert!(!Arc::ptr_eq(&pages, &viewer.pages));
        assert_ne!(viewer.pages.pages.len(), initial_pages);
    }

    #[test]
    fn viewer_owns_continuous_scene_state_and_region_rendering() {
        let mut viewer = viewer("# Heading\n\nA paragraph that is long enough to render.\n");
        assert!(viewer.prepare_continuous_scene(480, 320));
        let (_, height) = viewer.continuous_scroll_metrics().unwrap();
        assert!(height > 0.0);

        let image = viewer
            .render_continuous_region_reusing(PixelBuffer::default(), 480, 0.0, 128.0, 1.0)
            .unwrap();
        assert_eq!(image.width(), 480);
        assert_eq!(image.height(), 128);
    }
}
