//! Format-neutral reading state. Hosts fulfill explicit work; sessions never
//! acquire sources or select a publication format.
//!
//! ```compile_fail
//! fn bypass(session: &mut flow_publication::reading_session::ReadingSession)
//!     -> &mut flow_publication::document::viewer::DocumentViewer {
//!     session.viewer_mut()
//! }
//! ```
//! ```compile_fail
//! fn bypass(session: &flow_publication::reading_session::ReadingSession,
//!           request: &flow_publication::document::viewer::ImageRequest) {
//!     session.viewer().renderer().retry_image(request);
//! }
//! ```

use crate::location::{LocationError, PERSISTENT_LOCATION_VERSION, PersistentReadingLocation};
use crate::publication::registry::{PublicationHandle, PublicationRegistry, RegistryError};
use crate::publication::*;
use flow_document::model::flow::{Destination, DocumentReference, FlowDocument};
use flow_document::position::{StableTextPosition, TextFingerprint};
use flow_document::settings::{ReadingMode, Settings};
use flow_document::viewer::{DocumentViewer, FontResource, ViewerSession};
use flow_document::viewer_cache::SemanticKey;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use thiserror::Error;

mod interaction;
mod pagination;
mod requests;
mod resources;
use pagination::PublicationPagination;
pub use requests::*;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DocumentCacheKey {
    publication: PublicationHandle,
    document: DocumentKey,
}

#[derive(Clone, Debug)]
struct Bindings {
    links: HashMap<DocumentReference, LinkBinding>,
    resources: HashMap<String, ResourceReference>,
}
struct CachedDocument {
    imported: ImportedDocument,
    semantic_key: Option<SemanticKey>,
}
#[derive(Clone, Debug)]
struct ActiveDocument {
    key: DocumentCacheKey,
    target: PublicationTarget,
    bindings: Arc<Bindings>,
    fingerprint: TextFingerprint,
}
struct SuspendedDocument {
    active: ActiveDocument,
    viewer: Option<ViewerSession>,
    position: Option<StableTextPosition>,
}

#[derive(Clone, Copy, Debug)]
enum HistoryAction {
    Record,
    Replace,
    Back,
    Forward,
}

#[derive(Clone)]
struct NavigationPlan {
    publication: PublicationKey,
    revision: Option<PublicationRevision>,
    allow_revision_change: bool,
    target: Option<PublicationTarget>,
    restore: Option<PersistentReadingLocation>,
    history: HistoryAction,
}
struct Navigation {
    id: NavigationId,
    plan: NavigationPlan,
    state: NavigationState,
    bound: Option<(PublicationHandle, PublicationTarget)>,
    prepared: Option<CachedDocument>,
}

/// Coordinated viewer state over host-supplied publication descriptions.
/// There is no loader callback, URI router, mutable viewer or mutable registry.
pub struct ReadingSession {
    viewer: DocumentViewer,
    font_context: flow_document::font_context::FontContext,
    settings: Settings,
    font_resources: Vec<FontResource>,
    publications: PublicationRegistry,
    active: Option<ActiveDocument>,
    navigation: Option<Navigation>,
    back: Vec<SuspendedDocument>,
    forward: Vec<SuspendedDocument>,
    document_cache: HashMap<DocumentCacheKey, CachedDocument>,
    document_lru: VecDeque<DocumentCacheKey>,
    document_cache_limit: usize,
    resident_history_limit: usize,
    background_request_limit: usize,
    requests: HashMap<RequestId, PendingRequest>,
    effects: Vec<SessionEffect>,
    prefetch_failed: HashSet<DocumentCacheKey>,
    fonts: HashMap<(PublicationHandle, ResourceKey), FontResource>,
    font_lru: VecDeque<(PublicationHandle, ResourceKey)>,
    failed_fonts: HashSet<(PublicationHandle, ResourceKey)>,
    failed_images: Vec<(DocumentCacheKey, flow_document::viewer::ImageRequest)>,
    image_metadata: HashMap<(DocumentCacheKey, String), (u32, u32)>,
    image_lru: VecDeque<(DocumentCacheKey, String)>,
    deferred_images: VecDeque<(
        DocumentCacheKey,
        flow_document::viewer::ImageRequest,
        ResourceKind,
        RequestPurpose,
    )>,
    publication_pagination: Option<PublicationPagination>,
}

impl ReadingSession {
    pub fn new(settings: Settings) -> Self {
        Self::new_with_font_context(
            settings,
            flow_document::font_context::FontContext::default(),
        )
    }

