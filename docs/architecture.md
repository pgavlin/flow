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
select lexical policy or a source format's semantic conventions. Its optional
`html-parser` feature supplies a budgeted HTML5 document parser and DOM checks
using `html5ever` and `dom_query`, independently of Readability. Parser errors,
budgets, and regression tests live in this shared layer; article extraction,
normalization, and DOM-to-source-tree adaptation remain web-adapter policy.
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
bytes are bounded, and a budgeted HTML5 tree sink checks node allocations,
logical depth (including detached template fragments), attributes, accounted DOM
storage, and sink operations before mutation. Rejected creations use inert named
handles until the current bounded tokenizer batch finishes; failed trees never
become import results. Batch boundaries check only the sink failure flag, not
the accumulated DOM. A single linear audit after EOF processing shares the
construction work quota; its cost is traced as `flow.html.validate`. Independent
DOM checks remain available for adapters that subsequently transform the DOM. Subtree reparenting cannot bypass depth checks. These are
DOM construction budgets, not a process-wide allocator quota: tokenizer scratch,
allocator capacity/overhead, and native extraction's internal execution time are
not directly interruptible by this boundary.
Before extraction it preserves TeX and MathML, math scripts, static checkbox
state, and legacy anchor targets. Explicitly paired visual/semantic formula
representations become one math node. Links resolve against the HTML base URL,
including fragment-only references, without applying relative bases twice.
Image links retain their transparent link semantics and metadata; figure
lowering preserves arbitrary flow content instead of only direct images and
paragraphs. Preformatted content retains literal whitespace, explicit breaks,
links, and anchors instead of flattening the first code child. Media placeholders
retain inline and block fallback content. Referenced local notes use bounded
parsed-node snapshots rather than an unbudgeted HTML reparse. Restoration checks
total arena capacity and depth before copying, and follows retained references
transitively. Explicitly hidden descendants and remote note bodies are not
resurrected; inline visibility parsing handles CSS comments, escapes, and
`!important` priority without attempting stylesheet layout. Missing targets are
diagnosed.
Images retain a deterministic single-resource fallback for existing renderers,
plus optional responsive choices: ordered conditional sources with media/type
hints and separate fallback-image candidates, width/density descriptors, and
unevaluated `sizes` expressions. The web adapter snapshots picture sources before
extraction rewrites their wrappers, recovers lazy-image candidates afterwards,
and resolves each resource against the HTML base. An explicit image `src`
participates as an implicit 1x density candidate when appropriate; synthesized
fallbacks do not. Invalid candidates are omitted with diagnostics without losing
valid neighbors. Shared lowering only invokes an adapter-selected hook; legacy
XML adapters retain single-resource behavior. Builder validation covers all
candidate resource references and nonempty, consistently described candidate
sets with positive, unique descriptors. Semantic cache codec version 4 preserves
these choices and generated link-label provenance. Selection, condition evaluation, and resource fetching remain
consumer/host policy; existing renderers continue using the fallback.
Imported image dimensions are used per node without eagerly requesting metadata
for every sized image. Display-sized pixel replies retain other cached sizes and
other images; only replacing a resource\'s source pixels invalidates its derived
variants. This is derived-cache correctness, not HTTP caching policy. Composition
codec version 3 invalidates artifacts from the previous dimension behavior.
Rejected image resources retain their alternative text. Display math and other flow
content are split out of phrasing wrappers rather than flattened or inserted into
invalid inline IR. Layout-table conversion requires explicit presentation intent;
nesting and paragraph-containing cells are not sufficient evidence.
Adapter policy hooks preserve ruby grids, inline roles, titles, sections,
checkboxes, optional breaks, bidi isolation/overrides, inline metadata, note
relationships/roles, flow-content links, table scope/header associations, and
row-group-spanning zero row spans without changing the legacy XML adapters'
default lowering behavior.
Malformed ruby and specialized list/table structures recover to readable content
with diagnostics. Original-element identities survive extraction and repair
carriers without inventing serialized ranges. A Git submodule pins the upstream
Readability corpus: all 130 cases run as an offline IR-construction test, with
strict semantic comparisons for a selected subset and a full-corpus difference
report separating native extraction differences from pipeline differences.
Submodule initialization is an explicit development/CI setup step, never an
importer or test-time network operation. Remaining hardening includes
global allocation accounting and interruptible extraction-work quotas.
Responsive selection/rendering is separate from preserving alternatives in IR.

