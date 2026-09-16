# Symbol encoding ablation on GCP

## Findings

Keep the three optional encodings on this evidence. Removing an encoding does
not produce a consistent read-performance improvement that outweighs its storage
or preparation benefit. These are whole-implementation ablations, not isolated
measurements of branch cost.

- **Byte pairs:** removing them changes affected-language cursor/cached-iterator
  scans by +1.28%/+0.45%, with no slab savings and 2,762 extra table bytes.
  Preparation rises from 0.23–2.11 µs to 4.49–465.99 µs. The larger cached-digest
  regression is unstable across passes and does not reproduce in the follow-up.
- **Shared selectors:** removing them changes scans by -0.41%/+0.75% and queries
  by +0.17%/+0.12%. Total symbol tables grow from 82,252 to 218,698 bytes across
  these sixteen grammars. This encoding primarily saves shared memory.
- **Local selectors:** removing them changes aggregate scans by -3.25%/-2.35%,
  but the improvement is concentrated in tiny files. For files ≥64 KiB the
  changes are +0.43%/+1.81%. Queries stay near parity. Shared tables shrink by
  48,820 bytes, while compacted C++/YAML slabs grow by 791,840 bytes across 39
  trees: +9.67% for C++ and +10.89% for YAML.

The fallback remains necessary. None of the sixteen baseline grammars requires
it; removing local selectors sends C++ and YAML to it. All 39 of those sampled
trees retain the grammar-ID column. Synthetic tests cover its omission when all
IDs equal their display IDs, including growth, compaction, scanning, and loading.

Small timing differences need caution: Tree-sitter controls also move by a few
percent, and changing generated code can alter function placement. The results
do not establish that encoding dispatch is free. They do show little reason to
remove an encoding solely to reduce dispatch in these workloads.

## Method

Baseline `c4bd2dfe2` includes the optional grammar-ID tail column. Three builds
independently remove byte pairs, shared selectors, or local selectors. Remaining
encodings retain their selection priority; separate grammar columns remain the
fallback. The experiment removes the disabled decoder paths at compile time,
while keeping descriptor layouts and the rest of the implementation identical.
No hash map is used. The native comparison assertion fix is `ba245dd23`.

2026-09-16, Google Cloud `squatter-benchmark`, `us-central1-a`,
`e2-standard-2`: two vCPUs reported as SMT siblings on one Intel Xeon core at 2.20 GHz, with 55 MiB reported
L3. Benchmarks run sequentially on CPU 0, with no concurrent benchmark or cache
pressure worker. Scheduled package maintenance is paused for the run. Binaries
are built locally in the same Ubuntu image using GCC 13.3 and Rust 1.98.1,
Cargo release settings, without CPU-specific flags. Hardware counters are not
exposed by the VM (`perf_event_open` reports `ENOENT` even with
`perf_event_paranoid=1`); wall time and thread CPU time are available.

The corpus has 237 distinct files across 16 grammars, totaling 26,936,989 source
bytes. It combines the existing seed-42 pressure corpus with three size-selected
files per grammar from the fixed-width experiment. Identical content within a
grammar is deduplicated; files above 2 MiB are excluded. The 204 files no larger
than 64 KiB also run navigation, materialized attribute walks, seeks, and each
grammar's staged upstream highlighting and available tags queries. Larger files
run allocation-free scans/digests and parse-plus-pack only. A pilot including
materialized walks on the large files was stopped for excessive runtime and
kept separately; its timings are excluded. Content hashes, source paths,
exclusions, grammar pins, libraries, and query hashes are recorded in the artifacts.

Four balanced passes put each build in each execution position once. Each
invocation uses seven repeats; the allocation-free scans and digests perform
one traversal per timed sample. A three-traversal calibration run is retained
separately and excluded from the aggregates. The harness alternates
Tree-sitter/Squatter execution order and rotates workload order. It checks results outside timing.
Parse-plus-pack measurements reuse prepared grammars and packing contexts.
Separate preparation measurements time 31 grammar constructions after one
warmup, excluding destruction. Native layout measurements use compacted slabs.

