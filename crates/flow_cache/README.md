# flow_cache

Low-level implementation support shared by flow's document and publication
codecs. It provides versioned framing, checksums, dependency digests, byte bounds,
and positional binary Serde transport.

This crate has no document, publication, rendering, font, or telemetry dependency.
It owns no artifact schemas: the engine codec owners select kinds and versions,
validate structure and semantics, and expose their domain APIs. Storage hosts
keep encoded artifacts opaque and should use those owner APIs rather than this
transport directly.

Binary serialization internals remain private. Checksums detect corruption,
not authenticity.