    pub fn new_with_font_context(
        settings: Settings,
        font_context: flow_document::font_context::FontContext,
    ) -> Self {
        Self {
            viewer: DocumentViewer::new("", settings.clone()),
            font_context,
            settings,
            font_resources: Vec::new(),
            publications: PublicationRegistry::new(),
            active: None,
            navigation: None,
            back: Vec::new(),
            forward: Vec::new(),
            document_cache: HashMap::new(),
            document_lru: VecDeque::new(),
            document_cache_limit: 3,
            resident_history_limit: 4,
            background_request_limit: 2,
            requests: HashMap::new(),
            effects: Vec::new(),
            prefetch_failed: HashSet::new(),
            fonts: HashMap::new(),
            font_lru: VecDeque::new(),
            failed_fonts: HashSet::new(),
            failed_images: Vec::new(),
            image_metadata: HashMap::new(),
            image_lru: VecDeque::new(),
            deferred_images: VecDeque::new(),
            publication_pagination: None,
        }
    }

    /// Recover the reusable font owner when discarding the reading state.
    pub fn into_font_context(self) -> flow_document::font_context::FontContext {
        self.font_context
    }

    pub fn font_context_stats(&self) -> flow_document::font_context::FontContextStats {
        self.font_context.stats()
    }
    pub fn font_context_options(&self) -> flow_document::font_context::FontContextOptions {
        self.font_context.options()
    }

    /// Apply host cache limits without invalidating geometry or page indexes.
    pub fn set_font_context_options(
        &mut self,
        options: flow_document::font_context::FontContextOptions,
    ) {
        self.font_context.set_options(options);
    }
    pub fn trim_font_context(&mut self) {
        self.font_context.trim();
    }

    pub fn viewer(&self) -> &DocumentViewer {
        &self.viewer
    }
    pub fn publications(&self) -> &PublicationRegistry {
        &self.publications
    }
    pub fn current_publication(&self) -> Option<PublicationHandle> {
        Some(self.active.as_ref()?.key.publication)
    }
    pub fn current_target(&self) -> Option<&PublicationTarget> {
        Some(&self.active.as_ref()?.target)
    }
    pub fn navigation_state(&self, id: NavigationId) -> Option<&NavigationState> {
        self.navigation
            .as_ref()
            .filter(|navigation| navigation.id == id)
            .map(|navigation| &navigation.state)
    }
    pub fn history_is_empty(&self) -> bool {
        self.back.is_empty() && self.viewer.history_is_empty()
    }

    pub fn set_cache_limits(&mut self, documents: usize, resident_history: usize) {
        self.document_cache_limit = documents;
        self.resident_history_limit = resident_history;
        self.trim_resident_history();
        self.trim_document_cache();
        self.cancel_unwanted_prefetch();
    }

    /// At most 31 background requests, reserving a foreground slot.
    pub fn set_background_request_limit(&mut self, limit: usize) {
        self.background_request_limit = limit.min(31);
        let ids = self
            .requests
            .iter()
            .filter(|(_, request)| request.purpose() >= RequestPurpose::Prefetch)
            .skip(self.background_request_limit)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in ids {
            self.cancel_request(id);
        }
    }
    pub fn set_layout_artifact_cache_limit(&mut self, bytes: usize) {
        self.viewer.set_layout_artifact_cache_limit(bytes);
    }

    pub fn register_publication(
        &mut self,
        publication: Publication,
    ) -> Result<PublicationHandle, ReadingSessionError> {
        let handle = self.publications.register(publication)?;
        self.drive_navigation()?;
        Ok(handle)
    }

    pub fn open_target(
        &mut self,
        publication: PublicationHandle,
        target: PublicationTarget,
    ) -> Result<NavigationId, ReadingSessionError> {
        let model = self.model(publication)?;
        if model.document(&target.document).is_none() {
            return Err(ModelError::UnknownDocument(target.document).into());
        }
        self.start_navigation(NavigationPlan {
            publication: model.key().clone(),
            revision: Some(model.revision().clone()),
            allow_revision_change: false,
            target: Some(target),
            restore: None,
            history: HistoryAction::Record,
        })
    }

    /// Resolves a missing model through the host, then opens its declared start.
    pub fn open_publication(
        &mut self,
        publication: PublicationKey,
    ) -> Result<NavigationId, ReadingSessionError> {
        self.start_navigation(NavigationPlan {
            publication,
            revision: None,
            allow_revision_change: false,
            target: None,
            restore: None,
            history: HistoryAction::Record,
        })
    }

    pub fn restore_location(
        &mut self,
        location: &PersistentReadingLocation,
    ) -> Result<NavigationId, ReadingSessionError> {
        location.validate()?;
        self.start_navigation(NavigationPlan {
            publication: location.publication.clone(),
            revision: Some(location.publication_revision.clone()),
            allow_revision_change: true,
            target: Some(PublicationTarget {
                document: location.document.clone(),
                anchor: None,
            }),
            restore: Some(location.clone()),
            history: HistoryAction::Replace,
        })
    }

