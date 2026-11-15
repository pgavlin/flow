//! Disposable cache artifacts.
//!
//! Single-document semantic/layout codecs require `render`. Low-level framing
//! and binary transport live in the shared `flow_cache` support crate, not this
//! document API. Storage hosts retain encoded artifacts as opaque bytes.
//!
//! ```compile_fail
//! use flow_document::cache_codec::transport;
//! ```

pub use flow_cache::{CacheCodecError, DependencyDigest};

#[cfg(feature = "render")]
mod document;
#[cfg(feature = "render")]
pub use document::{
    COMPOSITION_CODEC_VERSION, CacheCodecLimits, CacheDisposition, PAGINATION_CODEC_VERSION,
    SEMANTIC_CODEC_VERSION, composition_or_recompute, decode_composition, decode_pagination,
    decode_semantic, encode_composition, encode_pagination, encode_semantic,
    pagination_or_recompute, semantic_or_recompute,
};
