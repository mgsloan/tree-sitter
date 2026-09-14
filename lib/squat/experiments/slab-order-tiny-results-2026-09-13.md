# Slab column order on very small inputs, 2026-09-13

**Superseded by the [cloud rerun](slab-order-tiny-cloud-results-2026-09-13.md).**
The user reported a laptop power-state change during these local measurements;
use the cloud run for performance conclusions.

The reordered slab has no substantial timing advantage in this warm-batch test.
On files up to 64 bytes, byte-only packing/seeks improve by about 0.9%/0.8%;
points-enabled changes are within 0.7%. Larger buckets also stay close to flat,
with a 1.4% points-enabled seek slowdown in the 257–1024 byte bucket.

Positive changes mean slower. Each number is the median of seven paired
aggregate elapsed-time ratios. Each pair sums its per-grammar batch times;
each batch time is the median of seven timed samples. Grouping paired runs
before taking a median reduces sensitivity to changes in machine speed.

| Points | Source bytes | Files | Ordinary pack | Context pack | Cursor walk | Byte seeks |
|---|---|---:|---:|---:|---:|---:|
| Disabled | 1–64 | 126 | -0.90% | +0.23% | +0.19% | -0.75% |
| Disabled | 65–256 | 328 | +0.14% | +0.06% | +0.10% | +0.35% |
| Disabled | 257–1024 | 400 | -0.39% | +0.02% | -0.20% | +0.08% |
| Enabled | 1–64 | 126 | +0.25% | +0.64% | +0.23% | -0.21% |
| Enabled | 65–256 | 328 | -0.18% | -0.12% | +0.04% | -0.02% |
| Enabled | 257–1024 | 400 | -0.25% | -0.12% | -0.04% | +1.42% |

## Inputs and locality

854 distinct, nonempty, non-whitespace files, sampled deterministically from
`build/squat-corpus-10k/corpus`. Content hashes remove duplicate files within
each grammar. Seed 20260913 selects at most 100 files per grammar/size bucket.
These are corpus files, including test snippets, not generated repetitions.

| Grammar | 1–64 bytes | 65–256 bytes | 257–1024 bytes |
|---|---:|---:|---:|
| C++ (`.cpp`, `.hpp`) | 0 | 43 | 100 |
| JSON | 25 | 94 | 100 |
| Python | 16 | 91 | 100 |
| TypeScript | 85 | 100 | 100 |

100 of the 126 smallest files occupy exactly one live slab group. The middle
bucket has 26 one-group trees; the largest bucket has none. Counts describe live
groups, not reserved capacity: all measurements use default noncompact packing.
No C++ input in the sampled corpus met the smallest bucket's criteria.

## Method

Same baseline/candidate snapshots and machine as the
[previous layout comparison](slab-order-results-2026-09-13.md): baseline
`7533e47d9c2d95398431edb325b4c03713c4f7a4`, candidate with the pending slab order,
SQLayout member order, resize-copy order, and version-6 changes. Intel Core Ultra
7 165U, CPU 0, GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`, 16-slot groups,
eight-byte column alignment. Separate builds enable and disable points.

[The harness](tiny-layout.c) parses and prepares each batch before timing.
It calibrates a power-of-two batch repetition count to reach at least about
10 ms, then collects seven process-CPU-time samples. Seven alternating process
pairs per grammar/bucket/configuration give 22 rows and 154 process pairs.

Ordinary packing and reusable-context packing both include output allocation
and deletion; context creation and parsing are excluded. Cursor walks create
and delete a cursor and read all cursor attributes at every node. Seeks perform
eight deterministic zero-length byte-range lookups spread across each source,
including endpoints, and read the returned byte bounds and symbol. Prepared
read batches are repeatedly revisited without explicit cache eviction. This
measures warm access, not the first access after parsing or a cold-cache read.

Every input's full-walk checksum matches mainline. Before/after pairs match
file, node, group, one-group, source-byte, and slab-byte counts, plus checksums
for each operation. Seek results are checked across layouts, not against
mainline in this harness. No timings include checksum comparison work inside
the measured interval beyond accumulation of the operation results themselves.

## Investigated outlier

The initial ratio of separate medians suggested a 60.7% improvement for
points-enabled seeks on the 16 Python files below 65 bytes. Raw samples show
both binaries transitioning from approximately 1.7 to 4.4 microseconds per batch;
the transition straddled one before/after pair, placing the two medians on
opposite sides. The cause of that timing transition was not established.

A fresh nine-pair batch run measured +0.15% instead, with individual pair ratios
between 0.996 and 1.007. Three additional pairs on each of the 16 individual
files gave median changes between -0.63% and +0.51%. The apparent large gain
is therefore not reproducible and should not be attributed to the layout.
The table above uses paired aggregation throughout; raw results retain both
aggregation methods and all follow-up samples.

## Artifacts and reproduction

[Raw measurements, summaries, input hashes, and follow-up runs](slab-order-tiny-results-2026-09-13.json).
The harness is available as the `tiny-layout-bench` Makefile target:

```sh
make -C lib/squat ../../build/squat/tiny-layout-bench
build/squat/tiny-layout-bench GRAMMAR_LIBRARY GRAMMAR_SYMBOL 7 SOURCE...
```

Build both revisions with identical `CFLAGS` and matching point modes. Run each
with the same input list, alternating order and pinning CPU affinity. Local
sampling/build/run/aggregation scripts are saved in `build/slab-order-tiny`;
its `inputs.json` fixes all paths and hashes. The before/after snapshots and
four benchmark binaries remain in `build/slab-order/{before,after}/build/p{0,1}`.

These results do not cover queries, iterators, compact slabs, single-group-only
sampling, forced cache eviction, or the cloud benchmark machine.
