//! Generic, versioned transport for disposable cache artifacts.
//!
//! Implementation support shared by the engine codec owners. This crate knows
//! no document, layout, or publication schemas: owners provide a kind/version
//! pair, an exact dependency digest, and a byte bound. Public Rust functions
//! enable cross-crate sharing; binary serialization internals remain private.
//!
//! Envelope checks detect corruption and stale dependencies, not authenticity.
//! Decoded values still require their owner's structural and semantic validation.
//! Storage hosts should persist returned bytes opaquely, not interpret framing.

mod binary;

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

// Version-one cache envelope signature.
const MAGIC: &[u8; 8] = b"FLWCACH1";
const HEADER_LEN: usize = 88;

/// Caller-owned wire kind and payload version. Owners must avoid kind collisions;
/// changing a payload's schema requires a new version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactFormat {
    pub kind: u8,
    pub version: u32,
}

/// Digest of the exact inputs required to reuse an artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DependencyDigest(pub [u8; 32]);

impl DependencyDigest {
    pub fn for_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
}

/// Rejection of framing, payload encoding, or owner-supplied artifact validation.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CacheCodecError {
    #[error("cache artifact exceeds its configured byte limit")]
    ByteLimit,
    #[error("cache artifact is truncated")]
    Truncated,
    #[error("cache artifact has an invalid magic value")]
    Magic,
    #[error("cache artifact kind does not match the requested artifact")]
    Kind,
    #[error("cache artifact version is unsupported")]
    Version,
    #[error("cache artifact dependency digest does not match")]
    Dependency,
    #[error("cache artifact checksum does not match")]
    Checksum,
    #[error("cache payload is malformed")]
    Malformed,
    #[error("cache artifact exceeds a structural limit")]
    StructuralLimit,
    #[error("cache artifact failed validation")]
    Validation,
}

/// Wraps an opaque payload with version, dependency, length, and checksum checks.
/// The byte limit includes the envelope. Payload interpretation belongs to its owner.
pub fn encode_payload(
    format: ArtifactFormat,
    dependency: DependencyDigest,
    payload: &[u8],
    max_encoded_bytes: usize,
) -> Result<Vec<u8>, CacheCodecError> {
    let total = HEADER_LEN
        .checked_add(payload.len())
        .ok_or(CacheCodecError::ByteLimit)?;
    if total > max_encoded_bytes {
        return Err(CacheCodecError::ByteLimit);
    }
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(MAGIC);
    bytes.push(format.kind);
    bytes.extend_from_slice(&[0; 3]);
    bytes.extend_from_slice(&format.version.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&dependency.0);
    bytes.extend_from_slice(blake3::hash(payload).as_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

/// Checks the entire envelope before returning a borrowed payload.
/// Trailing bytes, incomplete payloads, stale dependencies, and unsupported
/// kind/version pairs are rejected.
pub fn decode_payload(
    bytes: &[u8],
    format: ArtifactFormat,
    dependency: DependencyDigest,
    max_encoded_bytes: usize,
) -> Result<&[u8], CacheCodecError> {
    if bytes.len() > max_encoded_bytes {
        return Err(CacheCodecError::ByteLimit);
    }
    if bytes.len() < HEADER_LEN {
        return Err(CacheCodecError::Truncated);
    }
    if &bytes[..8] != MAGIC {
        return Err(CacheCodecError::Magic);
    }
    if bytes[8] != format.kind {
        return Err(CacheCodecError::Kind);
    }
    if bytes[9..12] != [0; 3] {
        return Err(CacheCodecError::Malformed);
    }
    let encoded_version = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    if encoded_version != format.version {
        return Err(CacheCodecError::Version);
    }
    let payload_len_u64 = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    let payload_len = usize::try_from(payload_len_u64).map_err(|_| CacheCodecError::ByteLimit)?;
    let expected_len = HEADER_LEN
        .checked_add(payload_len)
        .ok_or(CacheCodecError::ByteLimit)?;
    if expected_len > max_encoded_bytes {
        return Err(CacheCodecError::ByteLimit);
    }
    if bytes.len() != expected_len {
        return Err(CacheCodecError::Truncated);
    }
    if bytes[24..56] != dependency.0 {
        return Err(CacheCodecError::Dependency);
    }
    let payload = &bytes[HEADER_LEN..];
    if bytes[56..88] != *blake3::hash(payload).as_bytes() {
        return Err(CacheCodecError::Checksum);
    }
    Ok(payload)
}

/// Serializes a positional binary Serde payload and wraps it in the envelope.
/// Field and variant order are part of the owner's versioned wire schema.
/// This is a statically typed transport, not a general-purpose Serde format:
/// sequences/maps need known lengths, and collection elements must occupy at
/// least one payload byte. Dynamic `deserialize_any` schemas are unsupported.
pub fn encode_binary<T: Serialize>(
    format: ArtifactFormat,
    dependency: DependencyDigest,
    value: &T,
    max_encoded_bytes: usize,
) -> Result<Vec<u8>, CacheCodecError> {
    let payload = binary::to_vec(value).map_err(|_| CacheCodecError::Malformed)?;
    encode_payload(format, dependency, &payload, max_encoded_bytes)
}

/// Checks the envelope, then deserializes a positional binary Serde payload.
/// Lengths are checked against remaining payload bytes and no eager collection
/// size hint is supplied. The owner must additionally validate schema-specific
/// limits and semantics before returning the value to its host.
pub fn decode_binary<T: DeserializeOwned>(
    bytes: &[u8],
    format: ArtifactFormat,
    dependency: DependencyDigest,
    max_encoded_bytes: usize,
) -> Result<T, CacheCodecError> {
    let payload = decode_payload(bytes, format, dependency, max_encoded_bytes)?;
    binary::from_slice(payload).map_err(|_| CacheCodecError::Malformed)
}

#[cfg(test)]
mod tests;
