# Local seek profiling and optimization — 2026-09-11

The follow-up [cloud comparison](seek-cloud-results-2026-09-11.md) repeats the full corpus and separately tests large-file regression stability.

Both indexed seeks were profiled against commit `220ee121c`, which already binary-searches group starts. The retained changes compare selected-group start deltas with SSE2, use conditional binary-search bounds, and avoid whole-tree checks during equal-start walks. Byte seeking scans end deltas to find enclosing ancestors. Point seeking retains span-based parent traversal. Shared-boundary descent remains available, with inlining disabled under GCC/Clang to keep it out of the indexed search body. No encoding changes or index allocations were introduced.

The paired benchmark covers the frozen 10,000-file selection across 11 grammars, original and deterministically mutated: 20,000 file cases per workload. Every query result is checked against a separately compiled, symbol-renamed baseline on the same packed tree. Source and grammar hashes are verified. Parsing, packing, query generation, and comparisons are outside the timer.

Measurements use thread CPU time, five alternating before/after repeats, and the median time per file. Each repeat runs 32 rounds of 128 deterministic queries. “Empty” means unnamed zero-length root queries. “Mixed” alternates root/subtree queries, uses named lookups for half the queries, and adds 0–31 bytes to two thirds of the ranges, clamped at EOF; this includes ranges extending beyond a selected subtree. The same byte offsets supply point queries. Repetition measures warmed seeks, not cold-cache latency.

Hardware: local Intel Core Ultra 7 165U, pinned to performance CPU 0, GCC 15.3.0, `-O3 -g`, default 16-node groups. The laptop was on battery. These paired ratios are separate from the earlier Broadwell VM benchmark; they do not establish cross-machine speedups.

| Build / seek | Workload | After/before total CPU | CPU reduction | Median per-file ratio |
|---|---|---:|---:|---:|
| Byte-only / byte | empty | 0.667× | 33.3% | 0.663× |
| Byte-only / byte | mixed | 0.642× | 35.8% | 0.632× |
| Points enabled / byte | empty | 0.669× | 33.1% | 0.663× |
| Points enabled / byte | mixed | 0.652× | 34.8% | 0.643× |
| Points enabled / point | empty | 0.839× | 16.1% | 0.825× |
| Points enabled / point | mixed | 0.922× | 7.8% | 0.912× |

Each language improved in aggregate. Ratios below use the byte-only build for byte seeks and the points-enabled build for point seeks.

| Grammar | Byte empty | Byte mixed | Point empty | Point mixed |
|---|---:|---:|---:|---:|
| bash | 0.664× | 0.628× | 0.837× | 0.923× |
| c | 0.706× | 0.662× | 0.866× | 0.937× |
| cpp | 0.676× | 0.639× | 0.859× | 0.928× |
| css | 0.681× | 0.665× | 0.802× | 0.926× |
| go | 0.681× | 0.610× | 0.834× | 0.918× |
| html | 0.612× | 0.619× | 0.883× | 0.936× |
| json | 0.624× | 0.597× | 0.824× | 0.919× |
| python | 0.734× | 0.711× | 0.871× | 0.933× |
| tsx | 0.658× | 0.621× | 0.812× | 0.903× |
| typescript | 0.692× | 0.667× | 0.854× | 0.923× |
| yaml | 0.600× | 0.625× | 0.828× | 0.917× |

Hardware sampling initially put 20.8% of byte-seek cycles and 15.5% of point-seek cycles in parent lookup on the TypeScript pilot. The selected-group search and repeated public preorder bounds checks were also hot. The final hardware-counter probe uses the same 20 TypeScript files, both input modes and workloads, 3,000 rounds, and three runs. Unlike the timed table above, these counters include the process setup, parsing, and correctness checks.

| Seek | Instructions after/before | Branches after/before | Branch misses after/before | Cycles after/before |
|---|---:|---:|---:|---:|
| byte | 0.661× | 0.531× | 0.493× | 0.691× |
| point | 0.857× | 0.818× | 0.961× | 0.909× |

An unrestricted point-end scan improved the initial full-run totals more (0.736× empty, 0.774× mixed), but made some large mixed-range cases substantially slower: about 1.24× on `black/profiling/dict_huge.py` and 1.80× on a minified Bootstrap CSS file. Span filters, bounded scans, and separate ancestor helpers were also measured. Retaining point parent traversal removed those large regressions. The JSON includes the exploratory measurements; the retained point algorithm trades some aggregate gain for more consistent behavior on large inputs.

Individual timings still vary. The JSON records cases slower by more than 5% and their source sizes; aggregate improvement is not a promise that every file/query is faster. Parent traversal and byte end scans retain linear worst cases.

Correctness checks preserve the old sibling descent as an independent oracle, covering named/unnamed lookups, subtree roots, boundaries, malformed variants, invalid/reversed ranges, and exhaustive small-file ranges.

| Configuration | Byte comparisons | Point comparisons |
|---|---:|---:|
| points-full | 100,992,822 | 101,111,988 |
| bytes-full | 100,992,822 | 0 |
| group32 | 2,289,478 | 2,292,088 |
| group64 | 2,289,478 | 2,292,088 |
| sanitized | 2,289,478 | 2,292,088 |
| scalar | 2,289,478 | 2,292,088 |

All checks passed. Packed-column unit tests passed in both point modes, 32/64-node groups, and the sanitizer build. The scalar seek path was forced with `-U__SSE2__`; this is not a test on an ARM CPU. The sanitizer run enabled AddressSanitizer, UndefinedBehaviorSanitizer, and leak detection. Byte-only preprocessing contains no point seek/comparison helpers.

Reproduce the paired measurements from this checkout:

```sh
python3 tools/squatter/benchmark-seek.py --output build/seek-repro/points --points 1
python3 tools/squatter/benchmark-seek.py --output build/seek-repro/bytes --points 0
```

Use `--files-per-grammar 20` for a pilot. To sample a seek, add `--grammar typescript --files-per-grammar 20 --rounds 3000 --profile point --perf-event cpu_core/cycles/u`; use `before-point`, `byte`, or `before-byte` for the other paths, and select a supported event on other CPUs. The driver accepts a different `--baseline`, but baseline `node.c` must use the current slab layout.

Raw rows, compiler logs, exact manifests, source snapshots, profiles, rejected variants, and validation logs are in `build/squat-seek-profile/`. The adjacent JSON retains aggregate results, source/binary hashes, commands, profiling counters, language summaries, and validation counts.
