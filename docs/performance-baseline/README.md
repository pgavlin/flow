# Performance baseline

This directory holds the current baseline; Git history holds previous measurements.
Machine: Apple M4 Max, 64 GiB RAM, macOS 26.7.1, arm64.
Toolchain: Rust/Cargo 1.94.0. Features: `render`; optimized bench profile.

All **22 workloads** have timing and memory baselines. See
[measurement boundaries](../performance.md) before interpreting the results.

## Collection

- Full Criterion sampling: 3-second warmup, 5-second requested measurement
  window, 95% confidence, 100,000 bootstrap resamples. Chapter/scratch cases
  have 100 samples; publication/font-environment cases have 10.
  Criterion extended collection for cold long-paragraph composition and
  `font_environments/shared/154`; all requested samples were collected.
- Timing ran first, followed by three complete serial DHAT runs.
  Allocation counts, cumulative bytes, completion live bytes, and post-drop
  live bytes matched exactly across all three runs.
- Peak live bytes varied in three cache-pressure cases:

| Workload | Peak live byte range across three runs |
| --- | ---: |
| `font_environments/shared/unique_tokens` | 5311087–5374157 |
| `font_environments/shared/unique_tokens_154` | 5363866–5464042 |
| `font_environments/alternating/unique_tokens` | 5143696–5159070 |

- Default publication policy: 4 MiB aggregate accounted cache admission,
  four retained environments, sixteen cached shaper faces per environment,
  eight plans per face. No benchmark policy overrides were set.
- Background load and thermal/power state were not controlled. Dev dependencies
  enable `test-fonts`, including additional preloaded fixture families.

## Results

Timing uses Criterion's slope estimate when present, otherwise its mean.
Brackets are 95% confidence intervals in the same units as the estimate.
Memory columns use MiB (1,048,576 bytes) and the first DHAT run; raw reports
preserve exact counters from all three runs. Completion live bytes include the
output and newly allocated caches held by external renderers, not just the
returned geometry. These measurements are not RSS or portable thresholds.

| Workload | Time [95% CI] | Allocated MiB | Allocation count | Peak live MiB | Live at completion MiB |
| --- | --- | ---: | ---: | ---: | ---: |
| `chapter/compose_cold_fonts` | 590.737 µs [583.338, 597.381] | 1.052 | 12162 | 0.505 | 0.482 |
| `chapter/compose_warm_fonts` | 453.329 µs [451.838, 454.834] | 0.715 | 7301 | 0.276 | 0.253 |
| `chapter/import_markdown` | 158.389 µs [158.024, 158.762] | 0.364 | 723 | 0.068 | 0.014 |
| `chapter/long_paragraph/compose_cold_fonts` | 1.856 ms [1.852, 1.862] | 4.343 | 29695 | 1.873 | 0.992 |
| `chapter/long_paragraph/paginate_composed` | 10.624 µs [10.563, 10.718] | 0.051 | 12 | 0.043 | 0.001 |
| `chapter/paginate_composed` | 1.282 µs [1.276, 1.287] | 0.014 | 9 | 0.014 | 0.001 |
| `font_environments/alternating/77` | 50.119 ms [50.004, 50.195] | 87.915 | 643464 | 0.850 | 0.553 |
| `font_environments/alternating/unique_tokens` | 210.259 ms [209.760, 210.880] | 250.582 | 2387305 | 4.920 | 4.586 |
| `font_environments/grouped/77` | 50.370 ms [50.234, 50.484] | 87.915 | 643464 | 0.850 | 0.553 |
| `font_environments/rotating_four/77` | 50.189 ms [50.121, 50.313] | 88.275 | 646909 | 0.984 | 0.687 |
| `font_environments/scoped_aliases/77` | 50.267 ms [50.069, 50.411] | 88.362 | 648678 | 0.832 | 0.534 |
| `font_environments/scoped_shared/77` | 49.765 ms [49.662, 49.895] | 87.764 | 642053 | 0.793 | 0.496 |
| `font_environments/shared/154` | 98.403 ms [98.296, 98.511] | 174.450 | 1272421 | 0.853 | 0.556 |
| `font_environments/shared/77` | 49.763 ms [49.697, 49.862] | 87.711 | 640834 | 0.779 | 0.482 |
| `font_environments/shared/unique_tokens_154` | 432.548 ms [431.033, 434.310] | 508.268 | 4830064 | 5.191 | 4.609 |
| `font_environments/shared/unique_tokens` | 211.277 ms [210.740, 211.791] | 250.507 | 2386184 | 5.125 | 4.709 |
| `paragraph_scratch/large_then_small` | 4.454 ms [4.445, 4.463] | 8.854 | 72702 | 2.592 | 2.358 |
| `paragraph_scratch/many_small` | 2.641 ms [2.634, 2.650] | 4.112 | 44598 | 1.380 | 1.258 |
| `publication_77_chapters/images/cached_index_open` | 1.455 ms [1.451, 1.459] | 2.699 | 19466 | 0.692 | 0.672 |
| `publication_77_chapters/images/cold_open` | 51.856 ms [51.767, 51.948] | 98.823 | 690131 | 1.241 | 0.934 |
| `publication_77_chapters/text/cached_index_open` | 946.521 µs [944.230, 949.071] | 1.717 | 11255 | 0.452 | 0.399 |
| `publication_77_chapters/text/cold_open` | 49.290 ms [49.169, 49.459] | 87.711 | 640834 | 0.779 | 0.482 |

## Saved data

- `metadata.json`: machine, toolchain, environment, policy, commands,
  timestamps, repetition checks, and memory-report SHA-256 hashes.
- `timing.json`: Criterion estimates, individual samples, workload metadata,
  and Tukey outlier fences.
- `memory.json`, `memory-2.json`, `memory-3.json`: unmodified DHAT reports.

Update these files in the same commit as performance-affecting changes, keeping
workloads and measurement configurations equivalent. Use Git history to compare
with previous baselines; do not add revision-specific directories or identifiers.

## Compare locally

The named Criterion baseline is installed under
`target/criterion/**/baseline/` on the measurement machine:

```sh
cargo bench --locked -p flow_publication --features render --bench opening -- --baseline baseline
cargo bench --locked -p flow_publication --features render --bench memory > target/memory-current.json
```

Compare memory counters by workload name against the saved reports, using
equivalent machine, toolchain, feature, workload, and policy configurations.

If `target/` has been cleaned, restore the named baseline from the repository
root before running the timing comparison:

```sh
python3 - <<'PY'
import json
from pathlib import Path
source = Path("docs/performance-baseline/timing.json")
for case in json.loads(source.read_text())["benchmarks"]:
    directory = Path("target/criterion") / case["benchmark"]["directory_name"] / "baseline"
    directory.mkdir(parents=True, exist_ok=True)
    for name in ("benchmark", "estimates", "sample", "tukey"):
        (directory / f"{name}.json").write_text(json.dumps(case[name]))
PY
```
