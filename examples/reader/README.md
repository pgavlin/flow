# Tiny flow reader host

One window (or browser canvas), one open publication, continuous or paginated reading.
Opens Markdown, **flow semantic XHTML**, EPUB, expanded MediaWiki/Parsoid DOM,
and experimental web articles.
No library, persistence, TOC/search/settings panels, or application framework.

## Desktop

```sh
cargo run --locked --manifest-path examples/reader/Cargo.toml
cargo run --locked --manifest-path examples/reader/Cargo.toml -- /path/to/book.epub
cargo run --locked --manifest-path examples/reader/Cargo.toml -- https://example.org/article
```

Targets macOS, Windows and Linux (X11/Wayland). Native rendering-host tests run
without a display; a desktop session is needed to run the window/file picker.
Linux's picker needs an XDG desktop portal backend or Zenity at runtime.

- Ctrl/Cmd+O: native picker; dropping a file also opens it.
- P: toggle continuous/paginated reading (also a browser toolbar button).
- Wheel, Space, Page Up/Down, Up/Down: scroll, or turn pages in paginated mode.
- Tab/Shift+Tab: keyboard focus through document targets, including visible wide
  tables and code blocks. Escape clears focus; pointer input does not acquire it.
- Left/Right: scroll a focused wide-content viewport (including at its limits);
  otherwise previous/next page in paginated mode or sequential document.
- Horizontal wheel/trackpad input scrolls the hovered wide-content viewport.
- Shift+Space: scroll/turn backward.
- Alt+Left or Backspace: back after following a link.
- Ctrl/Cmd +/−: font size.

Opening another file or URL replaces the current publication and its history. Document
navigation within an open EPUB is transactional through `ReadingSession`.
Page turns cross EPUB document boundaries: forward to the next document's first
page, backward to the previous document's last page. The title/status shows the
current document and its local page count, not a whole-book page number.
Resizing and changing text size repaginate; changing mode preserves the text anchor.

## Web (no bundler or JS framework)

Install the Rust target, a clang/LLVM toolchain with WebAssembly support, and the
matching wasm-bindgen CLI:

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.129
# macOS: brew install llvm; Apple's system clang lacks the needed WASM backend.
# export CC_wasm32_unknown_unknown="$(brew --prefix llvm)/bin/clang"
# export AR_wasm32_unknown_unknown="$(brew --prefix llvm)/bin/llvm-ar"
cargo build-web                         # optimized; --dev for faster builds
python3 -m http.server 8080 --directory examples/reader/target/web
```

Open <http://localhost:8080>. Use Open, Back, previous/next, and text-size buttons;
the focused canvas also accepts the desktop keys. Touch drag scrolls in continuous
mode; a horizontal or vertical swipe turns one page in paginated mode. Horizontal
swipes starting in a wide table/code block scroll that content instead, even at
its limits. Split fragments share their horizontal offset across pages. Tap follows
links. Use the URL field to fetch a web location; cross-origin servers must allow
CORS. JavaScript streams a bounded response into WASM with its final URL and
Content-Type. Selected local files are never uploaded to a server. Serve over
HTTP(S), not `file:`. No cross-origin-isolation headers needed.

An uploaded EPUB can supply its own images/fonts. Standalone browser files have
no access to neighboring files; embedded base64 data images work. Directory
picking and cross-file standalone links are deliberately out of scope. Remote
image loading is currently desktop-only; browser URL imports show article text
and embedded data images. Browser code blocks are plain text unless a host registers WASM grammar
modules; this demo does not load them. The canvas is not a screen-reader text view.

## Host boundary and limits

`winit` owns events/windows; `softbuffer` presents CPU pixels. `book.rs` owns
adapter selection, acquisition, reference policy and image decoding. The desktop
uses one bounded input worker; browser jobs run on the event loop (large imports
can briefly block it). Generations and engine request IDs reject stale results.
There is no background prefetch or whole-book pagination. Desktop HTTP uses a
blocking client on the input worker; browser fetching uses the platform Fetch API.

Fonts are explicitly supplied from the repository's licensed Literata/Fira Code
fixtures, not discovered from the system. See
[font notices](../../crates/flow_document/tests/fixtures/fonts/README.md). The
small bundled set is not comprehensive multilingual coverage.

Demo limits: 64 MiB input, 16 MiB standalone source/resource, 4096×4096 source
images with a 64 MiB decoder budget, 4 megapixel decoded variants, 16 megapixel
window. Desktop standalone assets must remain inside the selected document's
canonical directory; assume those files remain unchanged while open. External
activation allows HTTP(S) only. Desktop documents opened from web URLs may fetch
HTTP(S) images; local files do not gain that capability. Fetches omit credentials,
limit redirects to five on desktop, time out after 30 seconds, and enforce byte
limits. This demo is not an SSRF isolation boundary: explicitly opening web content
allows requests from the host's network.

## Host-owned HTML routing

Wikipedia article/view URLs automatically request Parsoid HTML through the
wiki's `/api/rest_v1/page/html/{title}` endpoint. Desktop and browser shells use
the same Rust URL resolver. It recognizes the actual Wikipedia hostname
(including language/mobile subdomains), not substring lookalikes; it also checks
the destination of an ordinary HTTP redirect. `oldid` requests fetch the given
revision, and known fragment targets become the initial reading location.
The article URL—not the API URL—is supplied as the document's link/resource
base. Edit, diff, search, and other non-article URL forms are not rewritten.
Direct Wikipedia REST HTML URLs keep their request URL but use an article base.
Fetch/import errors are surfaced; there is no silent fallback to Readability.
Desktop HTTP connections are pooled. A per-open-book, byte-bounded encoded
resource cache avoids downloading the same image separately for metadata and
pixels (64 MiB FIFO eviction is this demo host\'s policy, not an engine policy).
Images with imported dimensions do not trigger whole-article metadata fetches;
visible image pixels are requested as needed. Browser requests still require
CORS. No engine importer performs this routing
or networking, and other MediaWiki sites are not automatically redirected to an
API.

For any host, an HTML response with a Parsoid Content-Type profile, such as
`text/html; profile="https://www.mediawiki.org/wiki/Specs/HTML/2.8.0"`, selects
the independent MediaWiki importer. The profile's version must agree with any
document declaration. Wikipedia-routed responses without a profile must still
declare a supported DOM version. Unsupported/conflicting versions are errors.

Other responses dispatch by Content-Type, not URL suffix: `text/html` and
`application/xhtml+xml` use web article extraction; `text/markdown`,
`text/x-markdown`, and `text/plain` use Markdown; `application/epub+zip` uses EPUB.
Unsupported or missing response media types are errors. Declared HTTP charsets
are decoded by the host (default UTF-8).

Local `.mw.html`/`.mw.htm` files explicitly select MediaWiki document import and
must declare their DOM version. Ordinary `.html`/`.htm` files remain web
articles; `.xhtml` remains flow semantic XHTML. Saved files do not acquire an
original document URL from their filename: absolute/protocol-relative resources
and same-page URLs may need a host-supplied URL to resolve fully. This UI imports
complete documents, not body fragments. The database title is used only as a
window/document label; categories and other wiki metadata have no UI.

The independent `web-html` and `mediawiki-html` adapters both lower directly
through shared HTML infrastructure to flow IR. Only the web adapter runs
Readability extraction/normalization; expanded wiki content, Cite notes, Math,
and responsive alternatives use the MediaWiki adapter's policies. No XHTML
intermediate or engine networking is involved. Both reuse the shared budgeted
HTML5 parser (2 MiB input limit); these construction budgets are not a
process-wide allocation quota. Exact HTML byte offsets, browser CSS fidelity,
and responsive selection/rendering are not implemented.

## Startup tracing

Enable the reader's opt-in desktop subscriber and the engine's phase spans:

```sh
cargo run --manifest-path examples/reader/Cargo.toml --features telemetry -- \
  https://en.wikipedia.org/wiki/Apple 2> /tmp/flow-reader.trace
