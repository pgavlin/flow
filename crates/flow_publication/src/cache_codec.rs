//! Versioned, disposable publication cache artifacts.
//!
//! Publication page indexes belong to this layer, not the single-document
//! engine. Encoded artifacts are opaque storage bytes, not durable reading
//! locations. Hosts supply the exact publication/layout dependency digest;
//! this codec checks it and validates the decoded index before use.

use flow_cache::{self as transport, ArtifactFormat};
pub use flow_cache::{CacheCodecError, DependencyDigest};
use serde::{Deserialize, Serialize};

pub const PUBLICATION_INDEX_CODEC_VERSION: u32 = 1;

// Keep the established wire kind and version so ownership changes do not
// invalidate otherwise reusable cache bytes.
const PUBLICATION_INDEX_FORMAT: ArtifactFormat = ArtifactFormat {
    kind: 4,
    version: PUBLICATION_INDEX_CODEC_VERSION,
};

/// One optional page count per normalized sequential document. None means not yet paginated;
/// present counts must be nonzero. Counts are derived layout state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublicationPageIndexArtifact {
    pub pages: Vec<Option<u32>>,
}

/// Publication-specific bounds, independent of document block/node limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationIndexCodecLimits {
    pub max_encoded_bytes: usize,
    pub max_reading_order_entries: usize,
}

impl Default for PublicationIndexCodecLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 64 * 1024 * 1024,
            max_reading_order_entries: 1_000_000,
        }
    }
}

/// Validates and encodes a publication's page index into opaque cache bytes.
pub fn encode_publication_page_index(
    index: &PublicationPageIndexArtifact,
    dependency: DependencyDigest,
    limits: PublicationIndexCodecLimits,
) -> Result<Vec<u8>, CacheCodecError> {
    validate_publication_page_index(index, limits)?;
    transport::encode_binary(
        PUBLICATION_INDEX_FORMAT,
        dependency,
        index,
        limits.max_encoded_bytes,
    )
}

/// Checks framing, dependencies, schema bounds, and the exact normalized reading-order length.
pub fn decode_publication_page_index(
    bytes: &[u8],
    dependency: DependencyDigest,
    expected_reading_order_entries: usize,
    limits: PublicationIndexCodecLimits,
) -> Result<PublicationPageIndexArtifact, CacheCodecError> {
    let _span = flow_document::telemetry::phase_span!(
        "flow.cache.decode",
        artifact_kind = PUBLICATION_INDEX_FORMAT.kind as u64,
        encoded_bytes = bytes.len() as u64,
    );
    let index = transport::decode_binary(
        bytes,
        PUBLICATION_INDEX_FORMAT,
        dependency,
        limits.max_encoded_bytes,
    )?;
    validate_publication_page_index(&index, limits)?;
    if index.pages.len() != expected_reading_order_entries {
        return Err(CacheCodecError::Validation);
    }
    Ok(index)
}

fn validate_publication_page_index(
    index: &PublicationPageIndexArtifact,
    limits: PublicationIndexCodecLimits,
) -> Result<(), CacheCodecError> {
    if index.pages.len() > limits.max_reading_order_entries
        || index.pages.iter().flatten().any(|pages| *pages == 0)
    {
        return Err(CacheCodecError::StructuralLimit);
    }
    index
        .pages
        .iter()
        .flatten()
        .try_fold(0u64, |total, pages| total.checked_add(*pages as u64))
        .ok_or(CacheCodecError::StructuralLimit)?;
    Ok(())
}

#[cfg(test)]
mod tests;
