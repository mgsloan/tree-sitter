# Highlighting and tags query timings on GCP — 2026-09-14

These benchmarks execute real highlighting and tags query files through the Rust bindings, including regex, equality, negated-equality, and set-membership text predicates. Highlighting consumes ordered captures; tags consumes complete matches and captured ranges. Query compilation, parsing, and packing are outside timing. Cursor creation, execution, predicate evaluation, result consumption, and cursor destruction are inside timing.

This measures query execution, not the complete highlighting or tagging application: rendering, language injections, and tag-documentation transformations are excluded. The `strip!`, `set-adjacent!`, and `select-adjacent!` directives are retained and reported but not applied to documentation strings.

## Findings

On these prepared trees, Squatter highlighting takes **0.584–7.187 ms per three-file small batch** and **37.439–241.830 ms per large file** with points enabled. Tags takes **0.199–0.579 ms per small batch** and **3.402–113.803 ms per large file**. Result counts matter: the large Python tags workload finds only two matches, while the large TSX workloads find tens of thousands.

The strongest measured highlighting transition is **10→16**, at **5.2–5.9% less time**, but this row covers only C++. Its tags improvement is about 1%. **9→16** gives smaller highlighting gains of **1.5–2.4%** and tags gains of **2.9–3.6%**. These transitions should be evaluated separately.

**5→8** improves highlighting by only about **1.1–1.3%** on the affected grammars; the available tags workloads cannot measure that transition. Its stronger rationale remains the earlier full-walk results. These query measurements do not establish an optimal 16-bit threshold because widths 11–15 are absent. Small differences are close to the variation in the unchanged-layout control and should not be treated as decisive.

## Absolute execution time

All numbers below use the compact-width baseline with points enabled. Cells show **Squatter / Tree-sitter**, in milliseconds of process CPU time. They are medians across accepted rounds, with five samples per round. The input tree and query are already prepared.

### Small-file batches

Each row processes three complete files. Times are for the entire batch, not for one file. Capture and match counts are identical in both backends.

| Grammar | Total KiB | Highlight ms (SQ / TS) | Tags ms (SQ / TS) | Highlight captures | Tag matches |
|---|---:|---:|---:|---:|---:|
| bash | 38.7 | 1.421 / 2.892 | — | 3,046 | — |
| c | 67.4 | 7.187 / 14.787 | 0.233 / 9.242 | 19,953 | 68 |
| cpp | 38.4 | 2.538 / 5.371 | 0.199 / 3.654 | 5,299 | 68 |
| css | 11.6 | 0.712 / 1.505 | — | 2,119 | — |
| go | 12.0 | 0.649 / 1.265 | 0.225 / 0.919 | 1,621 | 176 |
| html | 38.6 | 1.319 / 2.183 | — | 4,183 | — |
| json | 43.7 | 4.417 / 8.625 | — | 10,133 | — |
| python | 20.8 | 3.541 / 5.294 | 0.330 / 1.618 | 3,073 | 112 |
| tsx | 14.9 | 1.688 / 2.644 | 0.579 / 1.885 | 3,053 | 174 |
| typescript | 15.2 | 1.215 / 2.092 | 0.331 / 1.220 | 2,221 | 145 |
| yaml | 24.3 | 0.584 / 1.067 | — | 1,390 | — |

### Large files

Each row processes one complete file. An em dash means there is no applicable accepted query measurement.

| Case | MiB | Highlight ms (SQ / TS) | Tags ms (SQ / TS) | Highlight captures | Tag matches |
|---|---:|---:|---:|---:|---:|
| cpp-large-3 | 3.45 | 145.630 / 326.681 | 13.653 / 226.046 | 322,809 | 5,299 |
| json-large-0 | 1.06 | 97.466 / 174.923 | — | 234,807 | — |
| json-large-8 | 1.81 | 211.448 / 398.624 | — | 496,781 | — |
| python-large-4 | 1.07 | 135.830 / 294.050 | 3.402 / 172.933 | 165,759 | 2 |
| tsx-large-1 | 1.20 | 186.743 / 326.076 | 63.976 / 218.341 | 386,202 | 22,399 |
| tsx-large-7 | 3.34 | 241.830 / 745.936 | 113.803 / 642.131 | 728,981 | 16,144 |
| typescript-large-2 | 1.25 | 73.928 / 124.565 | 27.808 / 80.945 | 175,012 | 10,691 |
| typescript-large-6 | 2.24 | 85.091 / 147.649 | 33.885 / 95.662 | 201,063 | 12,343 |
| yaml-large-5 | 1.09 | 37.439 / 80.108 | — | 88,563 | — |

## Individual width transitions

Each row changes only the named storage width. Negative percentages mean less query execution time. The 8-bit and 16-bit thresholds are independent; a missing measurement is not treated as zero benefit.

Only affected grammars enter each row. Comparisons use `r2/r5` for 2→8, `r5/r6` for 5→8, `r6/r7` for 6→8, and `r5_9/r5` for 9→16 and 10→16, restricting the last two to their corresponding grammar widths. Other actual symbol/field widths remain fixed. These are marginal comparisons in those configurations, not all against the completely compact layout.

