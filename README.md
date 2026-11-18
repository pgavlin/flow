# flow 🌊

flow is a Rust engine for reading reflowable documents. It imports Markdown and
XHTML into a semantic document model, lays out and paints text, and provides
navigation, search, selection, and pagination. A format-neutral publication
layer coordinates reading sessions across documents. An optional adapter for
importing EPUBs into the publication and document model is also available.

| Crate | Responsibility |
| --- | --- |
| [`flow_cache`](crates/flow_cache) | Shared low-level cache transport used by engine codecs |
| [`flow_document`](crates/flow_document) | One document: import, semantic model, layout, interaction, and painting |
| [`flow_publication`](crates/flow_publication) | Format-neutral publication model, host-driven sessions, saved locations, and optional EPUB adapter |

flow is not itself a reader application or a publication library/catalog. Applications
own UI, font acquisition, networking, storage, threading, platform lifecycle,
and synchronization.

## Try it

```sh
cargo run -p flow_document --example parse_markdown
cargo run -p flow_document --features render --example render_document -- /tmp/flow.png
cargo run -p flow_publication --features epub --example inspect_epub -- /path/to/book.epub
cargo run --locked --manifest-path examples/reader/Cargo.toml -- /path/to/book.epub
```

The [tiny demo reader](examples/reader/README.md) uses winit and CPU presentation to host
the reader. Its README includes browser build instructions.

## Development

```sh
cargo lint
cargo lint --all # also run the feature matrix and lint regression suites
cargo test --workspace --no-default-features
cargo test --workspace --features render
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

Capability lints help keep external interactions in hosts. Run `cargo lint --all` from
the repository root. The [lint runner](tools/xtask/README.md) is host tooling, not an
engine dependency.

Tests use crate-local fixtures and explicit fonts, not installed system fonts
or a private EPUB corpus. Rendering tests are exercised on native hosts;
WebAssembly and Android runtime validation require separate host harnesses.

See [Contributing](CONTRIBUTING.md) for the release process.

## License

Engine code is MIT licensed. Test fonts have their own licenses; see
[font notices](crates/flow_document/tests/fixtures/fonts/README.md).