    /// Sequential reading does not add link-navigation history. Repeated source
    /// references were already normalized by the immutable model.
    pub fn move_reading_order(
        &mut self,
        direction: isize,
    ) -> Result<Option<NavigationId>, ReadingSessionError> {
        let Some(active) = &self.active else {
            return Ok(None);
        };
        let model = self.model(active.key.publication)?;
        let Some(index) = model.reading_order_index(&active.key.document) else {
            return Ok(None);
        };
        if direction == 0 {
            return Ok(None);
        }
        let Some(next) = index
            .checked_add_signed(direction.signum())
            .and_then(|index| model.reading_order().get(index))
            .cloned()
        else {
            return Ok(None);
        };
        let plan = NavigationPlan {
            publication: model.key().clone(),
            revision: Some(model.revision().clone()),
            allow_revision_change: false,
            target: Some(PublicationTarget::document(next)),
            restore: None,
            history: HistoryAction::Replace,
        };
        self.start_navigation(plan).map(Some)
    }

    pub fn reading_order_position(&self) -> Option<(usize, usize)> {
        let active = self.active.as_ref()?;
        let model = self.publications.get(active.key.publication)?;
        Some((
            model.reading_order_index(&active.key.document)? + 1,
            model.reading_order().len(),
        ))
    }

    pub fn follow_destination(
        &mut self,
        destination: Destination,
    ) -> Result<Option<NavigationId>, ReadingSessionError> {
        match destination {
            Destination::Local(anchor) => {
                self.viewer.activate_destination(Destination::Local(anchor));
                Ok(None)
            }
            Destination::External(reference) => {
                self.effects.push(SessionEffect::OpenExternal(reference));
                Ok(None)
            }
            Destination::Document { document, anchor } => {
                let active = self
                    .active
                    .as_ref()
                    .ok_or(ReadingSessionError::NoDocument)?;
                let binding = active.bindings.links.get(&document).cloned();
                if let Some(LinkBinding::Document(mut target)) = binding {
                    if anchor.is_some() {
                        target.anchor = anchor;
                    }
                    if target.document == active.key.document {
                        if let Some(anchor) = target.anchor {
                            self.viewer.activate_destination(Destination::Local(anchor));
                        }
                        return Ok(None);
                    }
                    return self.open_target(active.key.publication, target).map(Some);
                }
                let model = self.model(active.key.publication)?;
                let key = active.key.clone();
                let publication = model.key().clone();
                let revision = model.revision().clone();
                let destination = match binding {
                    Some(LinkBinding::Host(reference)) => Destination::Document {
                        document: DocumentReference::Host(reference),
                        anchor,
                    },
                    _ => Destination::Document { document, anchor },
                };
                let plan = NavigationPlan {
                    publication: publication.clone(),
                    revision: Some(revision.clone()),
                    allow_revision_change: false,
                    target: None,
                    restore: None,
                    history: HistoryAction::Record,
                };
                self.cancel_current_navigation();
                let id = NavigationId::new()?;
                self.navigation = Some(Navigation {
                    id,
                    plan,
                    state: NavigationState::Pending,
                    bound: None,
                    prepared: None,
                });
                let request = RequestId::new()?;
                self.requests
                    .insert(request, PendingRequest::Link { navigation: id });
                self.effects.push(SessionEffect::ResolveLink {
                    id: request,
                    publication,
                    revision,
                    document: key.document,
                    destination,
                });
                Ok(Some(id))
            }
        }
    }

    pub fn provide_link_target(
        &mut self,
        id: RequestId,
        resolved: ResolvedLink,
    ) -> Result<(), ReadingSessionError> {
        let Some(PendingRequest::Link { navigation, .. }) = self.requests.get(&id) else {
            return Err(ReadingSessionError::StaleRequest);
        };
        let navigation = *navigation;
        if !self.is_pending(navigation) {
            return Err(ReadingSessionError::StaleRequest);
        }
        match resolved {
            ResolvedLink::External(reference) => {
                check_string(&reference)?;
                self.requests.remove(&id);
                self.effects.push(SessionEffect::OpenExternal(reference));
                self.finish_navigation(NavigationState::External);
            }
            ResolvedLink::Target(target) => {
                self.requests.remove(&id);
                let current = self.navigation.as_mut().expect("pending navigation exists");
                current.plan.publication = target.publication;
                current.plan.revision = target.revision;
                current.plan.target = Some(target.target);
                self.drive_navigation()?;
            }
        }
        Ok(())
    }

    pub fn provide_publication(
        &mut self,
        id: RequestId,
        publication: Publication,
    ) -> Result<(), ReadingSessionError> {
        let Some(PendingRequest::Publication {
            navigation,
            key,
            revision,
            allow_change,
        }) = self.requests.get(&id)
        else {
            return Err(ReadingSessionError::StaleRequest);
        };
        if !self.is_pending(*navigation) {
            return Err(ReadingSessionError::StaleRequest);
        }
        if publication.key() != key
            || (!allow_change
                && revision
                    .as_ref()
                    .is_some_and(|revision| revision != publication.revision()))
        {
            return Err(ReadingSessionError::ResponseMismatch);
        }
        let handle = self.publications.register(publication)?;
        self.requests.remove(&id);
        if let Some(navigation) = &mut self.navigation {
            // Bind the explicit host choice, including an older preferred revision.
            let target = navigation
                .plan
                .target
                .clone()
                .or_else(|| self.publications.get(handle).and_then(Publication::start));
            if let Some(target) = target {
                navigation.bound = Some((handle, target));
            }
        }
        self.drive_navigation()
    }

