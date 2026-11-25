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

## Features

Default features are empty.

- `render`: text shaping, rasterization, settings, `DocumentViewer`, and
  document/layout cache codecs.
- `serde`: serialization of core text-position evidence and fingerprints.
- `web-html`: experimental host-fed web article extraction into the flow IR;
  no networking. Available without rendering; see `formats::web_html` for limits.
- `telemetry`: tracing spans and events using the host's subscriber.
- `test-fonts`: bundled fixture fonts for testing hosts.

Hosts register fonts explicitly and may supply ordered family preferences;
default preference lists are empty. On WebAssembly, hosts also register
Tree-sitter language modules explicitly.

Engine code is MIT licensed. Bundled test fonts use the Bitstream Charter and
SIL Open Font licenses; see the [font notices](https://github.com/pgavlin/flow/blob/main/crates/flow_document/tests/fixtures/fonts/README.md).