The optional `mediawiki-html` adapter is an independent Parsoid DOM 2.8.0
pipeline, enabling only the shared HTML5 parser, URL handling, and JSON metadata
dependencies—not `web-html` or Readability. It supports explicit document and
body-fragment modes; fragments require a caller-supplied expected version.
Document version declarations must agree with one another and any expected
version. It returns typed database-title, category/sort-key, language-link, and
redirect metadata separately from the reading tree. No redirect following,
wikitext/template expansion, CSS execution, or resource acquisition occurs.
The host supplies document URL separately from identity. An explicit resource
base overrides a document base resolved against that URL; fragment-only links
remain local. Relative references stay relative without a usable base.
MediaWiki-owned DOM adaptation preserves expanded content and responsive image
choices, interprets RDFa tokens with prefix rebinding, and confines Cite/Math
interpretation to supported extension boundaries. Unknown extensions fall back
to ordinary HTML with diagnostics. Script/style/template and explicit HTML
`hidden` subtrees are omitted; arbitrary CSS visibility is not evaluated.
Cite call-site anchors, note-body roles, and multiple backlinks survive; missing
local targets are diagnosed. Math uses documented `data-mw.body.extsrc` TeX and
display metadata, consuming its fallback subtree once. Mixed phrasing/flow
extension output is split locally while retaining anchors, language, and inline
runs. Empty external autonumber links receive article-order labels with
`GeneratedReason::LinkLabel` provenance; semantic codec version 4 covers the new
reason. An offline Wikipedia revision capture pins real Parsoid 2.8.0 Cite/Math
shapes (including 100 formulas), with attribution/license notes; it is excluded
from the published crate. Synthetic contract tests cover recovery and incomplete
markup, and a rendering/cache test exercises imported semantics. This is not a
claim of support for all extensions or browser-equivalent wiki presentation.

Constructed trees can retain original node identities, omit unknown serialized
ranges, and associate generated nodes with their original constructs. HTML
provenance refers to the original `text/html` source, never a normalized
serialization. Default construction budgets bound nodes, depth, attributes, and
text/attribute data while adapting a DOM; each parser integration must also
bound its own input and construction work. XML retains its existing lexical limits.
Publication resource resolution remains in `flow_publication`. The example
reader explicitly selects MediaWiki import for saved `.mw.html` files and
Parsoid response profiles. Its shared desktop/browser host URL resolver requests
Parsoid HTML for Wikipedia article URLs (including revision requests), retaining
the article URL as the source/link base and honoring known initial fragments.
Its opt-in desktop `telemetry` feature installs a host-owned subscriber for
engine/host phase spans and records queue wait plus window/open-to-first-present
milestones. Default reader builds install no subscriber; the engine only emits
feature-gated spans and never configures logging or export.
Other hosts are not guessed to be MediaWiki; ordinary HTML keeps its web
extraction path. This provider-specific acquisition never runs in either importer.

With `render`, text shaping and paragraph composition produce continuous
layouts and page fragments. `DocumentViewer` coordinates presentation,
navigation, search, selection, resources, and invalidation. The font stack uses
Fontique, HarfRust, Skrifa, and Swash. Paragraph breaking uses `text_layout`;
syntax highlighting uses Tree-sitter.

Navigation construction gathers anchors once into node/position groups, retaining
exact-name alias order. Per-construction geometry lookups use node maps and
interval sweeps followed by binary searches, rather than scanning the whole
layout for every target. Overlapping/interleaved table ranges retain their first
visual-order match; following-line fallback still uses canonical source order.
These transient lookups do not change IR persistence. The viewer constructs
stable semantic navigation once when installing a document, clears only obsolete
coordinates/generated actions when composition changes, and binds geometry when
continuous placement becomes available. Labels, identities, anchor aliases, and
owned-node lists survive reflow; layout-generated note-return targets are refreshed
only from actions present in the new layout. The semantic index and its bound
geometry travel with suspended viewer sessions. Table-of-contents construction
shares the anchor lookup.

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
