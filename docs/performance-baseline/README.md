# Performance baseline

Captured by `cargo perf`: full Criterion timing followed by three serial DHAT
runs. See [measurement guidance](../performance.md) and `metadata.json` for
the environment, cache policy, commands, timestamps, and variation ranges.

- `timing.jsonl`: raw cargo-criterion messages, samples and confidence intervals.
- `memory.json`, `memory-2.json`, `memory-3.json`: raw allocation reports.
- `comparison.txt`: comparison against the baseline present at capture time,
  using typical timings and median memory counters across three runs.

Update this directory with `cargo perf --accept` in performance-affecting commits.
Git history holds previous baselines; filenames do not contain revision IDs.
