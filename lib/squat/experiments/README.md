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
This initial run used a fixed 12-nodes/group capacity estimate in all variants.
That over-reserved the larger builds before compaction; their compact byte counts
remain valid, but the packing timings include that allocation choice. The later
query matrix scales the estimate with group size.
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

## Query workloads

[The query results](query-results-2026-09-09.json) record a separate 88-file
training/holdout sample, bounded to 100 KiB per file, across eleven grammars.
All 120 grammar/Zed query sources compiled in both engines: 1,407 patterns.
Each configuration passed three repeats on original and mutated inputs, including
complete ordered partial-match capture snapshots and built-in text predicates.
The ten configurations cover 16/32/64 slots, unoptimized 16-slot execution, and
repacked 16-slot slabs. This matrix uses the corrected group-capacity estimate.

Per-file paired elapsed-time ratios, squat divided by mainline (lower is better):

| Input | Operation | Median | 90th percentile | Maximum |
|---|---|---:|---:|---:|
| Original | query-captures | 0.625 | 0.759 | 0.853 |
| Original | query-matches | 0.557 | 0.683 | 0.790 |
| Mutated | query-captures | 0.593 | 0.796 | 0.879 |
| Mutated | query-matches | 0.544 | 0.709 | 0.869 |

On original inputs, disabling scan/plan shortcuts increased median per-file
query time by 28.4% for matches and 22.3% for captures. The unoptimized executor
still uses the shared capture coordinator, so this ablation measures the
shortcuts collectively; it does not isolate SWAR from structural plans or field
presence filtering. These cross-run comparisons use per-file medians, unlike
the paired-repeat ratios against mainline within each run.

32-slot groups had no clear query-speed advantage (about 0.5% higher median time
in this sample), and 64-slot groups were about 1–2% higher. Keep 16 as the default.
Repacking reduced the original sample from 21.66 to 18.03 bytes/node, with almost
unchanged median query time. The unrepacked 32/64 variants used 21.39/31.59
bytes/node here; do not compare these allocation-inclusive figures directly with
the earlier compact-layout table's different input sample.

These timings include equal snapshot collection and host predicate evaluation,
and exclude query/regex compilation. Hardware counters were unavailable. Earlier
multi-megabyte generated TypeScript/JavaScript stress cases exceeded the 30-second
mainline timeout or the four-million captured-node snapshot budget. They remain
failed stress cases and are not included in this passing bounded sample.

Reproduce the matrix and validated summary with the commands in
[the corpus tool documentation](../../../tools/squatter/README.md#query-comparisons).

## Cursor caching

[Recorded cursor results](cursor-results-2026-09-09.json) cover 88 files across
all eleven grammars, original and mutated, with five repeats and a 100 KiB input
cap. Every cursor workload passed full ordered identity/attribute comparisons.
Both variants use the same navigation implementation and bulk attribute API.

Median cached time divided by uncached time (lower is better):

| Workload | Original | Mutated | Original files faster |
|---|---:|---:|---:|
| Native forward navigation | 1.040 | 1.028 | 22 / 88 |
| Native backward navigation | 1.064 | 1.080 | 25 / 88 |
| Forward walk with attributes | **0.865** | **0.863** | **81 / 88** |
| Backward walk with attributes, compatibility adapter | 1.578 | 1.611 | 0 / 88 |

Caching saved about 14% of forward attribute-walk time at the median. Navigation
alone generally did not amortize decoding whole groups. The reverse attribute
adapter creates a new cursor per node to preserve mainline's forward field/alias
semantics; the cache's allocation and decoding costs therefore have little reuse.
Its result is not a measurement of a long-lived native reverse cursor reading
attributes. Keep both types and leave `Cursor` as the default.

These are ratios of separate per-file medians within the same run, rather than
paired-repeat ratios between the two cursor variants. Both selectors also report
paired-repeat ratios against mainline in the raw results. Allocation, snapshot
collection, and destruction are timed. Child/descendant counts remain ordinary
node scans in both variants. Hardware counters were unavailable.

The new shared bulk attribute API reduces FFI calls for the uncached cursor too;
these walk measurements should not be compared directly with the earlier walk
numbers. See [reproduction instructions](../../../tools/squatter/README.md#cursor-comparisons).

The [larger-file run](cursor-large-results-2026-09-09.json) passed the same eight
workloads on 53 originals and their mutations, with five repeats. It includes
nine inputs over 1 MiB, up to a 4 MiB cap, and totals 4,741,921 original nodes
(4,426,809 mutated). Across all 53 files, forward attribute-walk ratios were
0.860 for both original and mutated inputs; native forward navigation was near
parity overall (0.978 / 0.994).

For the **nine inputs over 1 MiB**, grouped by original input size:

| Workload | Original cached/uncached | Mutated cached/uncached |
|---|---:|---:|
| Native forward navigation | **0.900** | **0.898** |
| Native backward navigation | 0.963 | 0.951 |
| Forward walk with attributes | **0.855** | **0.865** |
| Backward attribute compatibility adapter | 1.450 | 1.396 |

All nine large inputs benefited from caching in native forward navigation and
forward attribute walks; seven benefited in native backward navigation. More
reuse on larger inputs can amortize cache setup, but the reverse compatibility
adapter remained slower with caching. Both recorded reports include size-band
and per-language statistics, source/binary identities, and per-file measurements.
Reproduce this sample using the cursor command above with `--per-bucket 1` and
`--max-file-bytes 4194304`, in a new output directory.

Mainline's native previous-sibling movement may rescan preceding siblings to
restore columns after crossing a line break (`lib/src/tree_cursor.c`). Its
large-file reverse timings therefore include a cost absent from the sibling
adapter. Cached/uncached ratios compare the two squat cursors directly and are
kept separate from each variant's paired comparison against mainline.