    pub fn provide_document(
        &mut self,
        id: RequestId,
        imported: ImportedDocument,
    ) -> Result<(), ReadingSessionError> {
        let Some(PendingRequest::Document {
            key,
            purpose,
            navigation,
        }) = self.requests.get(&id)
        else {
            return Err(ReadingSessionError::StaleRequest);
        };
        let key = key.clone();
        let purpose = *purpose;
        let navigation = *navigation;
        imported.validate(self.model(key.publication)?)?;
        if navigation.is_some_and(|id| !self.is_pending(id)) {
            return Err(ReadingSessionError::StaleRequest);
        }
        self.requests.remove(&id);
        let cached = CachedDocument {
            imported,
            semantic_key: None,
        };
        if navigation.is_some() {
            self.navigation
                .as_mut()
                .expect("validated pending navigation")
                .prepared = Some(cached);
            self.drive_navigation()?;
        } else if purpose == RequestPurpose::Pagination {
            self.accept_pagination_document(key, cached)?;
        } else {
            self.cache_document(key, cached);
        }
        Ok(())
    }

    pub fn fail_request(
        &mut self,
        id: RequestId,
        failure: LoadFailure,
    ) -> Result<(), ReadingSessionError> {
        let request = self
            .requests
            .remove(&id)
            .ok_or(ReadingSessionError::StaleRequest)?;
        match request {
            PendingRequest::Publication { navigation, .. }
            | PendingRequest::Link { navigation, .. } => {
                if self.is_pending(navigation) {
                    self.finish_navigation(NavigationState::Failed(failure));
                }
            }
            PendingRequest::Document {
                key,
                purpose,
                navigation,
            } => {
                if navigation.is_some_and(|id| self.is_pending(id)) {
                    self.finish_navigation(NavigationState::Failed(failure));
                } else if purpose == RequestPurpose::Pagination {
                    self.fail_pagination(failure);
                } else {
                    self.prefetch_failed.insert(key);
                }
            }
            request @ PendingRequest::Resource { .. } => self.fail_resource(request, failure),
        }
        Ok(())
    }

    pub fn cancel_navigation(&mut self, id: NavigationId) -> bool {
        if !self.is_pending(id) {
            return false;
        }
        self.cancel_current_navigation();
        true
    }
    pub fn retry_navigation(
        &mut self,
        id: NavigationId,
    ) -> Result<NavigationId, ReadingSessionError> {
        let navigation = self
            .navigation
            .as_ref()
            .filter(|navigation| {
                navigation.id == id && matches!(navigation.state, NavigationState::Failed(_))
            })
            .ok_or(ReadingSessionError::NoFailedNavigation)?;
        let plan = navigation.plan.clone();
        self.failed_fonts.clear();
        self.start_navigation(plan)
    }

    pub fn take_effects(&mut self) -> Result<Vec<SessionEffect>, ReadingSessionError> {
        self.collect_viewer_effects()?;
        self.drive_navigation()?;
        self.advance_pagination()?;
        self.schedule_prefetch()?;
        Ok(std::mem::take(&mut self.effects))
    }

    pub fn persistent_location(&self) -> Option<PersistentReadingLocation> {
        let active = self.active.as_ref()?;
        let model = self.publications.get(active.key.publication)?;
        Some(PersistentReadingLocation {
            version: PERSISTENT_LOCATION_VERSION,
            publication: model.key().clone(),
            publication_revision: model.revision().clone(),
            document: active.key.document.clone(),
            text_fingerprint: active.fingerprint,
            position: self.viewer.stable_position()?,
            anchor_hint: active.target.anchor.clone(),
        })
    }

    pub fn compare_persistent_locations(
        &self,
        left: &PersistentReadingLocation,
        right: &PersistentReadingLocation,
    ) -> Option<Ordering> {
        left.validate().ok()?;
        right.validate().ok()?;
        if left.publication != right.publication
            || left.publication_revision != right.publication_revision
        {
            return None;
        }
        let handle = self
            .publications
            .revision(&left.publication, &left.publication_revision)?;
        let model = self.publications.get(handle)?;
        let left_index = model.reading_order_index(&left.document)?;
        let right_index = model.reading_order_index(&right.document)?;
        if left_index == right_index && left.text_fingerprint != right.text_fingerprint {
            return None;
        }
        Some(
            left_index
                .cmp(&right_index)
                .then_with(|| left.position.text_offset.cmp(&right.position.text_offset)),
        )
    }

