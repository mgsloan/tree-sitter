# Hybrid point-seek results — 2026-09-11

Point seeking now keeps both ancestor searches. After the indexed start search, it uses the physical distance from the candidate to the requested subtree root: at most 512 groups uses the direct grouped end scan; a longer distance uses parent traversal. The comparison is one integer branch. It needs no additional index and does not change the packed encoding.

The cutoff sweep used the 220-file pilot plus the 63 unique large local regressions, with original and mutated inputs (566 cases per workload). A cutoff of zero is parent-only. Increasing the cutoff improved mixed point seeking through 512 groups; 4,096 and 8,192 groups admitted the long scans again and raised the selected-set ratios to 1.001× and 1.056×. Adding a lower cutoff did not consistently help. The retained power-of-two cutoff leaves a wide margin before that failure region.

The cloud run uses the same GCP Broadwell `e2-standard-2`, pinned CPU, compiler, frozen 10,000-file corpus, query generation, and baseline (`220ee121c`) as the [earlier strategy comparison](seek-cloud-results-2026-09-11.md). Each file is tested both original and deterministically mutated. Each reported case is the median of five alternating before/after pairs, with 32 rounds of 128 warmed queries. Parsing, packing, query preparation, and result checks are outside the timer.

| Point ancestor strategy | Empty after/before | Mixed after/before |
|---|---:|---:|
| Parent only | 0.756× | 0.873× |
| Unrestricted end scan | 0.679× | 0.758× |
| 512-group hybrid | 0.679× | 0.759× |

The heuristic keeps essentially all of the unrestricted scan’s aggregate cloud gain: 32.1% less CPU for empty ranges and 24.1% less for mixed ranges. Parent-only saved 24.4% and 12.7%. Byte-seek code is unchanged; its small difference between runs is measurement variation.

| Original point-mixed file | Parent only | Unrestricted scan | Hybrid |
|---|---:|---:|---:|
| `black/profiling/dict_huge.py` | 0.999× | 1.189× | 0.964× |
| `slate/bootstrap.rtl.min.css` | 0.977× | 1.449× | 0.905× |
| `senticon_eu.json` | 1.003× | 1.144× | 0.957× |

The hybrid removes the repeatable medians that motivated rejecting the unrestricted scan. Its second cloud boot was noisier—the seven-sweep hybrid ranges for these rows were 0.938–1.251×, 0.860–1.018×, and 0.833–1.106×—so the medians are evidence about the large regressions, not precise estimates. The full 20,000-case totals average that variation, and every grammar improved in aggregate.

Local full-corpus results were 0.773× for empty point ranges and 0.796× for mixed ranges, compared with parent-only 0.839× and 0.922×. Outlining the two ancestor methods was flat for empty ranges and 0.3% slower for mixed ranges, so they remain inline.

All cloud checks passed (10,383,360 comparisons). The independent full descent oracle passed 100,992,822 byte and 101,111,988 point comparisons. Pilot oracles and unit tests also passed with 32/64-node groups, scalar decoding, AddressSanitizer, UndefinedBehaviorSanitizer, and leak detection. The byte-only build excludes the hybrid point code.

Raw rows, all five timing samples, exact commands, CPU accounting, binary/source hashes, threshold sweeps, and validation logs are under `build/squat-seek-profile/`. The adjacent JSON preserves the durable aggregates and all seven large-case sweep samples.
