# Tiny flow reader host

One window (or browser canvas), one open publication, continuous or paginated reading.
Opens UTF-8 Markdown, **flow semantic XHTML** (not arbitrary web pages), and EPUB.
No library, persistence, TOC/search/settings panels, remote fetching,
or application framework.

## Desktop

```sh
cargo run --locked --manifest-path examples/reader/Cargo.toml
cargo run --locked --manifest-path examples/reader/Cargo.toml -- /path/to/book.epub
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

Opening another file replaces the current publication and its history. Document
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
links. Only selected file bytes enter WASM—documents are never uploaded to a
server. Serve over HTTP(S), not `file:`. No cross-origin-isolation headers needed.

An uploaded EPUB can supply its own images/fonts. Standalone browser files have
no access to neighboring files; embedded base64 data images work. Directory
picking, remote resources and cross-file standalone links are deliberately out
of scope. Browser code blocks are plain text unless a host registers WASM grammar
modules; this demo does not load them. The canvas is not a screen-reader text view.

## Host boundary and limits

`winit` owns events/windows; `softbuffer` presents CPU pixels. `book.rs` owns
adapter selection, acquisition, reference policy and image decoding. The desktop
uses one bounded input worker; browser jobs run on the event loop (large imports
can briefly block it). Generations and engine request IDs reject stale results.
There is no background prefetch or whole-book pagination, and no async runtime.

Fonts are explicitly supplied from the repository's licensed Charter/Fira Code
fixtures, not discovered from the system. See
[font notices](../../crates/flow_document/tests/fixtures/fonts/README.md). The
small bundled set is not comprehensive multilingual coverage.

Demo limits: 64 MiB input, 16 MiB standalone source/resource, 4096×4096 source
images with a 64 MiB decoder budget, 4 megapixel decoded variants, 16 megapixel
window. Desktop standalone assets must remain inside the selected document's
canonical directory; assume those files remain unchanged while open. External
activation allows HTTP(S) only.

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
