# Absolute u32 iterator cache — 2026-09-09

The cached iterator now widens coordinate deltas directly from the slab into
absolute u32 values, using SIMD broadcast-base addition/subtraction. This
experiment compares it with the **uncached iterator**, with 16-slot slab groups
and 16/32/64/128-slot unpack windows. No delta-cache baseline was benchmarked.

## Cached versus uncached attribute walks

Numbers are cached elapsed time divided by uncached elapsed time; lower is
faster. For example, 0.960 means 4% less elapsed time. These are equal-file medians
of ratios of each file's separate eight-repeat timing medians. Allocation sizes
include the iterator itself; an uncached iterator uses 48 B.

| Window | Cached allocation | Bounded original | Bounded mutated | ≥1 MiB original | ≥1 MiB mutated |
|---:|---:|---:|---:|---:|---:|
| 16 | 552 B | 0.960 | 0.957 | 0.996 | 0.986 |
| 32 | 1,032 B | 0.944 | 0.949 | 0.989 | 0.979 |
| 64 | 1,992 B | 0.934 | 0.931 | 0.972 | 0.970 |
| 128 | 3,912 B | 0.929 | 0.936 | 0.966 | 0.966 |

The bounded sample contains 88 files across eleven grammars (up to 100 KiB).
The large-file columns isolate the nine files at least 1 MiB from a 53-file mixed
sample (up to 4 MiB). Original and deterministic mutated inputs are separate.
The complete mixed sample, per-file values, CPU timings, and ratio quantiles are
preserved in the machine-readable results.

Caching does not win on every file: at 128 slots, 7 of 88 original files and
10 of 88 mutated files had measured ratios above 1.0. The largest regressions
were on very small trees. Among trees with fewer than 100 nodes, the 128-slot
median was 0.952 for originals and 1.000 for mutations; these microsecond-scale
walks can have noisy timing controls. Keep the uncached constructor available.

## Window sizes compared with 16-slot caching

These ratios compare cached walks across builds; all builds have the same slab
layout and differ only in unpack-window size.

| Window | Cached allocation | Bounded original | Bounded mutated | ≥1 MiB original | ≥1 MiB mutated |
|---:|---:|---:|---:|---:|---:|
| 16 | 552 B | 1.000 | 1.000 | 1.000 | 1.000 |
| 32 | 1,032 B | 0.977 | 0.994 | 0.993 | 0.988 |
| 64 | 1,992 B | 0.955 | 0.977 | 0.985 | 0.989 |
| 128 | 3,912 B | 0.968 | 0.972 | 0.977 | 0.981 |

Cross-build mainline controls drifted by up to about 1.9% on bounded inputs
and 1.5% on large files. Interpret small cross-build differences cautiously;
the cached/uncached comparison within each binary is the primary result.

Keep the 16-slot default and preserve all four build options. Wider windows
trade additional per-iterator memory and speculative decoding for better
amortization; the whole-tree measurements do not establish the best size for
short subtree walks or applications holding many iterators.

## Timing controls

Mainline uses its same ordinary cursor under both workload names. Its
cached-labeled/uncached-labeled timing ratios expose workload-order bias:

| Window | Cached allocation | Bounded original | Bounded mutated | ≥1 MiB original | ≥1 MiB mutated |
|---:|---:|---:|---:|---:|---:|
| 16 | 552 B | 1.001 | 0.997 | 0.998 | 0.999 |
| 32 | 1,032 B | 0.996 | 1.000 | 1.004 | 1.000 |
| 64 | 1,992 B | 0.999 | 0.998 | 1.001 | 1.000 |
| 128 | 3,912 B | 0.997 | 1.000 | 0.999 | 1.001 |

The harness rotates workload positions and backend order; eight repeats balance
both positions for these two workloads. These are complete attribute walks,
including allocation, attribute collection, depth reconstruction, and result
recording, rather than isolated SIMD instruction timings. IDs are also cached,
so the cache/no-cache comparison does not isolate coordinate arithmetic alone.

## Implementation and validation

- AVX2 widens eight unsigned u8/u16 deltas into u32 lanes and applies a broadcast
  base with lane-local addition or subtraction. SSE2 handles four lanes; the
  portable implementation uses unsigned scalar arithmetic. Runtime dispatch
  checks AVX2 support once when constructing a cached iterator.
- Bases change every 16 physical slots, including inside wider windows. Decode
  stops at the final live group. Waste slots may be decoded but are never
  returned as nodes. Field-only access fills only the field column; navigation
  alone does not unpack any columns.
- The cache holds three u16 ID columns and six absolute u32 coordinate columns.
  One-bit flags remain packed and counts use existing tree operations. The
  serialized slab and boolean cached/uncached API are unchanged.
- All 16 cloud operations passed, with eight repeats per file and zero
  unexpected comparison failures. The result validator checks input hashes,
  binary hashes, repeat coverage, node counts, and equal slab layouts across
  window builds.
- C tests passed all four windows and alternate 32/64-slot slab groups, with
  JSON, TypeScript, CSS, and YAML fixtures. Scalar, SSE2, and AVX2 reconstruction
  passed ASan/UBSan; unit cases cover high-bit bases, unsigned wraparound, exact
  allocation boundaries, and every short vector tail. Rust tests/doctests,
  formatting, and strict Clippy passed.
- Release disassembly contains `vpmovzxbd`, `vpmovzxwd`, `vpbroadcastd`, `vpaddd`,
  and `vpsubd`. Measured and committed iterator preprocessing matches for all
  four windows; subsequent source edits were explanatory comments only.

## Artifacts and reproduction

- [Validated per-file results, summaries, and manifests](iterator-absolute-results-2026-09-09.json)
- [Implementation and build options](../README.md#preorder-node-iterator)
- [Benchmark runner instructions](../../../tools/squatter/README.md#iterator-comparisons)
- [Questions and decisions](../../../questions-and-decisions-for-human.md#absolute-coordinate-unpacking)

The benchmark ran on `squatter-benchmark` in `mgsloan-compute/us-central1-a`:
`e2-standard-2`, two vCPUs, 8 GiB, Intel Broadwell, pinned to CPU 0. Binaries were
built locally with GCC 15.3 / Rust 1.95 and uploaded; nothing was compiled on the
VM. Guest commands use its glibc 2.39 loader explicitly. Source and binary hashes,
CPU topology, commands, and process-stat snapshots accompany the results.

Build from the same source with `SQ_ITERATOR_CACHE_ALL=2`, `SQ_GROUP_SIZE=16`,
and `SQ_ITERATOR_UNPACK_SLOTS=16/32/64/128`, preserving each resulting binary:

```sh
CFLAGS="-DSQ_ITERATOR_CACHE_ALL=2 -DSQ_GROUP_SIZE=16 -DSQ_ITERATOR_UNPACK_SLOTS=16" \
  cargo build --release --locked -p squatter-bench
```

The uploaded bundle already contains the saved corpora and grammar libraries.
Run the four binaries sequentially with:

```sh
python3 benchmark-upload.py ~/squatter-benchmark --repeat 8 \
  --variants absolute16 absolute32 absolute64 absolute128 \
  --output-name absolute-results --unpack-sizes \
  --benchmarks walk-iterator walk-iterator-cached
```

Use a fresh output name when repeating. Local raw results, build logs, source
snapshot, and exact uploaded binaries live under `build/squat-iterator/`:
`absolute-results/`, `absolute-build-manifest.json`, `source-absolute/`, and
`binaries/absolute{16,32,64,128}/`. `absolute-sizes.txt` records compiled allocation
sizes; `absolute-preprocessed-source.txt` records preprocessor fingerprints.
