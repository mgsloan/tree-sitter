# Layout and equality experiments

The [recorded run](results-2026-09-09.json) includes every source hash, grammar
identity, input-level result, and tool snapshot hash. It covers 27 files in eleven
grammars, totaling 2,809,297 visible nodes. These are convenience samples, with
large JSON and Python files contributing heavily to the node-weighted totals.

| Group slots | Column alignment | Bytes/node | Occupied slots | Sum of median pack times |
|---:|---:|---:|---:|---:|
| 16 | 8 | 16.20 | 96.5% | 1,897 ms |
| 32 | 8 | 15.24 | 91.1% | 1,838 ms |
| 64 | 8 | 17.10 | 76.5% | 1,882 ms |
| 16 | 64 | 16.21 | 96.5% | 1,811 ms |

These are actual compact slabs, including indexes, dictionaries, and field
exceptions. The cache-line variant aligns allocations and column offsets.
Packing repeats seven times; scheduling and thermal noise limit timing claims.
Every variant passed unit checks and grammar comparisons on small inputs and
mutations. The differential checks include serialization and all encoded-column
SWAR masks, including 64-slot mask boundaries.

32 slots wins this sample's aggregate size by about 6%, but is worse for CSS, Go,
HTML, and especially TypeScript (23.50 versus 20.75 bytes/node). 64 slots wastes
much more space on those grammars. Keep the specified 16-slot default; the sample
is not enough to overturn the design's earlier broader Pareto result. The larger
formats remain available as build-time experiments. Cache-line padding increases
small-file overhead and has no clear demonstrated access-speed benefit here.

The 16-slot byte models suggest sparse grammar IDs could save 2,620,468 bytes
(5.8% of total slab bytes), and variable-width supertypes 1,381,976 bytes (3.0%).
These models include sparse alias bitmaps and rank checkpoints. They do not
measure the extra access/packing work or implement a new persisted layout.
Interleaving symbol and field lanes increased these columns by 110,912 bytes
(2.5%); retain separate columns for independent scans. Only 48 bytes of field
lookup exceptions were needed across this sample.

## Equality kernels

All kernels use the same non-straddling packed words, including unused tail bits.
The input has 131,075 words, which exercises SIMD remainder handling. Each kernel
is checked against scalar counts before eleven timing repeats of sixteen batches.
The indirect volatile call prevents the compiler from hoisting repeated scans.

Median nanoseconds per value on the recorded machine:

| Kernel | 8-bit | 9-bit | 12-bit |
|---|---:|---:|---:|
| Scalar extraction | 1.293 | 1.258 | 1.277 |
| Portable SWAR | 0.483 | 0.550 | 0.770 |
| SWAR + hardware popcount | 0.183 | 0.204 | 0.291 |
| Compiler-targeted AVX2 | 0.183 | 0.208 | 0.297 |
| Explicit SSE2 | 0.142 | 0.163 | 0.228 |
| Explicit AVX2 | 0.122 | 0.139 | 0.196 |

Explicit AVX2 improves bulk counts over the fair hardware-popcount SWAR baseline
by roughly 1.5× for these widths. The compiler-targeted function alone did not
produce that improvement. Keep the explicit kernels as requested. Portable SWAR
remains the runtime group-equality implementation: the benchmark counts a long
column, whereas group lookup must produce exact slot masks and handle boundaries.
A count-kernel speedup is not yet evidence of an equal query-engine speedup.

Conversion already buffers absolute values for one provisional group. Optimistic
full-group placement is deferred: inserting padding changes buffered ancestor
spans and field targets, so an implementation would need transactional placement
state. Establish query workloads and conversion profiles before adding that
complexity. Likewise, presence-index threshold changes need query measurements;
the format's existing threshold of 32 groups remains unchanged.

Reproduce with:

```sh
python3 tools/squatter/run.py --output build/layouts --per-bucket 1 \
  --skip-benchmarks --skip-sampling
python3 tools/squatter/summarize.py build/layouts --output layout-results.json
```
