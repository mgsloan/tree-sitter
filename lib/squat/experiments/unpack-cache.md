# Iterator unpack cache

Historical measurements supporting removal of the iterator unpack cache.
The cache, constructor flag, and cached benchmark variants have since been removed.

Measured 2026-09-16 at `1c6c5da2d2932af922019bb9af8ca78565a851ea`, with
uncached digest/scan selectors added to `squatter-bench`. No runtime changes.

The default cache expands one 16-slot physical group into absolute start/end
bytes and points. Byte-range reads fill both byte columns; bulk attribute reads
also fill both point columns. Crossing a group invalidates the window. IDs and
flags still come directly from the slab. Navigation and ordinary node getters
do not use the cache. Each cached iterator allocates 400 extra bytes on x86-64;
there is no shared or persistent state. Automatic byte-coordinate decoding uses
AVX2 on this machine.

## Method

- Intel Core Ultra 7 165U, pinned to CPU 0; GCC 15.3.0, Rust 1.95.0, release build.
- Default 16-slot groups/window, points enabled, same binary for both paths.
- Existing staged corpus from `build/squat-pressure-gcp-20260915`, restricted to
  files under 256 KiB: 175 files, 11 grammars, 1,371,933 source bytes. Actual files
  range from empty to 85,386 bytes; median 4,061 bytes.
- Warm: seven repeats, 20 traversals per scan/digest measurement. Every traversal
  creates a fresh iterator. Walk/navigation use one pass and include result
  vectors and identity-map lookups.
- Wash: smallest, median-sized and largest bounded file per grammar, 33 files;
  five repeats, one traversal after touching a randomized 24 MiB working set
  (twice this machine's LLC). Washing happens outside the timed operation.
- Workload/backend order rotates. Each variant checks against mainline Tree-sitter.
- Compare Squatter cached/uncached per-file median thread CPU times directly,
  then take the median of those ratios. Mainline timings are not the denominator.

An unrelated compilation was active. CPU time excludes descheduling but not
frequency or shared-resource effects. Instruction counts provide additional
evidence. These results cover dense full-attribute reads, not selective byte-only
reads, sparse access, large generated files, other CPUs, or points-disabled trees.
The unrestricted run and full-corpus wash were stopped for runtime and excluded.

Raw measurements, manifests, exact commands, benchmark patch, and the summary
script are in `build/unpack-cache-20260916/`.

## Results

Warm median changes with caching enabled (negative means less time/work):

| Workload | Thread CPU time | Instructions |
| --- | ---: | ---: |
| Full-attribute scan | -4.1% | -5.6% |
| Attribute digest | -3.0% | -5.2% |
| Allocating attribute walk | -1.7% | -4.2% |

The 175 files contain 368,743 nodes. Warm scans improve on 165/175 files;
per-language median savings range from 2.9% to 6.0%. The ratio of summed per-file
median CPU times is 0.955 for scans and 0.970 for digests. Restricting to trees
with at least 1,000 nodes gives similar median ratios, 0.958 and 0.970.

Navigation reports 10.9% lower median CPU time but essentially identical
instruction counts (ratio 1.0001). It never fills the cache, so this is not an
unpacking benefit. Its allocating workload is sensitive to allocator/cache state
and order; do not use that result to justify the unpack cache.

After a cache wash:

| Workload | Median CPU change | Sum of per-file median CPU times |
| --- | ---: | ---: |
| Full-attribute scan | +3.0% | +0.1% |
| Attribute digest | -1.3% | -7.6% |

The cold results are mixed and size-dependent: among trees with at least 1,000
nodes, median scan/digest ratios are 0.965/0.930. The 33-file sample deliberately
includes tiny trees, where fixed overhead matters. Do not interpret the overall
scan median as a universal cold-cache regression. All completed runs have zero
comparison failures.

## Assessment

This is a marginal optimization: a consistent 3–4% warm benefit for dense
coordinate reads, with mixed cold results. Its 400-byte memory cost is small;
the main cost is maintaining the alternate decoding path and cache-aware API.
Removing it during traversal simplification is reasonable. These measurements
do not justify carrying an unpack cache into the proposed group-based scan API.
Keeping the existing opt-in path is defensible if dense attribute traversal is a
measured application bottleneck. The subsequent deletion retains the uncached iterator and its benchmark workloads.
