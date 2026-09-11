# Indexed byte seeking — 2026-09-11

Byte-range descendant lookup now binary-searches existing group start-byte minima, scans the selected group, and ascends to the deepest qualifying ancestor. This removes the previous repeated linear sibling searches without changing the slab layout or adding an index allocation. Equal-start empty nodes retain the old descent to preserve boundary behavior. Parent ascent can still scan groups, so the complete operation is not guaranteed logarithmic.

## Before and after

Exactly 10,000 selected corpus files, original and deterministically mutated, three repeats: 20,000 paired file cases. Same Intel Broadwell GCP VM, CPU 0, byte-only release builds, other compile-time defaults unchanged. Before/after order alternates by 500-file batch and input mode. Only `seek-byte` was selected; automatic cold-parse rows are excluded here. The earlier query-heavy run remains stopped.

The timed benchmark performs 100 deterministic unnamed, zero-length byte-range lookups per file and records returned identities. Parsing, packing, and correctness comparisons are outside the seek measurement. Ratios against mainline are Squatter/mainline; lower is better. Median uses per-file paired-repeat ratios; total divides summed per-file median CPU times.

| Inputs | Cases | Before median | After median | Before total | After total | After/before total |
|---|---:|---:|---:|---:|---:|---:|
| combined | 20,000 | 1.118× | 0.348× | 1.760× | 0.288× | 0.162× |
| original | 10,000 | 1.109× | 0.340× | 1.733× | 0.285× | 0.163× |
| mutated | 10,000 | 1.126× | 0.355× | 1.788× | 0.292× | 0.162× |

Across paired file cases, the median after/before CPU ratio is 0.306× (69.4% less CPU); summed CPU fell 83.8%. 299 of 20,000 individual timings were slower. Those cases have a median source size of 0 bytes; the largest observed absolute increase was 0.025 ms per 100 seeks. Every language improved in aggregate.

## By language

| Grammar | Cases | Before median vs mainline | After median vs mainline | After/before total CPU |
|---|---:|---:|---:|---:|
| bash | 384 | 1.096× | 0.406× | 0.273× |
| c | 504 | 1.071× | 0.263× | 0.131× |
| cpp | 1,916 | 1.221× | 0.320× | 0.218× |
| css | 2,330 | 1.217× | 0.402× | 0.104× |
| go | 2,294 | 1.017× | 0.241× | 0.211× |
| html | 932 | 1.131× | 0.392× | 0.313× |
| json | 2,328 | 1.408× | 0.371× | 0.141× |
| python | 2,328 | 1.031× | 0.322× | 0.098× |
| tsx | 2,328 | 0.979× | 0.282× | 0.187× |
| typescript | 2,328 | 1.089× | 0.415× | 0.286× |
| yaml | 2,328 | 1.072× | 0.433× | 0.286× |

## Correctness

An independent test retains the previous sibling-descent implementation as an exact oracle, because the existing benchmark intentionally counts known mainline seek differences separately. It checks named/unnamed ranges, empty ranges, node boundaries, invalid/reversed ranges, subtree roots, and malformed variants. Small inputs also receive exhaustive range checks.

- `bytes`: 10,000 source files, 100,992,822 exact comparisons; passed.
- `points`: 10,000 source files, 100,992,822 exact comparisons; passed.
- `group32`: 220 source files, 2,289,478 exact comparisons; passed.
- `group64`: 220 source files, 2,289,478 exact comparisons; passed.
- `sanitized`: 220 source files, 2,289,478 exact comparisons; passed.

Packed-column unit tests passed in both point configurations, both larger group configurations, and the sanitizer build. The sanitizer probe used AddressSanitizer, UndefinedBehaviorSanitizer, and leak detection. The points configuration test exercises byte seeks with point data present; point-range seeking is unchanged.

The paired benchmark verifies identical source/tested hashes, node counts, slab byte sizes, and counted mainline seek differences before/after. Every seek workload row completed all three repeats without a comparison failure; known mainline seek differences retain the existing benchmark policy.

## Reproduction

- Production change: `lib/squat/node.c`; regression probe: `lib/squat/tests/seek.c`.
- Release build: `cargo build --release --locked --no-default-features -p squatter-bench`.
- Raw results, exact commands, driver and aggregation scripts, build logs, and correctness logs: `build/squat-seek/`.
- Input selection and frozen pre-change source: `build/squat-corpus-10k/`.
- GCP results bundle: `/home/mgsloan/squatter-benchmark/corpus-10k/seek-byte/`.
- JSON beside this report includes binary/source hashes, CPU and wall-time aggregates, language results, and run diagnostics.
