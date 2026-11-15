# flow semantic XHTML fixtures

These synthetic XHTML fixtures are covered by the repository's MIT license.
Resource references use `example.test` hosts and relative paths; tests supply
resources locally.

## Focused contract cases

Each file tests one feature area of [flow semantic XHTML](../../../../../docs/semantic-xhtml.md).

| File in `contract/` | Coverage |
| --- | --- |
| `serialization.xhtml` | UTF-8, XML entities, numeric references, CDATA, comments, processing instructions, multiple top-level elements |
| `namespace.xhtml` | Optional XHTML namespace and `xml:lang` |
| `blocks.xhtml` | Paragraphs, all six heading levels, all grouping containers, nested quotations, thematic breaks, explicit anchors |
| `inline.xhtml` | Every defined inline role, neutral spans, time metadata, hard breaks |
| `metadata.xhtml` | Language inheritance, override, reset, `lang` precedence, all direction values, inline metadata |
| `whitespace.xhtml` | Collapse across inline boundaries, paragraph/break boundaries, nonbreaking spaces, inline-code whitespace |
| `links.xhtml` | Local and percent-encoded anchors, relative documents/fragments, external and mail URIs, linkless anchors, linked images |
| `lists.xhtml` | Ordered/unordered/nested lists, defaults, reversed numbering, explicit starts and item ordinals, signed values |
| `definitions.xhtml` | Multiple terms and definitions per group, multi-paragraph definitions |
| `preformatted.xhtml` | Exact spacing/newlines, plain/code distinction, unspecified language, first language token, escaped code, Mermaid source |
| `images.xhtml` | Meaningful/decorative alternatives, integer/fractional size hints, remote/relative identities, repeated resources |
| `figures.xhtml` | Caption before/after content, multi-paragraph captions, images and block content |
| `tables.xhtml` | Captions, all row-group kinds, implicit body rows, header semantics, row/column spans, occupied-cell placement |
| `math.xhtml` | Inline/display TeX, XML escaping, authoritative source replacing synthetic presentation children |
| `disclosures.xhtml` | Open/closed state, phrasing summaries, nested block bodies |
| `media.xhtml` | All three media kinds, direct/child source precedence, titles, size hints, block fallback, ignored track metadata |

The contract corpus collectively covers every specified element. Tests assert
semantic structure, canonical text, attributes, destinations, and diagnostics.
Expected media-placeholder diagnostics are listed in the test inventory.

## Regression cases

`regressions/` contains separately authored, deliberately nonconforming or
normalization-focused inputs:

- `table-spans.xhtml`: a span crossing a row-group boundary and placement after
  an active row span;
- `orphan-table-parts.xhtml`: detached table structure retaining readable content
  and an image, without inventing a table;
- `root-code.xhtml`: a root-level code element receiving a preformatted wrapper;
- `invalid-metadata.xhtml`: duplicate IDs, invalid language, missing image
  alternatives, and invalid/incomplete size hints;
- `anonymous-paragraphs.xhtml`: bare block-boundary text and paragraph-wrapped
  terms/summaries;
- `ignored-attributes.xhtml`: presentation/event attributes, missing time
  metadata, class-only pseudo-math, ignored titles, and nonnumeric/nonfinite
  image dimensions.

These structures are tested recovery behavior, not additions to the authoring
contract. Separate parser and semantic tests cover malformed XML, unsupported
versions, and rejected child structures.

## Whole-document examples

`examples/` combines features into complete, fictional documents:

- `garden-survey.xhtml`: a survey with sections, lists, mathematics, a figure,
  measurements table, disclosure, quotation, and cross-references;
- `workshop-manual.xhtml`: a manual with definitions, nested instructions, code,
  diagram source, a figure, video fallback, disclosure, and time metadata;
- `field-notes.xhtml`: multilingual notes with namespace/language/direction
  metadata, local navigation, a table, preformatted text, and audio fallback.

All three undergo semantic checks and, with `render`, composition, pagination,
and painting at narrow and wide viewport sizes using explicitly registered test
fonts. Images and media remain host-supplied resources; these tests do not fetch
them.

## Inventory

A single test-only inventory in `src/formats/flow_xhtml/fixtures.rs` supplies syntax,
semantic, and rendering tests. A directory-inventory test ensures every checked-in
XHTML fixture participates. All fixtures are tested at both supported format
versions, with exact expected diagnostic kinds. Add new feature cases to that
inventory and add behavioral assertions alongside them.
