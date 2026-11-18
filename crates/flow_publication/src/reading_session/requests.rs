//! Host work data. IDs are opaque and globally distinct, not measurements.
use super::*;
use flow_document::model::flow::Destination;
use flow_document::viewer::{ImagePixels, ImageRequest, ViewerEffect};
use std::sync::atomic::AtomicU64;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn next_id() -> Result<u64, ReadingSessionError> {
    crate::runtime_id::allocate(&NEXT_ID).ok_or(ReadingSessionError::Exhausted)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(u64);
impl RequestId {
    pub(super) fn new() -> Result<Self, ReadingSessionError> {
        next_id().map(Self)
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NavigationId(u64);
impl NavigationId {
    pub(super) fn new() -> Result<Self, ReadingSessionError> {
        next_id().map(Self)
    }
}

/// Lower values have higher scheduling priority. Hosts own the scheduler.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RequestPurpose {
    Foreground,
    History,
    Prefetch,
    Pagination,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentRequest {
    pub id: RequestId,
    pub publication: PublicationKey,
    pub revision: PublicationRevision,
    pub document: DocumentKey,
    pub purpose: RequestPurpose,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    Font,
    ImageMetadata,
    ImagePixels {
        desired_width: Option<u32>,
        desired_height: Option<u32>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceRequest {
    pub id: RequestId,
    pub publication: PublicationKey,
    pub revision: PublicationRevision,
    pub document: DocumentKey,
    pub resource: ResourceReference,
    pub kind: ResourceKind,
    pub purpose: RequestPurpose,
    pub source_base: Option<String>,
}

pub enum PreparedResource {
    Font(FontResource),
    ImageMetadata { width: u32, height: u32 },
    ImagePixels(ImagePixels),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NavigationTarget {
    pub publication: PublicationKey,
    /// `None` asks the host/registry for its selected revision.
    pub revision: Option<PublicationRevision>,
    pub target: PublicationTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedLink {
    Target(NavigationTarget),
    External(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestoreOutcome {
    Exact,
    Recovered,
    AnchorFallback,
    DocumentStartFallback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NavigationState {
    Pending,
    Ready { restored: Option<RestoreOutcome> },
    Failed(LoadFailure),
    Cancelled,
    External,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadFailure {
    message: String,
}
impl LoadFailure {
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        // Keep host failures bounded without splitting UTF-8.
        Self {
            message: message.chars().take(1024).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionEffect {
    ResolvePublication {
        id: RequestId,
        publication: PublicationKey,
        preferred_revision: Option<PublicationRevision>,
        allow_revision_change: bool,
    },
    LoadDocument(DocumentRequest),
    LoadResource(ResourceRequest),
    ResolveLink {
        id: RequestId,
        publication: PublicationKey,
        revision: PublicationRevision,
        document: DocumentKey,
        destination: Destination,
    },
    Reprioritize {
        id: RequestId,
        purpose: RequestPurpose,
    },
    CancelRequest(RequestId),
    /// More bounded CPU work is ready; the host chooses when to poll again.
    ContinueWork,
    /// Functional page-index data/readiness changed (not performance telemetry).
    PublicationPaginationChanged,
    OpenExternal(String),
    NavigationFinished {
        id: NavigationId,
        state: NavigationState,
    },
    Viewer(ViewerEffect),
}

impl SessionEffect {
    pub(super) fn request_id(&self) -> Option<RequestId> {
        match self {
            Self::ResolvePublication { id, .. }
            | Self::ResolveLink { id, .. }
            | Self::Reprioritize { id, .. } => Some(*id),
            Self::LoadDocument(request) => Some(request.id),
            Self::LoadResource(request) => Some(request.id),
            _ => None,
        }
    }
}

pub(super) enum PendingRequest {
    Publication {
        navigation: NavigationId,
        key: PublicationKey,
        revision: Option<PublicationRevision>,
        allow_change: bool,
    },
    Document {
        key: DocumentCacheKey,
        purpose: RequestPurpose,
        navigation: Option<NavigationId>,
    },
    Resource {
        key: DocumentCacheKey,
        reference: ResourceReference,
        kind: ResourceKind,
        purpose: RequestPurpose,
        navigation: Option<NavigationId>,
        images: Vec<ImageRequest>,
    },
    Link {
        navigation: NavigationId,
    },
}

impl PendingRequest {
    pub(super) fn purpose(&self) -> RequestPurpose {
        match self {
            Self::Document { purpose, .. } | Self::Resource { purpose, .. } => *purpose,
            _ => RequestPurpose::Foreground,
        }
    }
}
