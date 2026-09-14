# Slab column reorder timing, 2026-09-13

No material overall timing change on this machine. Points-enabled results are
within about half a percent of the previous layout. Byte-only typical-file seeks
show a small consistent slowdown; the largest aggregate movement is +1.26% for
byte-only tag-query matches. These results do not establish a performance benefit
from the reorder, nor a substantial regression.

Positive percentages mean slower. Totals sum the per-input median of five process
measurements (each process itself repeats the timed operation).

| Workload | Sample | Byte-only change | Points-enabled change |
|---|---|---:|---:|
| Pack, default capacity | 9 large files | -0.25% | +0.05% |
| Pack, compact | 9 large files | -0.36% | -0.09% |
| Forward cursor walk, all attributes | 9 large files | +0.30% | +0.11% |
| Byte descendant seek | 9 large files | +0.33% | -0.04% |
| Forward cursor walk, all attributes | 200 typical files | +0.17% | +0.04% |
| Byte descendant seek | 200 typical files | +0.69% | -0.00% |
| Highlight queries, matches | 200 typical files | +0.60% | -0.35% |
| Highlight queries, captures | 200 typical files | +0.15% | -0.10% |
| Tag queries, matches | 200 typical files | +1.26% | -0.51% |
| Tag queries, captures | 200 typical files | +0.06% | -0.25% |

The byte-only typical-seek slowdown appears in all five paired aggregate runs
(+0.4% to +0.8%). However, unchanged mainline seek timings in those same processes
also moved +0.41%; sub-percent attribution to the layout alone is uncertain.
Other mainline walk/seek aggregate controls moved between -0.46% and +0.57%.
The tag-match byte-only paired aggregate changes range from -0.1% to +1.3%.
The table uses ratios of sums of per-input medians, so it need not equal the
median of those paired aggregate ratios.

## Comparison and method

Baseline: `7533e47d9c2d95398431edb325b4c03713c4f7a4`, before the pending reorder.
Candidate: the same code with the pending `slab.c` and `internal.h` changes:
base columns immediately precede values; fields follow the requested order;
resize copies and runtime offsets follow that order; format version is 6.
No other runtime source differs between the two snapshots.

Local Intel Core Ultra 7 165U, pinned to CPU 0, GCC 15.3.0,
`-O3 -g -fno-omit-frame-pointer`, 16-slot groups and eight-byte column alignment.
Separate otherwise-identical builds use `SQ_INCLUDE_POINTS=0` and `1`.
This is a local paired comparison, not a rerun on the Broadwell cloud VM used
in the earlier reports; absolute timings should not be compared across hosts.

Five alternating before/after process pairs per workload/input, 470 rows and
2,350 process pairs total. Workloads run serially. Packing uses one warmup and
seven timed conversions per process, reporting the median; parsing and output
deletion are outside the timed conversion. Walks, seeks, and queries use the
existing best-of-nine harnesses. Walks and seeks use default noncompact packing.
Seeks use deterministic zero-length byte ranges, 10,000/file for large inputs
and 5,000/file for typical inputs. Queries time complete 100-file language batches,
excluding parsing, packing, and query compilation.

Inputs are the existing nine 1.1–3.6 MB files in six grammars from
`build/conv-next/inputs.json`, plus the same 100 C and 100 Python files used in
the current overview, from `build/conv2/query-upload/corpus200`. Queries are
`highlights.scm` and `tags.scm`, draining matches and captures separately.

Every before/after pack pair has identical node/group/capacity/supertype counts
and slab sizes. Serialized hashes intentionally differ because layout and format
version changed. Every walk checksum matches mainline; zero seek-checksum
mismatches and zero query-count mismatches were reported. These are harness
checks, not exhaustive semantic proofs.

Not measured here: iterator-specific walks, point-range seeks, persistence loading,
small-file conversion, or end-to-end parse-plus-convert latency.

## Saved results and reproduction

[Results and input/source hashes](slab-order-results-2026-09-13.json) include
aggregate timings and all five process measurements per row, plus mainline
walk/seek/query control measurements. The full process output, including each
individual conversion timing, remains in `build/slab-order/results.json`.

The local build and runner scripts are `build/slab-order/build.py` and
`build/slab-order/run.py`; aggregation uses `summarize.py` and `archive.py` in that
directory. Harness sources are `build/conv-next/bench.c`,
`build/conv2/points-compare.c`, and `build/conv2/query-bench.c`. Both source and
binary snapshots remain under `build/slab-order/{before,after}`. The runner
checkpoints completed rows; use a fresh results path to perform a new full run.