```

The default filter is `flow::phase=info`; `RUST_LOG` overrides it. Output includes
thread names, process-relative timestamps, per-job queue wait and generation,
HTTP headers/body, decoding/import stages, navigation/search indexing,
composition/placement, rendering, and presentation. The host alone installs the
subscriber; default builds have no subscriber and compile out phase fields.

The `first_content` milestone reports `open_ms` since open submission and
`window_ms` since window/surface setup. It is emitted once after the requested
document's first successful buffer presentation, not after all images finish.
Span `time.busy` is entered-span wall time (including synchronous I/O), not a CPU
sample count; parent timings include child spans and must not be added together.
Default phase output records sizes, formats, and HTTP hostnames, not source HTML
or full URLs/query strings.

Before eliminating per-chunk DOM budget rescans, a debug-build Apple baseline
on macOS measured about 3.00 s window-to-content:
HTTP ~350 ms; import ~496 ms (HTML5 parsing ~339 ms); composition ~583 ms;
navigation indexing ~1.36 s across three builds (two empty-layout builds and one
with 365 blocks / 4,171 anchors). These are observational timings, not test
thresholds. With one final DOM audit instead of per-chunk rescans, a subsequent
run measured parsing ~110 ms (final audit ~2.6 ms), total import ~270 ms, and
window-to-content ~2.77 s. Replacing repeated global navigation scans with
per-construction anchor/geometry lookups then reduced the three index builds to
~18 / 18 / 23 ms and window-to-content to ~1.47 s, without changing their lifecycle
or count. Construction uses a single anchor pass and interval sweeps; geometry
queries use binary search while preserving visual-order priority for overlapping
table ranges. Separating semantic construction from geometry binding subsequently
reduced this to one ~17 ms semantic build and one ~6 ms geometry bind; first content
measured ~1.41 s in that run, with composition still ~583 ms. Reflow/session tests
verify that semantic target storage is retained while coordinates and generated
note-return actions follow the current placement. Tracing itself does not change
scheduling or optimize those phases.

## Checks

```sh
cargo test --locked --manifest-path examples/reader/Cargo.toml
cargo clippy --locked --manifest-path examples/reader/Cargo.toml --all-targets
python3 examples/reader/tests/make_epub.py /tmp/flow-demo.epub
```

Wide-content fixtures live in
[`crates/flow_document/tests/fixtures/horizontal-overflow`](../../crates/flow_document/tests/fixtures/horizontal-overflow).
Open them at a narrow width to exercise tables, authored code lines, nested
content, shared page-fragment offsets, and keyboard/wheel/touch interactions.

The synthetic EPUB exercises links, images and scrolling/pagination. Native
host tests also exercise EPUB fonts, back/navigation, bidirectional chapter-boundary
page turns, wheel thresholds, end intent during repagination, owned XHTML,
pixel packing and local resource policy. CI builds native hosts and WASM.
Browser smoke testing should open the synthetic EPUB, follow its link, go back,
change text size,
scroll, toggle pagination, turn pages across both chapter boundaries, and resize
the canvas.
