# Performance benchmarks

The publication benchmarks measure generated, in-memory engine workloads—not
filesystem access, publication artifact decoding, image decoding, or painting.
Workload definitions and settings live in the benchmark code:

- [`opening.rs`](../crates/flow_publication/benches/opening.rs): Criterion timing cases and measurement boundaries.
- [`memory.rs`](../crates/flow_publication/benches/memory.rs): DHAT allocation measurements and JSON report fields.
- [`support/mod.rs`](../crates/flow_publication/benches/support/mod.rs): shared fixtures, scenario matrix, settings, host responses, and preflight assertions.

## Capture, compare, and accept

Install cargo-criterion once, then run from the repository root:

```sh
cargo install --locked cargo-criterion --version 1.1.0
cargo perf
# Review target/performance/latest/comparison.txt and the raw captures.
cargo perf --accept
```

`cargo perf` runs the full timing suite followed by three serial allocation
runs, prints a comparison against `docs/performance-baseline/`, and saves a
completed capture under `target/performance/latest/`. It does not change the
checked-in baseline. Timing uses Criterion's typical estimate; memory compares
medians across the three runs and prints only changed counters. Raw memory
reports and variation ranges are retained. Runner, environment, and cache-policy
differences must be considered before attributing changes to code.

`cargo perf --accept` validates and copies the latest completed capture into
`docs/performance-baseline/`, without rerunning benchmarks or committing files.
It removes obsolete baseline files, including the older `timing.json` when a
`timing.jsonl` capture replaces it. Review and commit the baseline alongside the
code it measured; rerun if that code or the measurement configuration changes
before acceptance.

Failed or incomplete captures cannot replace the previous successful capture or
baseline. Run capture and acceptance commands serially. This workflow always
runs the full suite; use the individual commands below for filtered experiments.

## Running and comparing timings

Run from the repository root:

```sh
cargo bench --locked -p flow_publication --features render --bench opening
```

For one execution of each workload and its validation assertions, without
statistical measurements:

```sh
cargo bench --locked -p flow_publication --features render --bench opening -- --test
```

Save a baseline before changing the code, then compare against it:

```sh
cargo bench --locked -p flow_publication --features render --bench opening -- --save-baseline before
# Make the change, keeping workloads, settings, and features unchanged.
cargo bench --locked -p flow_publication --features render --bench opening -- --baseline before
```

Add a filter after `--` to select cases, such as `publication_77_chapters` or
`compose_cold_fonts`. Criterion stores measurements under `target/criterion/`
(ignored by Git). `--quick` is useful for exploration, not reliable regression
claims.

## Formatting comparisons

`cargo perf --compare` prints a compact before/after table with percentage changes.
It accepts both the older saved `timing.json` format and cargo-criterion JSON
streams (`timing.jsonl`). Passing two capture directories compares both timing
and memory, using medians across their three allocation runs:

```sh
cargo perf --compare docs/performance-baseline target/performance/latest
```

To compare timing files only:

```sh
cargo perf --compare target/perf-before.jsonl target/perf-after.jsonl
# Older timing.json snapshots can be supplied in either position too.
```

To capture JSON streams instead (requires cargo-criterion; version 1.1.0 was
verified with this suite's Criterion 0.8.2):

```sh
cargo install --locked cargo-criterion --version 1.1.0
cargo criterion --locked -p flow_publication --features render --bench opening --message-format=json > target/perf-before.jsonl
# Make the change, keeping workloads, settings, and features unchanged.
cargo criterion --locked -p flow_publication --features render --bench opening --message-format=json > target/perf-after.jsonl
cargo perf --compare target/perf-before.jsonl target/perf-after.jsonl
```

The table compares `typical` point estimates (slope when available, otherwise
mean), with negative changes meaning faster. Geomean rows below the results
summarize equal-weight after/before ratios across matched positive measurements,
including unchanged cases. Timing and each memory metric are summarized
separately; `n` shows the contributing pair count. Missing or zero-valued pairs
are excluded, and no eligible pairs produces `n/a`. It marks added/removed benchmarks
and does not infer statistical significance. In particular, it ignores each
stream's emitted change classification, which may refer to a different prior
run. Retain the raw reports for confidence intervals and statistical analysis.

## Measuring allocations

Run the separate DHAT executable and capture its JSON report:

```sh
cargo bench --locked -p flow_publication --features render --bench memory > target/memory-before.json
# Make the change, keeping workloads, settings, and features unchanged.
cargo bench --locked -p flow_publication --features render --bench memory > target/memory-after.json
```

The full matrix can take several minutes. Select cases with a substring, or
prefix the filter with `=` for an exact name:

```sh
cargo bench --locked -p flow_publication --features render --bench memory -- scoped_aliases
cargo bench --locked -p flow_publication --features render --bench memory -- '=font_environments/shared/77'
```

Progress goes to stderr; stdout contains the JSON report. No DHAT profile file
is written. The allocator is confined to this executable; it does not replace
the library's or Criterion executable's allocator. Do not use DHAT runtime to
compare speed: allocation tracking adds significant overhead.

## Measurement boundaries

- “Cold” means fresh engine caches, not cold OS caches. Generated sources and
  font bytes are already in memory.
- Import and renderer/font setup are excluded from composition-only cases.
  Warm composition cases still perform composition; they do not measure a
  viewer's retained-layout lookup. Pagination-only cases use existing geometry.
- Publication cases include session creation, font registration, requested
  imports, host replies, and completion of the page index. Cached-index opens
  install an in-memory page-count vector, not a serialized artifact.
- Publication timings include session destruction. Memory reports capture live
  bytes both before and after output destruction. Composition cases can leave
  allocations in an externally held renderer after the composed output drops.

## Interpreting results

Compare elapsed time, cumulative allocated bytes, allocation counts, peak live
bytes, and retained bytes separately. Fewer cumulative allocations can coexist
with higher peak or retained memory. Compare repeated versus distinct text,
alternating versus grouped environments, and increasing chapter counts to
separate reuse benefits, switching costs, and retention growth.

DHAT tracks only allocations made within each profiling scope. Pre-existing
input storage is excluded; a pre-existing block reallocated within the scope is
treated as a new tracked allocation. Its counters describe incremental tracked
heap use, not the complete size of objects reachable from the output. Requested
bytes exclude allocator metadata, fragmentation, stack usage, mappings, and the
profiler's own storage; they are **not process RSS**. Bytes remaining after
session destruction check release behavior but do not prove bounded retention
within a session.

Compare on the same machine and Rust toolchain, with the same features, workload
revision, cache-policy overrides, and power settings, and with other
CPU-intensive work stopped. In particular, `test-fonts` preloads fixture families
into each backend, so construction costs differ from builds without that
feature. The publication cases accept `FLOW_BENCH_FONT_CACHE_BYTES`,
`FLOW_BENCH_FONT_ENVIRONMENTS`, and `FLOW_BENCH_DISABLE_PLAN_CACHE` overrides;
keep them consistent unless deliberately comparing policies.

Record the machine, toolchain, benchmark name, and confidence interval
when reporting timing results. Do not use fixed wall-time thresholds as unit-test
assertions or generalize these synthetic workloads to unmeasured operations.

## Recorded baseline

The [current baseline](performance-baseline/README.md) contains all 22 workloads,
full Criterion samples and confidence intervals, three DHAT runs, cache policy,
environment metadata, and baseline restoration instructions.

Update `docs/performance-baseline/` in the same commit as performance-affecting
changes. Overwrite the existing files rather than adding revision-specific
snapshots or commit hashes; use Git history for comparisons. Compare only
equivalent workload, toolchain, feature, machine, and policy configurations.