    pub fn go_back(&mut self) -> Result<Option<NavigationId>, ReadingSessionError> {
        self.cancel_current_navigation();
        if self.viewer.go_back() {
            return Ok(None);
        }
        self.start_history_navigation(true)
    }
    pub fn go_forward(&mut self) -> Result<Option<NavigationId>, ReadingSessionError> {
        self.cancel_current_navigation();
        self.start_history_navigation(false)
    }

    fn start_history_navigation(
        &mut self,
        back: bool,
    ) -> Result<Option<NavigationId>, ReadingSessionError> {
        let Some(target) = (if back {
            self.back.last()
        } else {
            self.forward.last()
        }) else {
            return Ok(None);
        };
        let model = self.model(target.active.key.publication)?;
        let plan = NavigationPlan {
            publication: model.key().clone(),
            revision: Some(model.revision().clone()),
            allow_revision_change: false,
            target: Some(target.active.target.clone()),
            restore: None,
            history: if back {
                HistoryAction::Back
            } else {
                HistoryAction::Forward
            },
        };
        self.start_navigation(plan).map(Some)
    }

    fn model(&self, handle: PublicationHandle) -> Result<&Publication, ReadingSessionError> {
        self.publications
            .get(handle)
            .ok_or(RegistryError::UnknownPublication.into())
    }
    fn is_pending(&self, id: NavigationId) -> bool {
        self.navigation.as_ref().is_some_and(|navigation| {
            navigation.id == id && navigation.state == NavigationState::Pending
        })
    }
    fn start_navigation(
        &mut self,
        plan: NavigationPlan,
    ) -> Result<NavigationId, ReadingSessionError> {
        self.cancel_current_navigation();
        let id = NavigationId::new()?;
        self.navigation = Some(Navigation {
            id,
            plan,
            state: NavigationState::Pending,
            bound: None,
            prepared: None,
        });
        self.drive_navigation()?;
        Ok(id)
    }
    fn finish_navigation(&mut self, state: NavigationState) {
        let Some(navigation) = &mut self.navigation else {
            return;
        };
        navigation.state = state.clone();
        navigation.prepared = None;
        let id = navigation.id;
        self.effects
            .push(SessionEffect::NavigationFinished { id, state });
        let requests = self
            .requests
            .iter()
            .filter_map(|(request, value)| match value {
                PendingRequest::Publication { navigation, .. }
                | PendingRequest::Link { navigation }
                    if *navigation == id =>
                {
                    Some(*request)
                }
                PendingRequest::Document {
                    navigation: Some(navigation),
                    ..
                }
                | PendingRequest::Resource {
                    navigation: Some(navigation),
                    ..
                } if *navigation == id => Some(*request),
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in requests {
            self.cancel_request(id);
        }
    }
    fn cancel_current_navigation(&mut self) {
        if self
            .navigation
            .as_ref()
            .is_some_and(|navigation| navigation.state == NavigationState::Pending)
        {
            self.finish_navigation(NavigationState::Cancelled);
        }
    }
    fn cancel_request(&mut self, id: RequestId) {
        if let Some(PendingRequest::Resource { key, images, .. }) = self.requests.remove(&id) {
            for image in images {
                self.retry_image_in_scope(&key, &image);
            }
        }
        self.effects
            .retain(|effect| effect.request_id() != Some(id));
        self.effects.push(SessionEffect::CancelRequest(id));
    }

    fn drive_navigation(&mut self) -> Result<(), ReadingSessionError> {
        let Some(navigation) = self
            .navigation
            .as_ref()
            .filter(|navigation| navigation.state == NavigationState::Pending)
        else {
            return Ok(());
        };
        let id = navigation.id;
        if self.requests.values().any(|request| matches!(request, PendingRequest::Link { navigation, .. } if *navigation == id)) { return Ok(()); }
        if navigation.bound.is_none() {
            let plan = navigation.plan.clone();
            // A registered model is an explicit host selection. Saved source
            // revisions are recovery evidence, not instructions to reacquire
            // an older edition behind the host\'s back.
            let handle = if plan.allow_revision_change || plan.revision.is_none() {
                self.publications.latest(&plan.publication)
            } else {
                plan.revision
                    .as_ref()
                    .and_then(|revision| self.publications.revision(&plan.publication, revision))
            };
            if let Some(handle) = handle {
                let target = plan
                    .target
                    .clone()
                    .or_else(|| self.publications.get(handle).and_then(Publication::start));
                let Some(target) = target else {
                    self.finish_navigation(NavigationState::Failed(LoadFailure::new(
                        "publication has no default start",
                    )));
                    return Ok(());
                };
                self.navigation.as_mut().expect("navigation exists").bound = Some((handle, target));
                let obsolete = self.requests.iter().filter_map(|(request, value)| matches!(value, PendingRequest::Publication { navigation, .. } if *navigation == id).then_some(*request)).collect::<Vec<_>>();
                for request in obsolete {
                    self.cancel_request(request);
                }
            } else {
                if !self.requests.values().any(|request| matches!(request, PendingRequest::Publication { navigation, .. } if *navigation == id)) {
                    let request = RequestId::new()?;
                    self.requests.insert(request, PendingRequest::Publication { navigation: id, key: plan.publication.clone(), revision: plan.revision.clone(), allow_change: plan.allow_revision_change });
                    self.effects.push(SessionEffect::ResolvePublication { id: request, publication: plan.publication, preferred_revision: plan.revision, allow_revision_change: plan.allow_revision_change });
                }
                return Ok(());
            }
        }
        let navigation = self.navigation.as_ref().expect("pending navigation");
        let (handle, target) = navigation.bound.clone().expect("resolved above");
        if self.model(handle)?.document(&target.document).is_none() {
            self.finish_navigation(NavigationState::Failed(LoadFailure::new(
                "saved or requested document is absent; host remapping required",
            )));
            return Ok(());
        }
        let key = DocumentCacheKey {
            publication: handle,
            document: target.document.clone(),
        };
        let history = navigation.plan.history;
        let resident = match history {
            HistoryAction::Back => self.back.last(),
            HistoryAction::Forward => self.forward.last(),
            _ => None,
        }
        .is_some_and(|entry| entry.active.key == key && entry.viewer.is_some());
        let already_active = !matches!(history, HistoryAction::Back | HistoryAction::Forward)
            && self.active.as_ref().is_some_and(|active| active.key == key);
        if !resident
            && !already_active
            && self
                .navigation
                .as_ref()
                .is_some_and(|navigation| navigation.prepared.is_none())
        {
            if let Some(cached) = self.take_cached_document(&key) {
                self.navigation
                    .as_mut()
                    .expect("pending navigation")
                    .prepared = Some(cached);
            } else {
                self.request_document(
                    key.clone(),
                    if matches!(history, HistoryAction::Back | HistoryAction::Forward) {
                        RequestPurpose::History
                    } else {
                        RequestPurpose::Foreground
                    },
                    Some(id),
                )?;
            }
        }
        let fonts_ready = self.ensure_fonts(&key, RequestPurpose::Foreground, Some(id))?;
        if !fonts_ready
            || (!resident
                && !already_active
                && self
                    .navigation
                    .as_ref()
                    .is_none_or(|navigation| navigation.prepared.is_none()))
        {
            return Ok(());
        }
        self.commit_navigation(key, target, resident, already_active)
    }

    fn commit_navigation(
        &mut self,
        key: DocumentCacheKey,
        target: PublicationTarget,
        resident: bool,
        already_active: bool,
    ) -> Result<(), ReadingSessionError> {
        let _span = flow_document::telemetry::phase_span!(
            "flow.session.activate",
            document = key.document.as_str()
        );
        let expected = self.navigation.as_ref().expect("pending navigation").id;
        self.collect_viewer_effects()?;
        if !self.is_pending(expected) {
            return Ok(());
        }
        let history = self
            .navigation
            .as_ref()
            .expect("pending navigation")
            .plan
            .history;
        let restore = self
            .navigation
            .as_ref()
            .expect("pending navigation")
            .plan
            .restore
            .clone();
        let mut old_position = self.viewer.stable_position();
        let old_active = self.active.clone();
        self.configure_active_environment(&key)?;
        let mut history_position = None;
        let mut history_fingerprint = None;
        if !already_active {
            let prepared = self
                .navigation
                .as_mut()
                .expect("pending navigation")
                .prepared
                .take();
            let previous;
            if resident {
                let entry = if matches!(history, HistoryAction::Back) {
                    self.back.last_mut()
                } else {
                    self.forward.last_mut()
                }
                .expect("resident history target");
                history_position = entry.position.clone();
                history_fingerprint = Some(entry.active.fingerprint);
                let active = entry.active.clone();
                previous = self
                    .viewer
                    .activate_session(entry.viewer.take().expect("checked resident"));
                self.active = Some(active);
            } else {
                let cached = prepared.expect("prepared target checked before commit");
                let fingerprint = TextFingerprint::of_document(&cached.imported.document);
                let bindings = Arc::new(Bindings {
                    links: cached.imported.links,
                    resources: cached.imported.resources,
                });
                self.apply_known_metadata(&key, &cached.imported.document, false);
                let title = self
                    .model(key.publication)?
                    .document(&key.document)
                    .expect("validated target")
                    .title
                    .clone();
                previous = self.viewer.suspend_with_document_key(
                    title,
                    cached.imported.document,
                    cached.semantic_key,
                );
                self.active = Some(ActiveDocument {
                    key: key.clone(),
                    target: target.clone(),
                    bindings,
                    fingerprint,
                });
                if matches!(history, HistoryAction::Back | HistoryAction::Forward) {
                    let entry = if matches!(history, HistoryAction::Back) {
                        self.back.last()
                    } else {
                        self.forward.last()
                    }
                    .expect("history target");
                    history_position = entry.position.clone();
                    history_fingerprint = Some(entry.active.fingerprint);
                }
            }
            if let Some(active) = old_active {
                let suspended = SuspendedDocument {
                    active,
                    viewer: Some(previous),
                    position: old_position.take(),
                };
                match history {
                    HistoryAction::Record => {
                        self.back.push(suspended);
                        self.forward.clear();
                    }
                    HistoryAction::Back => {
                        self.back.pop();
                        self.forward.push(suspended);
                    }
                    HistoryAction::Forward => {
                        self.forward.pop();
                        self.back.push(suspended);
                    }
                    HistoryAction::Replace => {
                        self.retain_document(suspended);
                    }
                }
            }
        }
        self.active.as_mut().expect("active after commit").target = target.clone();
        let viewport = self.viewer.viewport_size();
        if viewport.0 != 0 && viewport.1 != 0 {
            match self.settings.reading_mode {
                ReadingMode::Paginated => {
                    self.viewer
                        .with_font_context(&mut self.font_context, |viewer| {
                            viewer.prepare_pagination(viewport.0, viewport.1)
                        });
                }
                ReadingMode::Continuous => {
                    self.viewer
                        .with_font_context(&mut self.font_context, |viewer| {
                            viewer.prepare_continuous_scene(viewport.0, viewport.1)
                        });
                }
            }
        }
        let mut restored = None;
        if let Some(location) = restore {
            let exact = self
                .active
                .as_ref()
                .is_some_and(|active| active.fingerprint == location.text_fingerprint);
            restored = Some(
                if self
                    .viewer
                    .restore_stable_position(&location.position, exact)
                {
                    if exact {
                        RestoreOutcome::Exact
                    } else {
                        RestoreOutcome::Recovered
                    }
                } else if location
                    .anchor_hint
                    .as_ref()
                    .is_some_and(|anchor| self.viewer.navigate_to_fragment(anchor.as_str()))
                {
                    RestoreOutcome::AnchorFallback
                } else {
                    self.viewer.scroll_to(0.0);
                    RestoreOutcome::DocumentStartFallback
                },
            );
        } else if let Some(position) = history_position {
            let exact = self
                .active
                .as_ref()
                .is_some_and(|active| Some(active.fingerprint) == history_fingerprint);
            self.viewer.restore_stable_position(&position, exact);
        } else if let Some(anchor) = target.anchor {
            self.viewer.navigate_to_fragment(anchor.as_str());
        }
        self.trim_resident_history();
        if self
            .publication_pagination
            .as_ref()
            .is_some_and(|index| index.publication != key.publication)
        {
            self.clear_pagination();
        }
        self.prefetch_failed.clear();
        self.finish_navigation(NavigationState::Ready { restored });
        self.cancel_obsolete_resources();
        self.cancel_unwanted_prefetch();
        self.trim_fonts();
        Ok(())
    }

    fn request_document(
        &mut self,
        key: DocumentCacheKey,
        purpose: RequestPurpose,
        navigation: Option<NavigationId>,
    ) -> Result<(), ReadingSessionError> {
        if let Some((id, request)) = self.requests.iter_mut().find(|(_, request)| matches!(request, PendingRequest::Document { key: candidate, .. } if *candidate == key)) {
            if let PendingRequest::Document { purpose: previous, navigation: previous_navigation, .. } = request {
                if purpose < *previous { *previous = purpose; self.effects.push(SessionEffect::Reprioritize { id: *id, purpose }); }
                if navigation.is_some() { *previous_navigation = navigation; }
            }
            return Ok(());
        }
        if !self.can_request(purpose) {
            return Ok(());
        }
        let model = self.model(key.publication)?;
        let publication = model.key().clone();
        let revision = model.revision().clone();
        let id = RequestId::new()?;
        self.requests.insert(
            id,
            PendingRequest::Document {
                key: key.clone(),
                purpose,
                navigation,
            },
        );
        self.effects
            .push(SessionEffect::LoadDocument(DocumentRequest {
                id,
                publication,
                revision,
                document: key.document,
                purpose,
            }));
        Ok(())
    }
    fn can_request(&self, purpose: RequestPurpose) -> bool {
        self.requests.len() < 32
            && (purpose < RequestPurpose::Prefetch
                || self
                    .requests
                    .values()
                    .filter(|request| request.purpose() >= RequestPurpose::Prefetch)
                    .count()
                    < self.background_request_limit)
    }
    fn take_cached_document(&mut self, key: &DocumentCacheKey) -> Option<CachedDocument> {
        self.document_lru.retain(|candidate| candidate != key);
        self.document_cache.remove(key)
    }
    fn cache_document(&mut self, key: DocumentCacheKey, document: CachedDocument) {
        if self.document_cache_limit == 0 {
            return;
        }
        self.document_lru.retain(|candidate| candidate != &key);
        self.document_lru.push_back(key.clone());
        self.document_cache.insert(key, document);
        self.trim_document_cache();
    }
    fn trim_document_cache(&mut self) {
        let neighbors = self.prefetch_targets();
        while self.document_cache.len() > self.document_cache_limit {
            // Publication pagination scans the entire book. Prefer evicting
            // those one-off entries over neighbors we'd immediately prefetch
            // again. Fall back to LRU when the limit cannot hold all neighbors.
            let candidate = self
                .document_lru
                .iter()
                .position(|key| !neighbors.contains(key))
                .unwrap_or(0);
            if let Some(key) = self.document_lru.remove(candidate) {
                self.document_cache.remove(&key);
            } else {
                break;
            }
        }
    }
    fn retain_document(&mut self, mut entry: SuspendedDocument) {
        if let Some((_, document, semantic_key)) =
            entry.viewer.as_mut().and_then(ViewerSession::take_document)
        {
            self.cache_document(
                entry.active.key,
                CachedDocument {
                    imported: ImportedDocument {
                        document,
                        links: entry.active.bindings.links.clone(),
                        resources: entry.active.bindings.resources.clone(),
                    },
                    semantic_key,
                },
            );
        }
    }
    fn trim_resident_history(&mut self) {
        while self
            .back
            .iter()
            .chain(&self.forward)
            .filter(|entry| entry.viewer.is_some())
            .count()
            > self.resident_history_limit
        {
            let candidate = self
                .back
                .iter()
                .position(|entry| entry.viewer.is_some())
                .map(|index| (true, index))
                .or_else(|| {
                    self.forward
                        .iter()
                        .position(|entry| entry.viewer.is_some())
                        .map(|index| (false, index))
                });
            let Some((back, index)) = candidate else {
                break;
            };
            let entry = if back {
                &mut self.back[index]
            } else {
                &mut self.forward[index]
            };
            let mut viewer = entry.viewer.take().expect("selected resident entry");
            let active = entry.active.clone();
            if let Some((_, document, semantic_key)) = viewer.take_document() {
                self.cache_document(
                    active.key,
                    CachedDocument {
                        imported: ImportedDocument {
                            document,
                            links: active.bindings.links.clone(),
                            resources: active.bindings.resources.clone(),
                        },
                        semantic_key,
                    },
                );
            }
        }
    }
    fn resident(&self, key: &DocumentCacheKey) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| &active.key == key)
            || self
                .back
                .iter()
                .chain(&self.forward)
                .any(|entry| entry.viewer.is_some() && &entry.active.key == key)
    }
    fn prefetch_targets(&self) -> Vec<DocumentCacheKey> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        let Some(model) = self.publications.get(active.key.publication) else {
            return Vec::new();
        };
        let Some(index) = model.reading_order_index(&active.key.document) else {
            return Vec::new();
        };
        [1, -1]
            .into_iter()
            .filter_map(|direction| {
                index
                    .checked_add_signed(direction)
                    .and_then(|index| model.reading_order().get(index))
            })
            .take(self.document_cache_limit.min(self.background_request_limit))
            .map(|document| DocumentCacheKey {
                publication: active.key.publication,
                document: document.clone(),
            })
            .collect()
    }
    fn schedule_prefetch(&mut self) -> Result<(), ReadingSessionError> {
        if self
            .navigation
            .as_ref()
            .is_some_and(|navigation| navigation.state == NavigationState::Pending)
        {
            return Ok(());
        }
        for key in self.prefetch_targets() {
            if !self.resident(&key)
                && !self.document_cache.contains_key(&key)
                && !self
                    .publication_pagination
                    .as_ref()
                    .and_then(|index| index.job.as_ref())
                    .is_some_and(|job| job.key == key)
                && !self.prefetch_failed.contains(&key)
            {
                self.request_document(key, RequestPurpose::Prefetch, None)?;
            }
        }
        Ok(())
    }
    fn cancel_unwanted_prefetch(&mut self) {
        let targets = self.prefetch_targets();
        let ids = self
            .requests
            .iter()
            .filter_map(|(id, request)| match request {
                PendingRequest::Document {
                    key,
                    purpose: RequestPurpose::Prefetch,
                    navigation: None,
                } if !targets.contains(key) => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in ids {
            self.cancel_request(id);
        }
    }
}

#[derive(Debug, Error)]
pub enum ReadingSessionError {
    #[error("no document is active")]
    NoDocument,
    #[error("response does not match the requested content or representation")]
    ResponseMismatch,
    #[error("request is unknown, cancelled or superseded")]
    StaleRequest,
    #[error("no matching failed navigation to retry")]
    NoFailedNavigation,
    #[error("engine work identities exhausted")]
    Exhausted,
    #[error("invalid supplied font or image resource")]
    InvalidResource,
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Location(#[from] LocationError),
}

#[cfg(test)]
mod tests;
