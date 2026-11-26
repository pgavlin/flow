# flow_document

The single-document layer of [flow](https://github.com/pgavlin/flow).

- Imports Markdown, flow semantic XHTML, and EPUB XHTML into an immutable semantic document.
- Composes text and tables, with continuous and paginated layouts.
- Provides navigation, search, selection, and a host-driven document viewer.
- Paints through portable surfaces and paint targets.

## Parse Markdown

```rust
use flow_document::formats::markdown::{parse, MarkdownOptions};

let imported = parse("# Hello\n\nA **flow** document.", MarkdownOptions::default())?;
assert!(imported.document.text().as_str().contains("Hello"));
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Import budgets

Hosts can override construction budgets through:

- `MarkdownOptions.limits` (`MarkdownLimits`): AST nodes and depth, checked after parsing.
- `XhtmlParseOptions.limits`, `XhtmlOptions.limits`, and `EpubXhtmlOptions.limits`
  (`XhtmlLimits`): source bytes, nodes, depth, and attributes per element.
- `WebOptions.limits` and `MediaWikiOptions.limits` (`HtmlLimits`): source bytes,
  nodes, depth, attributes, accounted DOM storage, parser work, and source-tree payload.
  Web note snapshots and serialized math each also use `max_source_bytes`.
- HTML/XHTML adapter `max_table_columns`: table width and column-span normalization;
  `None` preserves 4,096, and zero is treated as one.

Defaults retain the existing budgets. These are accounting checks, not process-wide
memory limits or interruptible execution quotas. Representation limits and separate
IR-builder limits (such as table rows, cells, and placement work) still apply.

## Features

Default features are empty.

- `render`: text shaping, rasterization, settings, `DocumentViewer`, and
  document/layout cache codecs.
- `serde`: serialization of core text-position evidence and fingerprints.
- `web-html`: experimental host-fed web article extraction into the flow IR;
  no networking. Available without rendering; see `formats::web_html` for limits.
- `mediawiki-html`: host-fed expanded Parsoid HTML 2.8.0, including page metadata,
  sections, responsive images, and fixture-backed Cite/Math semantics. Independent
  of `web-html` and Readability; no fetching or wikitext expansion. Document and
  explicit body-fragment modes are available without rendering. See
  `formats::mediawiki_html` and the `import_mediawiki` example.
- `html-parser`: internal HTML5 parsing infrastructure, independently testable
  without either adapter; not a public generic HTML importer.
- `telemetry`: tracing spans and events using the host's subscriber.
- `test-fonts`: repository fixture fonts for testing hosts, loaded at runtime.
  Fonts are not included in published crates. Outside the repository, set
  `FLOW_TEST_FONT_DIR` to a directory containing the repository's font fixtures.

Hosts register fonts explicitly and may supply ordered family preferences;
default preference lists are empty. On WebAssembly, hosts also register
Tree-sitter language modules explicitly.

Engine code is MIT licensed. Bundled test fonts use the Bitstream Charter and
SIL Open Font licenses; see the [font notices](https://github.com/pgavlin/flow/blob/main/crates/flow_document/tests/fixtures/fonts/README.md).
