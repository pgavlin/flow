//! Render-independent durable reading locations. Hosts choose serialization and acquisition.

use crate::publication::{
    DocumentKey, ModelError, PublicationKey, PublicationRevision, check_string,
};
use flow_document::{
    model::flow::AnchorId,
    position::{StableTextPosition, TextFingerprint},
};

pub const PERSISTENT_LOCATION_VERSION: u32 = 2;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentReadingLocation {
    pub version: u32,
    pub publication: PublicationKey,
    pub publication_revision: PublicationRevision,
    pub document: DocumentKey,
    pub text_fingerprint: TextFingerprint,
    pub position: StableTextPosition,
    pub anchor_hint: Option<AnchorId>,
}

impl PersistentReadingLocation {
    pub fn validate(&self) -> Result<(), LocationError> {
        if self.version != PERSISTENT_LOCATION_VERSION {
            return Err(LocationError::UnsupportedVersion(self.version));
        }
        if !self.position.is_bounded() {
            return Err(LocationError::Limit);
        }
        if let Some(anchor) = &self.anchor_hint {
            check_string(anchor.as_str())?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum LocationError {
    #[error("unsupported reading-location schema version {0}")]
    UnsupportedVersion(u32),
    #[error("reading-location evidence exceeds its limits")]
    Limit,
    #[error(transparent)]
    Model(#[from] ModelError),
}