Ratios compare the median of each file's four pass medians, then take geometric
means within each language and across languages. This gives each language equal
weight despite unequal file counts. Affected-language aggregates include only
grammars whose selected encoding changes; unchanged-language results measure
code/dispatch effects without changing those grammars' storage. Tree-sitter
measurements serve as timing controls. Per-language, size-group, CPU-time, and
individual-pass results are retained alongside aggregate wall times.

## Results

### Languages whose encoding changes

Positive changes mean slower execution. Each column compares the affected
languages with the complete baseline; the columns therefore cover different cohorts.

| Workload | Without byte pairs | Without shared selectors | Without local selectors |
| --- | --- | --- | --- |
| scan-forward | +1.28% | -0.41% | -3.25% |
| scan-iterator-cached | +0.45% | +0.75% | -2.35% |
| digest-forward | -2.05% | -3.13% | -5.94% |
| digest-iterator-cached | +6.53% | +0.78% | -2.51% |
| query-matches | -1.46% | +0.17% | -0.21% |
| query-captures | -0.63% | +0.12% | +0.04% |
| setup-parse | +0.42% | -1.04% | -1.38% |
| cursor-forward | -0.81% | -0.24% | +0.83% |
| iterator-forward | -1.03% | +0.05% | +0.62% |
| iterator-forward-cached | -0.66% | +0.03% | +0.42% |
| walk-iterator | -1.12% | -0.10% | -2.67% |
| walk-iterator-cached | +0.42% | +1.47% | -1.81% |
| seek-byte | +0.25% | +0.57% | +0.78% |
| seek-point | -1.44% | +0.25% | +1.09% |

### Controls and unchanged languages

| Removed encoding | Workload | All languages | Unchanged languages | Tree-sitter control (affected) |
| --- | --- | --- | --- | --- |
| no-bytes | scan-forward | -0.32% | -0.85% | -2.41% |
| no-bytes | scan-iterator-cached | -0.49% | -0.80% | -2.24% |
| no-bytes | query-matches | -0.51% | -0.19% | +0.45% |
| no-bytes | query-captures | +0.10% | +0.34% | +0.53% |
| no-bytes | setup-parse | +0.07% | -0.04% | +0.59% |
| no-shared | scan-forward | -0.72% | -1.22% | -2.76% |
| no-shared | scan-iterator-cached | +0.18% | -0.76% | -3.26% |
| no-shared | query-matches | -0.03% | -0.35% | +0.18% |
| no-shared | query-captures | +0.19% | +0.31% | +0.84% |
| no-shared | setup-parse | -0.92% | -0.73% | -0.99% |
| no-local | scan-forward | +0.31% | +0.83% | -0.46% |
| no-local | scan-iterator-cached | +1.12% | +1.63% | +0.32% |
| no-local | query-matches | +0.15% | +0.21% | -0.41% |
| no-local | query-captures | +0.56% | +0.63% | -0.87% |
| no-local | setup-parse | -0.23% | -0.06% | -1.03% |

### Per-language effects

| Removed encoding | Language | Cursor scan | Cached iterator scan | Query matches | Query captures |
| --- | --- | --- | --- | --- | --- |
| no-bytes | css | +2.50% | +0.73% | -1.31% | -1.19% |
| no-bytes | go | +0.95% | -0.93% | -0.03% | -0.03% |
| no-bytes | html | +1.46% | +0.84% | -3.61% | -0.23% |
| no-bytes | json | +0.23% | +1.20% | -0.83% | -1.07% |
| no-shared | bash | -1.13% | +0.42% | +1.16% | -0.40% |
| no-shared | c | -0.55% | +1.06% | -0.71% | +1.00% |
| no-shared | csharp | -1.08% | -1.02% | -0.35% | +0.00% |
| no-shared | java | -1.68% | -0.38% | +1.55% | +1.86% |
| no-shared | php | -1.97% | +0.38% | -0.81% | -0.96% |
| no-shared | python | -0.10% | +0.35% | +0.69% | +0.78% |
| no-shared | ruby | -1.21% | +0.07% | -0.00% | -0.30% |
| no-shared | rust | +1.64% | +1.68% | +0.51% | +0.14% |
| no-shared | tsx | -0.32% | +1.76% | -0.12% | -0.56% |
| no-shared | typescript | +2.35% | +3.21% | -0.19% | -0.38% |
| no-local | cpp | -3.13% | -1.33% | +0.12% | +1.53% |
| no-local | yaml | -3.37% | -3.36% | -0.55% | -1.42% |

