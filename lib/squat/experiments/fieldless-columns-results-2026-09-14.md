# Omit field storage for grammars without fields — 2026-09-14

Implemented in `65e50c348`. Grammars with `field_count == 0` have a zero-width,
zero-byte field column. Scalar reads return zero, the iterator skips decoding,
and group equality returns the occupied lanes for field zero and no lanes for
other field IDs. Packing skips field writes. No extra runtime metadata is needed.
The serialized format is v10; older-format slabs are rejected.

The user requested no further benchmarking of this clear storage improvement.
The following results were already collected before that instruction. No additional
production benchmark was run.

## Previously completed GCP measurements

Same GCP instance and harness as the [column experiments](column-probes-results-2026-09-14.md):
`mgsloan-compute/us-central1-a/squatter-benchmark`, e2-standard-2, Intel Xeon
2.20 GHz (family 6/model 79), pinned CPU 0. Runs were serial, with three randomized
paired rounds and five samples per variant. Query timings include complete
highlight execution and result consumption on prepared trees. Walks visit the
whole tree. Negative time percentages mean faster.

Each grammar uses three small source files. These are field-only omission results;
all percentages compare with the exact-width baseline.

| Layout | Grammar | Highlight time | Cursor walk | Uncached walk | Cached walk | Slab bytes |
|---|---|---:|---:|---:|---:|---:|
| Points | CSS | +0.30% | −8.7% | −7.1% | −2.1% | −1.83% |
| Points | HTML | −0.45% | −9.6% | −1.8% | −4.2% | −1.98% |
| Byte-only | CSS | −0.29% | −8.2% | −11.1% | −3.4% | −2.86% |
| Byte-only | HTML | +0.59% | −10.2% | −6.4% | −4.1% | −3.28% |

Highlighting is essentially flat, while walks and storage improve. Go and TSX,
which have fields, were included as unaffected controls: their highlighting/tags
changes range from −1.24% to +0.77%.

A separate combined probe omitted both the unused field and supertype columns.
It saved 9.06–9.87% of slab bytes with points and 14.19–16.35% without points;
CSS/HTML highlighting changed by −1.34% to +1.68%. Supertype omission remains
experimental. The probe framework adds eight common bytes per tree to every
variant, including its baseline; slab percentages exclude those bytes. The
production field-only change adds no such overhead.

The query run contains 132 records and 36 paired blocks, with no control rejection.
The walk run contains 96 records and 24 blocks, with one block rejected by the
predeclared identical-code control range of 0.85–1.15. Summaries use sample medians
and paired ratios; raw and unfiltered summaries are retained in the
[audit artifact](fieldless-columns-results-2026-09-14.json).

## Correctness

Production unit and supertype/persistence suites pass with `SQ_INCLUDE_POINTS=1`
and `0`, built using the committed Makefile. The new unit regression poisons the
neighboring column and checks zero field reads, zero/nonzero field equality,
invalid groups, and every possible group occupancy.

Production comparisons against Tree-sitter pass for three source files plus edge
cases each in CSS, HTML, Go, and TSX, with and without points. CSS has 954 existing
seek discrepancies in the points-enabled comparison; the exact baseline reports
the same count. This is not a strict zero-seek-mismatch claim. The recorded output
preserves this limitation. Earlier isolated omission probes also passed ordered
real-query validation and ASan/UBSan comparisons.

The audit artifact includes finished runs, verified binary/manifest hashes,
experimental patches, job inputs, filtered and unfiltered summaries, production
comparison output, and production check logs. The production change was not
benchmarked again after the user's instruction.
