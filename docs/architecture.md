# Architecture

flow has two engine layers:

```text
host application
      |
flow_publication
      |
flow_document
```

A single-document host can use `flow_document` alone. Both engine layers use
`flow_cache` for cache transport; the shared crate does not depend on either
engine layer.

## Document layer

Format importers lower Markdown, [flow semantic XHTML](semantic-xhtml.md), and
EPUB XHTML into `FlowDocument`, an immutable semantic model with logical text,
nodes, styles, resources, and provenance. Layout and interaction indexes derive
from that model.

`formats::xhtml` supplies strict XML parsing and validation-profile support.
Its public source-tree names remain available as compatibility re-exports.
Internal `formats::html` supplies the shared syntactic tree, bounded DOM
construction, source provenance, and structural HTML lowering. It does not
select lexical policy or a source format's semantic conventions.
`formats::flow_xhtml` owns the flow format's versions and conventions;
`formats::epub_xhtml` owns complete-document validation, author stylesheet
cascading, and semantic import. Adapters explicitly select their content root,
namespace policy, and supported source conventions through narrow internal
hooks. Shared lowering does not recognize flow-, EPUB-, or MediaWiki-specific
attributes or classes. HTML adapters can construct trees directly and lower to
the flow IR without serializing XML or going through the flow XHTML vocabulary.
The optional `web-html` adapter parses host-supplied HTML5 with a native
Readability implementation, performs article extraction and normalization, and
lowers its DOM directly through this boundary. It returns article metadata
separately and performs no networking. This importer is experimental: its input
bytes are bounded, but third-party DOM allocation precedes node/depth checks.

Constructed trees can retain original node identities, omit unknown serialized
ranges, and associate generated nodes with their original constructs. HTML
provenance refers to the original `text/html` source, never a normalized
serialization. Default construction budgets bound nodes, depth, attributes, and
text/attribute data while adapting a DOM; a future parser integration must also
bound its own input and allocations. XML retains its existing lexical limits.
Publication resource resolution remains in `flow_publication`.

With `render`, text shaping and paragraph composition produce continuous
layouts and page fragments. `DocumentViewer` coordinates presentation,
navigation, search, selection, resources, and invalidation. The font stack uses
Fontique, HarfRust, Skrifa, and Swash. Paragraph breaking uses `text_layout`;
syntax highlighting uses Tree-sitter.

Hosts register font bytes. They answer viewer image-resource requests and
supply surfaces or paint targets. Rendering returns host-readable pixel
buffers; hosts choose output encodings and own file writes. PNG export lives in
the example host, not the engine API. The engine does not require a window
system.

## Publication layer

`flow_publication` owns an immutable, format-neutral publication model and
`ReadingSession`. The model separates document/resource catalogs, normalized
sequential reading order, and typed navigation. Repeated references are
skipped. Sessions coordinate host-driven loading, bounded residency, history,
scoped resources, background pagination, and durable locations based on
canonical-text evidence. They contain no source acquisition or format routing.

The optional `formats::epub` adapter parses supplied archive bytes and
translates EPUB package semantics into that model. Hosts retain adapters and
fulfill session requests. Model/location serialization needs neither rendering
nor ZIP. Pagination is derived state, not location identity.

## Cache artifacts

`flow_document::cache_codec` owns semantic, composition, and pagination codecs.
`flow_publication::cache_codec` owns publication page-index artifacts,
versions, and reading-order validation/limits.

The lower-level `flow_cache` support crate owns framing, checksums, dependency
digests, byte bounds, and binary payload transport. It knows no document or
publication schemas and has no engine, rendering, font, or telemetry
dependency. Its functions are public for sharing between Rust crates, not
re-exported as a general-purpose transport API from the document or publication
layer. The binary serializer remains private, and artifact owners perform their
own validation.

Publication page-index codecs are always available without rendering or fonts;
document/layout codecs and persistent artifact support are included in
`render`. Storage hosts keep encoded artifacts opaque and let engine codecs
accept or reject them. These caches are disposable optimizations, not durable
reading locations.

## Host boundary

Hosts own application chrome, input bindings, platform lifecycle, threads,
networking, storage policy, configuration parsing, tracing
subscribers/exporters, progress chrome, catalogs, accounts, downloads, and
synchronization. The viewer keeps retained state private; publication sessions
expose only coordinated mutations and an immutable viewer view.