| Transition | Highlights, points | Highlights, byte-only | Tags, points | Tags, byte-only |
|---|---:|---:|---:|---:|
| 2→8 | -2.2% | -1.2% | — | — |
| 3→8 | — | — | — | — |
| 4→8 | — | — | — | — |
| 5→8 | -1.3% | -1.1% | — | — |
| 6→8 | -1.2% | -3.2% | -0.7% | -0.9% |
| 7→8 | — | — | — | — |
| 9→16 | -1.5% | -2.4% | -2.9% | -3.6% |
| 10→16 | -5.9% | -5.2% | -1.0% | -1.2% |
| 11→16 | — | — | — | — |
| 12→16 | — | — | — | — |
| 13→16 | — | — | — | — |
| 14→16 | — | — | — | — |
| 15→16 | — | — | — | — |

There are no real grammar measurements for 3→8, 4→8, 7→8, or 11–15→16. The available tags queries also do not exercise a changing 2-bit or 5-bit column. The 10→16 row covers only C++; the other rows average their affected grammars. Per-case ratios and grammar identities are retained in the raw artifact.

## Workloads and verification

- GCP `squatter-benchmark`, `us-central1-a`, `e2-standard-2`, pinned to CPU 0. This boot reports Intel Xeon 2.20 GHz, family 6/model 79 (Broadwell), with two SMT vCPUs. The earlier synthetic-query cloud run used an EPYC host; use comparisons within each run.
- Frozen runtime and Rust binding revision `ab9143b2fa3aff3fbde2062eb85988be7c6c7042`. The six layout policies use the same prebuilt C archives as the earlier byte-rounding experiment, with GCC 15.3.0 and `-O3 -g -fno-omit-frame-pointer`. The Rust harness uses rustc 1.95.0, release optimization, no LTO. All builds were uploaded and executed with the guest ELF loader.
- Highlighting uses Zed query files for ten grammars. C++ combines the upstream C and C++ highlighting queries to avoid silently ignoring Zed's host-specific ancestor predicate. TSX and TypeScript use their full Zed highlighting queries.
- Tags uses upstream grammar queries for C, C++, Go, and Python. TSX and TypeScript combine their upstream TypeScript additions with JavaScript tags from commit `44c892e0be055ac465d5eeddae6d3e194424e7de`. The original supplement-only TypeScript tags measurements remain in the raw data, but are excluded from these tables and policy aggregates.
- Every source, grammar, and query is hash-checked. For every binary/input/configuration, the first round compares all ordered highlight events or tag matches with Tree-sitter, including patterns, capture IDs, and byte ranges. Timing checks ordered checksums and event counts on every repetition; later rounds skip repeated vector validation. Four-million-capture and 30-second-per-query guards reject incomplete executions rather than count them as fast results.
- Three paired rounds with randomized policy order (exact first in round one for validation), five CPU-time samples, and calibration to at least 30 ms per sample when a single query execution is shorter. The exact policy also times Tree-sitter. Parsing and regex compilation are amortized outside the timed region.
- A paired block contains all six policies for one job and points setting. Reject and rerun the entire block when the identical-layout `r7/exact` timing ratio is outside 0.85–1.15. Unfiltered results from the original runs are preserved for sensitivity analysis. The query-specific control threshold was selected before inspecting the cloud timings.
- Transition ratios use the median of within-block ratios across accepted rounds, then geometric averaging within a grammar and across affected grammars. Absolute times and result counts remain separate from the grammar-balanced policy summaries.

## Control audit and sensitivity

All 42 source files passed ordered-result validation for the applicable highlighting and tags queries. The final suite has 32 query jobs (20 highlighting, 12 tags), points enabled and disabled, and three accepted paired rounds: **192 accepted / 200 total paired blocks**. Eight original blocks failed the control threshold and all eight replacements passed. There were no query failures or excluded final workloads.

The raw data contains **1,416 binary/job measurements**: 1,152 original final-suite measurements, 216 supplemental-only TypeScript tags measurements retained for audit, and 48 repair measurements. The latter TypeScript-only queries are not counted in the reported tags results.

Including every original block changes combined-policy averages by at most 0.81 percentage points. Individual byte-only highlighting transitions are more sensitive: the unfiltered estimates are −5.1% for 6→8, −4.6% for 9→16, and −6.8% for 10→16, versus −3.2%, −2.4%, and −5.2% after the control-based reruns. Points-enabled highlighting estimates change by at most 0.1 percentage points. Thus the roughly 1% effects are not strong evidence for a cutoff; the C++ highlighting gain is visible in both summaries.

## Reproduction and artifacts

The [raw artifact](highlight-tags-cloud-results-2026-09-14.json) includes all timings, failures/exclusions, query provenance, input/build hashes, absolute per-job results, control rejections, and the unfiltered summary. The committed harness is `crates/squatter-bench/src/bin/query-workload.rs`.

The build manifest records the commands and the experimental `crates/squatter/build.rs` override that links each already-built C layout archive through `SQ_PREBUILT_LIB`. Freeze the recorded revision, copy in the harness, and build `squatter-bench --bin query-workload --release --locked` once per archive and points feature. `--no-default-features` selects byte-only mode. Upload the hashed job inputs and binaries using the bundle paths in the artifact.

```sh
squatter-idle run -- python3 benchmark-query-workloads.py . --loader /lib64/ld-linux-x86-64.so.2 --cpu 0 --rounds 3 --repeat 5
squatter-idle run -- python3 benchmark-query-workloads.py full-tags --loader /lib64/ld-linux-x86-64.so.2 --cpu 0 --rounds 3 --repeat 5 --tag cloud-full-tags
python3 summarize-query-workloads.py cloud-queries.json full-tags/cloud-full-tags.json --widths widths.json --output summary.json
```

Include repair JSON files when summarizing accepted measurements. Add `--keep-all` and use only the original two runs to reproduce the unfiltered comparison. Production code and the rounding policy are unchanged.
