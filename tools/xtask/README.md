# Local development tasks

Run from the repository root:

```sh
rustup component add clippy rustfmt # once, if needed
cargo lint
cargo lint --all
cargo lint --help
```

The Cargo aliases run a Rust host tool, separate from the engine workspace.
Local builds, CI, and releases use the compiler pinned in `rust-toolchain.toml`.

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

## Performance capture and comparisons

```sh
cargo perf                 # Full timing suite, then three serial allocation runs.
cargo perf --accept        # Accept the latest complete capture; do not rerun.
cargo perf --compare before.jsonl after.jsonl
```

Capture requires cargo-criterion (`cargo install --locked cargo-criterion
--version 1.1.0`). Results and comparisons go under `target/performance/latest/`;
failed runs leave the previous capture untouched. Acceptance validates the
capture and replaces `docs/performance-baseline/`, removing obsolete files.
Neither command commits changes. Always rerun after changing the measured code
or configuration, and run these commands serially. Partial/filtered captures
are not supported by this workflow.

Timing reports compare typical point estimates; allocation reports show changed
counters using medians across three runs, retaining all raw data and variation
ranges. Review the report and environment metadata before accepting.

`perf --compare` also accepts two full capture directories to compare timing and
memory together (for example, `docs/performance-baseline` and
`target/performance/latest`). For timing-only comparisons, it accepts
cargo-criterion JSON message streams and the repository's saved
`timing.json` format, including comparisons between the two formats. Prints
before/after times and percentage changes; missing cases are marked as added
or removed. It uses cargo-criterion's `typical` estimate (slope when available,
otherwise mean) and ignores change classifications from unrelated runs.
Geomean summary rows appear below the results, with equal weight for every
matched positive pair, including hidden unchanged cases. Timing and memory
metrics have independent summaries; pair counts are shown, zeros/missing cases
are excluded, and an empty summary is `n/a`. No statistical significance is
inferred. Timing-file comparisons do not include allocation reports.

See [performance measurement](../../docs/performance.md) for capture commands.

## Checks

The default local loop runs:

- Formatting for the engine, standalone demo, and runner.
- Clippy for all engine targets/features and for the runner itself.

`--all` adds:

- A core document build with compiler warnings denied.
- Core document and publication Clippy configurations.
- Shared-cache dependency isolation and Linux font-feature checks. The Linux
  dependency graph is checked on every host without cross-compiling.
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
