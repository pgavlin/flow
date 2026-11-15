# Local build/lint runner

Run from the repository root:

```sh
rustup component add clippy rustfmt # once, if needed
cargo lint
cargo lint --all
cargo lint --help
```

The Cargo aliases run a dependency-free Rust build/lint tool.

## Browser demo build

```sh
cargo build-web       # optimized WASM + JS bindings + HTML
cargo build-web --dev # debug build
cargo build-web --help
```

Requires the WASM target, WASM-capable clang/LLVM, and the pinned wasm-bindgen CLI;
see [reader setup](../../examples/reader/README.md#web-no-bundler-or-js-framework).
Output is always `examples/reader/target/web`, with compilation isolated under
`examples/reader/target` even if `CARGO_TARGET_DIR` is set. Version checks happen
before compilation.

## Checks

The default local loop runs:

- Formatting for the engine, standalone demo, and runner.
- Clippy for all engine targets/features and for the runner itself.

`--all` adds:

- A core document build with compiler warnings denied.
- Core document and publication Clippy configurations.
- Clippy and window-free tests for the isolated demo reader host.
- The runner's own unit tests.

CI uses `cargo lint --all` on Linux and macOS. Runtime engine tests remain a
separate CI job.

Checks inherit the terminal for ordinary file/line diagnostics, stop at the
first failure, and propagate the failed command's exit code. There are no shell
command strings or wildcard expansion: arguments are passed directly to each
process.

## Adding a linter

Register a new `Check` in `checks()` in `src/main.rs`, adding a tool executable
if needed. Put routine checks in the default plan and slower validation in the
full plan. Both local use and CI then get the same invocation and failure
handling.

Run runner tests directly with:

```sh
cargo test --locked --manifest-path tools/xtask/Cargo.toml
```
