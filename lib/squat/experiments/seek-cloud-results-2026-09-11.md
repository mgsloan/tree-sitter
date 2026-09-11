# Cloud seek comparison — 2026-09-11

The subsequent [hybrid experiment](seek-hybrid-results-2026-09-11.md) keeps both
point ancestor methods and selects between them using candidate distance. It
retains the direct scan's aggregate gain without the regressions measured here.

The large-file point-end-scan regressions reproduce on the cloud benchmarking instance. The retained implementation uses the faster selected-group search for both seeks, end-delta scanning for byte seeks, and parent traversal for point seeks. No packed encoding changes were made.

Run on `squatter-benchmark`, GCP `e2-standard-2`, `us-central1-a`: Intel Xeon 2.20 GHz, family 6/model 79 (Broadwell), pinned to CPU 0. Binaries were built with GCC 15.3.0, `-O3 -g`, 16-node groups, and run with the guest dynamic loader. The comparison baseline is `220ee121c`, which already binary-searches group starts. The JSON records source/binary hashes and the guest platform.

Full corpus: the same frozen 10,000 files across 11 grammars, each original and deterministically mutated. Each workload therefore has 20,000 cases. Each case checks 128 deterministic queries against the baseline on the same packed tree before timing. Five alternating before/after timing pairs each run 32 rounds (4,096 seeks); the table sums each case's median thread CPU time. Parsing, packing, query preparation, checks, and warmup are outside timing. These are warmed-query measurements.

Empty queries are unnamed zero-length ranges from the root. Mixed queries alternate root/subtree roots, use named lookup for half the queries, and extend two thirds of the ranges by 0–31 bytes, clamped at EOF. Point coordinates come from the same byte offsets. Mutation inserts `}` and an invalid UTF-8 byte and truncates one byte.

| Retained build / seek | Workload | After/before CPU | CPU reduction |
|---|---|---:|---:|
| Byte-only / byte | empty | 0.734× | 26.6% |
| Byte-only / byte | mixed | 0.714× | 28.6% |
| Points enabled / byte | empty | 0.629× | 37.1% |
| Points enabled / byte | mixed | 0.652× | 34.8% |
| Points enabled / point | empty | 0.756× | 24.4% |
| Points enabled / point | mixed | 0.873× | 12.7% |

Point ancestor strategies, full-corpus totals. All include the start-search optimizations. `end_scan` and `span_scan` are exploratory source snapshots; unlike the retained variant, their boundary fallback was not explicitly marked `noinline`, so this is a comparison of complete candidates, not an isolated single-change measurement.

| Point strategy | Empty after/before | Mixed after/before |
|---|---:|---:|
| Retained parent traversal | 0.756× | 0.873× |
| Direct end scan | 0.679× | 0.758× |
| Span-filtered end scan | 0.710× | 0.759× |

To assess noise, the 20 largest files among the local point-mixed regressions were rerun in seven cloud sweeps. This is a deliberately selected regression set, not a random corpus sample. Each sweep uses five alternating pairs with 64 rounds (8,192 seeks per timing); grammar and variant order also alternate. Original and mutated inputs are analyzed separately. Values below are the median and observed min–max across the seven sweep ratios, not confidence intervals.

| Original file, point mixed | Retained parent | Direct end scan | Span-filtered end scan |
|---|---:|---:|---:|
| `black/profiling/dict_huge.py` | 0.999× (0.997–1.006) | 1.189× (1.187–1.194) | 1.304× (1.298–1.312) |
| `slate/bootstrap.rtl.min.css` | 0.977× (0.948–1.003) | 1.449× (1.430–1.463) | 1.212× (1.203–1.228) |
| `senticon_eu.json` | 1.003× (0.988–1.008) | 1.144× (1.141–1.151) | 1.272× (1.267–1.278) |

The dictionary and original Bootstrap regressions occur in every sweep and are much larger than their sweep-to-sweep spread. They are not explained by the local laptop's power state or by combining original and mutated cases. Mutated Bootstrap behaves differently: the direct scan improves it; those timings must not be pooled to estimate repeat noise. The JSON preserves all 35 before/after timing samples for every selected point-mixed case.

Each language's retained ratios follow. Byte values use the byte-only build.

| Grammar | Byte empty | Byte mixed | Point empty | Point mixed |
|---|---:|---:|---:|---:|
| bash | 0.787× | 0.733× | 0.758× | 0.876× |
| c | 0.725× | 0.718× | 0.775× | 0.877× |
| cpp | 0.748× | 0.719× | 0.772× | 0.876× |
| css | 0.738× | 0.718× | 0.740× | 0.881× |
| go | 0.739× | 0.699× | 0.743× | 0.859× |
| html | 0.649× | 0.684× | 0.799× | 0.878× |
| json | 0.711× | 0.666× | 0.741× | 0.871× |
| python | 0.761× | 0.757× | 0.787× | 0.887× |
| tsx | 0.736× | 0.700× | 0.734× | 0.859× |
| typescript | 0.807× | 0.759× | 0.764× | 0.873× |
| yaml | 0.684× | 0.711× | 0.748× | 0.873× |

All cloud query comparisons passed: 430,080 in the large phase, 35,840,000 in the full phase. The independent descent-oracle validation and local hardware profiles are in [the local report](seek-profile-results-2026-09-11.md).

CPU 0 steal time during the large phase: median operation 0.000%, maximum 0.725% of elapsed CPU-accounting ticks. Thread CPU timers exclude time when the process is descheduled, but do not eliminate every possible shared-host effect.

CPU 0 steal time during the full phase: median operation 0.033%, maximum 0.151% of elapsed CPU-accounting ticks. Thread CPU timers exclude time when the process is descheduled, but do not eliminate every possible shared-host effect.

The VM exposes no hardware PMU (`cycles:u` is unsupported even with sudo). Separate `cpu-clock:u` sampling profiles use the first 20 TypeScript files, 3,000 rounds, for byte/point baseline/current; their commands and exit statuses are in the JSON. Profiling runs follow all timed comparisons, so sampling overhead does not enter the tables.

Cloud software sampling places 18.2% of baseline byte samples in parent lookup and 4.4% in the public previous-preorder helper; neither remains a significant byte hotspot after the change. The retained point seek still spends about 15.6% of samples in parent lookup. These short profiles contain roughly 900–1,800 samples each; use the paired timing tables for performance estimates. Symbol percentages are saved in the JSON.

The rejected candidate sources are preserved as [direct-end-scan](seek-point-end-scan.diff) and [span-filtered-scan](seek-point-span-scan.diff) patches against retained commit `992449727`. They change only seeking, including the fallback annotation difference described above. Apply one in an isolated checkout to reproduce that candidate with the paired driver.

Reproduce the paired binaries with `tools/squatter/benchmark-seek.py` as described in the local report. The cloud bundle is `build/squat-seek-profile/cloud-bundle/`; its `run.py large` and `run.py full` commands verify frozen source/grammar/binary hashes and run all candidates serially. On this VM invoke it through `squatter-idle run -- python3 -u run.py PHASE`. The bundle contains the exact baseline and candidate sources. Raw downloaded rows, progress logs, per-repeat samples, and profiles are under `build/squat-seek-profile/cloud-results/`. Aggregate and selected-case measurements are preserved in the adjacent JSON.