### Size sensitivity

| Removed encoding | Source size | Cursor scan | Cached iterator scan |
| --- | --- | --- | --- |
| no-bytes | tiny | +2.68% | +2.27% |
| no-bytes | small | +0.85% | -0.06% |
| no-bytes | medium | -0.08% | +0.02% |
| no-bytes | large | +0.19% | -0.09% |
| no-shared | tiny | +2.25% | +3.79% |
| no-shared | small | +0.35% | +1.45% |
| no-shared | medium | -1.61% | -0.26% |
| no-shared | large | -1.63% | -0.72% |
| no-local | tiny | -7.47% | -7.89% |
| no-local | small | -1.78% | -0.23% |
| no-local | medium | +0.30% | +1.86% |
| no-local | large | +0.43% | +1.81% |

Buckets are <1 KiB, 1–16 KiB, 16–64 KiB, and ≥64 KiB. Each uses only
languages affected by that ablation. Tiny-file percentages are especially
sensitive to timing overhead.

### Shared symbol tables

Bytes per prepared grammar, including packing and validation maps; other shared
grammar metadata and the fixed descriptor are excluded.

| Grammar | Baseline encoding | Baseline bytes | No bytes | No shared | No local |
| --- | --- | --- | --- | --- | --- |
| bash | shared | 2408 | 2408 | 5640 | 2408 |
| c | shared | 3050 | 3050 | 7300 | 3050 |
| cpp | local | 11200 | 11200 | 11200 | 1120 |
| csharp | shared | 4332 | 4332 | 19044 | 4332 |
| css | bytes | 292 | 1252 | 292 | 292 |
| go | bytes | 442 | 1800 | 442 | 442 |
| html | bytes | 86 | 366 | 86 | 86 |
| java | shared | 2602 | 2602 | 6460 | 2602 |
| json | bytes | 54 | 218 | 54 | 54 |
| php | shared | 3608 | 3608 | 15624 | 3608 |
| python | shared | 2268 | 2268 | 5520 | 2268 |
| ruby | shared | 3010 | 3010 | 7060 | 3010 |
| rust | shared | 3006 | 3006 | 47124 | 3006 |
| tsx | shared | 3352 | 3352 | 27336 | 3352 |
| typescript | shared | 3206 | 3206 | 26180 | 3206 |
| yaml | local | 39336 | 39336 | 39336 | 596 |

### Compacted slabs without local selectors

| Grammar | Baseline bytes | No-local bytes | Growth | Trees retaining grammar column |
| --- | --- | --- | --- | --- |
| cpp | 1836712 | 2014312 | +9.67% | 19 |
| yaml | 5638344 | 6252584 | +10.89% | 20 |

Other ablations keep slab sizes unchanged. Slab extents exclude fixed runtime
descriptors and allocator slack; optional allocation tails below 256 bytes may
remain allocated even though they are excluded from serialization.

### Grammar preparation

| Grammar | Byte pairs | Without byte pairs | Ratio |
| --- | --- | --- | --- |
| css | 0.64 µs | 97.86 µs | 151.7× |
| go | 2.11 µs | 465.99 µs | 220.4× |
| html | 0.23 µs | 7.86 µs | 33.4× |
| json | 0.32 µs | 4.49 µs | 14.2× |

Preparation is reusable and excluded from the parse-plus-pack and read timings.

### Correctness exclusions

8 source/workload pairs are excluded consistently from every build.
The raw results retain failures; no timing is credited for an incorrect result.

