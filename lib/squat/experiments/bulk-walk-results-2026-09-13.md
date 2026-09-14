# O(1) bulk walks and decoder caching — 2026-09-13

Bulk cursor snapshots reduce native walk time by 40–42% versus individual
getters. Cached bulk iterators save another 14–21% versus uncached bulk
iterators. These comparisons precede the additional scalar decoder cache
measured below. Keep both the O(1) snapshots and decoder cache.

## Method

Small cloud VM `squatter-benchmark`, GCP `mgsloan-compute/us-central1-a`,
e2-standard-2, Intel Xeon Broadwell 2.20 GHz, CPU 0. GCC 15.3,
`-O3 -g -fno-omit-frame-pointer`, default packing, points off/on separately.
The retained direct-field packing cache and byte-only reverse position changes
are present in both builds; point positions still use the forward calculation.

The new [attributes.c](attributes.c) probe walks 9 large files, 200 typical
files (100 C + 100 Python), and 854 tiny files across C++, JSON, Python, and
TypeScript. Each grammar/bucket is one batch. Every timed mode consumes type
and grammar-type strings, public and grammar symbols, byte extents, optional
point extents, and named/extra/missing/error/has-error flags. Counts, field IDs,
and depth are excluded. The C bulk getters still decode an O(1) field ID;
the checksum ignores it. Timed string consumption reads their first byte;
full strings and all snapshot values are compared outside timing.

Parsing, packing, and differential validation are outside timed CPU regions.
Cursor/iterator creation and deletion are included. No Rust observation-vector
allocation or collection work is included, so these numbers do not replace the
historical end-to-end Rust walk measurements.

Each process calibrates batches to at least 10 ms and takes seven samples,
rotating the mode order. The baseline matrix uses five processes per case;
the decoder trial uses three alternating before/candidate process pairs per
case. Each table uses the median within a process, median across processes,
then sums batch medians for the stated sample. Percentages are changes in
time, not throughput; small changes around 1–2% should be treated as noise.
All 44 cases completed in each matrix. Both builds validate against mainline
and enforce equal file/node/byte/slab-byte counts and checksums.

## Bulk getters and cached iterators, before decoder caching

| Points | Sample | Cursor bulk vs individual | Cached vs uncached bulk iterator | Cached / mainline time |
|---|---|---:|---:|---:|
| off | large | -41.96% | -21.01% | 0.231× |
| off | typical | -40.73% | -15.30% | 0.244× |
| off | tiny | -41.83% | -19.82% | 0.280× |
| on | large | -42.08% | -18.99% | 0.259× |
| on | typical | -40.03% | -13.61% | 0.255× |
| on | tiny | -41.80% | -18.17% | 0.284× |

## Scalar decoder-parameter cache

Adapted only the C portion of `90011841a` from `investigate/rust-reader` to the
current v6 column layout. Store symbol/field lanes-per-word and masks once in
`SQLayout`, then reuse them for symbol, grammar-symbol, and field scalar reads.
Fixed widths 1/8/16/32 retain their direct reads. The runtime tree grows eight
bytes; slab offsets and persisted bytes do not change. No experimental Rust
reader or older serialized layout was imported.

| Points | Sample | Individual cursor | Bulk cursor | Node bulk | Uncached bulk iterator | Cached bulk iterator | Mainline control |
|---|---|---:|---:|---:|---:|---:|---:|
| off | large | -23.72% | -16.21% | -14.79% | -21.35% | -1.22% | -1.23% |
| off | typical | -22.15% | -13.65% | -13.03% | -19.61% | +0.36% | +1.69% |
| off | tiny | -22.43% | -15.31% | -13.79% | -20.68% | -0.77% | -0.53% |
| on | large | -21.62% | -15.01% | -11.59% | -18.32% | +0.36% | -0.45% |
| on | typical | -20.69% | -12.80% | -9.81% | -16.80% | +3.79% | +4.19% |
| on | tiny | -20.99% | -15.48% | -12.43% | -17.72% | -0.28% | +2.99% |

The gain is in scalar ID reads. The cached iterator already unpacks blocks and
remains the fastest traversal overall. Most cached controls change by less than
1.3%; the points-enabled typical sample rises 3.79%, alongside a 4.19% rise in
mainline control time. That case is inconclusive rather than evidence of a
cached-path improvement. This experiment does not establish a packing-speed
improvement.

## Profiles and next work

Byte-only typical C/Python profiles, about 12 seconds each, used `cpu-clock:u`
at 997 Hz. Baseline cursor-bulk snapshots account for 53.1/53.5% of samples.
Cached iterator snapshots account for 32.0/32.6%, block ID unpacking for
20.9/20.2%, and iterator advance for 15.7/16.3%. Language name/metadata/public
symbol accessors add about 12% on that cached path. These profiles include
common startup validation/calibration, so percentages are indicative.

Next, investigate cached snapshot construction and grammar metadata lookup,
then block unpacking. Preserve tiny-file and memory/setup-cost checks when
considering more caches. These are leads from the profile, not measured wins.

## Batch consumers and docs

`squatter-bench` now reuses a `PackContext` per grammar across read/query setup
files, batches, and repeats. Context construction is outside setup timing.
Explicit `cold-parse` selection preserves a fresh parser and one-shot packing;
read/query-only prerequisites are labeled `setup-parse` instead. Run metadata
records that distinction, and summaries accept older manifests with their
original `cold-parse` prerequisite rows.

Removed the stale `conversion-optimizations.md` roadmap; the short active
[performance-next.md](../../../performance-next.md) links completed and rejected
experiments instead of keeping them as untried suggestions.

## Validation and artifacts

- All widths 1–32 checked against packed-word reference decoding; layout
  constants and getters checked through growth and compaction.
- Exact slab bytes: 9 large inputs × 2 point modes × 4 packing variants.
- C differential and query checks across 11 grammars, points on/off; copied
  and borrowed loads, compaction, edited positions, context reuse, trimming,
  and injected allocation failures are covered by the existing checks.
- ASan/UBSan and leak checks passed across 11 grammars with both point modes.
- Rust tests and 8 batch integration runs passed: 16 files, 2 repeats, batch
  size 3, points on/off, cold/read setup, original/mutated inputs. Four further
  query-context runs passed matches and captures on the same files with an
  all-named-node query, points on/off and original/mutated inputs.
- Summary replay passed for current and legacy iterator manifests, and query
  representation totals were checked against actual `setup-parse` rows. Python
  summary syntax checks and Rust formatting passed.

[Raw timings, profiles, input hashes, drivers, and source/binary hashes](bulk-walk-cloud-results-2026-09-13.json).
[Standalone decoder-cache patch](decoder-cache-2026-09-13.patch).
The patch can be reversed against this runtime to recover the scalar baseline;
local build/check logs are under `build/bulk-walk/`.