| Grammar | Workload | Source |
| --- | --- | --- |
| bash | seek-byte | bash/c998ff4f6b57efc8.sh |
| bash | seek-point | bash/c998ff4f6b57efc8.sh |
| css | seek-byte | css/617a666b3374d18a.css |
| css | seek-byte | css/6bb2a46eabcef2d5.css |
| css | seek-byte | css/fb770941b3a08935.css |
| css | seek-point | css/617a666b3374d18a.css |
| css | seek-point | css/6bb2a46eabcef2d5.css |
| css | seek-point | css/fb770941b3a08935.css |


### Timing stability and alignment follow-up

The no-byte-pair cached-iterator digest changes across the four main passes are
+12.85%, +10.41%, +1.04%, and +0.86% on affected languages. Its +6.53% aggregate
therefore does not describe a stable penalty. Early slowdowns also occur in
languages whose encoding remains unchanged.

A follow-up uses one file nearest 8 KiB per grammar, three rotating build orders,
nine repeats, and thirty traversals per sample. It compares the baseline,
original no-byte-pair binary, and a no-byte-pair binary with only
`sq_node_iterator_next` aligned to 64 bytes. The alignment restores the iterator
functions' offsets within cache lines without changing their instruction bodies
or decoding logic.

| Affected-language workload | Original no-byte build | Aligned no-byte build |
| --- | ---: | ---: |
| Cursor scan | -0.07% | -1.05% |
| Cached iterator scan | +0.80% | -0.98% |
| Cursor digest | +1.37% | -0.65% |
| Cached iterator digest | -0.60% | -0.71% |

The large digest slowdown does not reproduce even without changing alignment,
so this experiment does **not** establish alignment as its cause. Across all
sixteen languages, aligning the function improves the no-byte cached scan by
about 1.3% but does not improve the cached digest. Treat the earlier large
penalty as unresolved run/workload sensitivity, not an inherent byte-pair benefit.

The main no-local scan improvements are more repeatable: cursor changes range
from -4.53% to -1.31%, and cached-iterator changes from -2.49% to -1.55%. Their
size dependence remains important. No-shared cursor changes range from -1.40%
to +0.93%, and cached-iterator changes from -0.11% to +1.80%. CPU-time aggregates
track the corresponding wall-time aggregates closely.

### Coverage and checks

The bulk corpus has 17 Bash files; 19 each for C, C++, CSS, Go, and HTML; 23 JSON;
21 Python; 25 TSX; 21 TypeScript; 20 YAML; and three each for C#, Java, PHP, Ruby,
and Rust. Supplemental languages therefore have less file diversity than the
original eleven. The ≥64 KiB no-local cohort contains only three files, so it
is evidence against a uniform speedup rather than a precise general estimate.

All 48 main invocations completed. Bulk and query comparisons have zero
failures. Every build and pass reproduces the same eight excluded seek pairs:
four files, each failing byte and point seeks. The 896 raw failures are these
same eight pairs repeated seven times in sixteen build passes. They are baseline
discrepancies, not introduced by an ablation. Native structural/persistence checks
passed for every build and grammar. Native unit/supertype tests and ASan/UBSan
checks passed for the grammar-column change. All nine alignment invocations
passed their comparisons.

All retained measurement processes ran sequentially. A brief overlapping
restart attempt was rejected and is kept under `rejected-restart/`; it contributes
no samples. Pilot/calibration results are also separate. Collection of the
first-pass preview occurred between benchmark processes. The VM was stopped
and its original four-vCPU machine type restored after collection.

## Reproduction

`build/symbol-ablation/` contains source and executable hashes, `source.tar`,
`ablation.patch`, `check.patch`, and the four binaries. `prepare-build.py` and
`build.sh` record source preparation and compilation; `stage-cloud.py` records
corpus selection. `run-cloud.py` executes the sequential matrix and
`summarize.py` computes the aggregates. `summary.json` contains per-language,
size-group, CPU-time, and per-pass results. `alignment.patch`, `alignment-run.py`,
and `alignment-summary.json` preserve the follow-up. `final-results.tar.gz`
contains the staged corpus, grammars, queries, and cloud outputs. Cloud
`commands.json` records commands, exit statuses, and durations; `results/` contains benchmark manifests and
per-file measurements, and `layouts/` contains compacted storage measurements.
