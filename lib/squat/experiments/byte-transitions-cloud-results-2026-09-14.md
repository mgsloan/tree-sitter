# Individual byte-width transitions on GCP — 2026-09-14

For real highlighting and tags query timings, see the [follow-up benchmark](highlight-tags-cloud-results-2026-09-14.md), which also reports each width transition separately.

The 8-bit and 16-bit thresholds are independent decisions. Each row below describes exactly one required-width → stored-width transition. These are new paired analyses of the existing GCP query and full-tree-walk measurements, not additional benchmark runs.

## How to read the results

Only grammars affected by that transition enter its average. For example, 5→8 covers Bash and JSON; 10→16 covers only C++. Different rows therefore cover different workload populations and should not be compared as if they were the same grammar mix. Negative times mean faster; memory is the increase in retained tree storage, not just the widened column. Each points configuration is shown separately, and the three query and three walk operations remain separate.

Every comparison holds all other actual symbol/field widths fixed. 2→8 compares `r2` against `r5`; 5→8 compares `r5` against `r6`; 6→8 compares `r6` against `r7`; 9→16 and 10→16 compare `r5_9` against `r5`, restricted to 9-bit and 10-bit grammars respectively. Thus, for example, 9→16 measures its marginal effect with lower-width rounding already enabled. These are not all comparisons against the completely compact layout.

Use the same accepted paired blocks as the [original cloud analysis](byte-rounding-cloud-results-2026-09-14.md): median of three within-block candidate/reference ratios per case, geometric averaging within a grammar and then across affected grammars; arithmetic averaging for memory. The analysis asserts that the only changed column width is the requested transition and that query/walk checksums agree. Timing-control variation still limits interpretation of small differences.

## Points enabled

| Transition | Type queries | Parent/child queries | Field queries | Cursor walk | Uncached iterator | Cached iterator | Tree memory |
|---|---:|---:|---:|---:|---:|---:|---:|
| 2→8 | -0.4% | +0.1% | +0.6% | -1.9% | -3.4% | -2.0% | +5.5% |
| 3→8 | — | — | — | — | — | — | — |
| 4→8 | — | — | — | — | — | — | — |
| 5→8 | -2.4% | -1.4% | +0.0% | -2.4% | -3.8% | -3.4% | +2.4% |
| 6→8 | +1.1% | +1.0% | -0.3% | -2.8% | -2.6% | -3.9% | +1.3% |
| 7→8 | — | — | — | — | — | — | — |
| 9→16 | -2.0% | +0.7% | -0.6% | -3.4% | -5.0% | -6.9% | +5.5% |
| 10→16 | -2.3% | -1.1% | -0.0% | -2.7% | -5.7% | -4.4% | +4.1% |
| 11→16 | — | — | — | — | — | — | — |
| 12→16 | — | — | — | — | — | — | — |
| 13→16 | — | — | — | — | — | — | — |
| 14→16 | — | — | — | — | — | — | — |
| 15→16 | — | — | — | — | — | — | — |

## Points disabled

| Transition | Type queries | Parent/child queries | Field queries | Cursor walk | Uncached iterator | Cached iterator | Tree memory |
|---|---:|---:|---:|---:|---:|---:|---:|
| 2→8 | +0.9% | +2.2% | +2.2% | -2.0% | -1.9% | -3.2% | +8.7% |
| 3→8 | — | — | — | — | — | — | — |
| 4→8 | — | — | — | — | — | — | — |
| 5→8 | -1.7% | -4.9% | -4.6% | -4.5% | -5.4% | -2.5% | +3.8% |
| 6→8 | +0.3% | -0.3% | -0.4% | -3.5% | -3.0% | -4.8% | +1.9% |
| 7→8 | — | — | — | — | — | — | — |
| 9→16 | -2.3% | -2.6% | -3.1% | -6.8% | -7.4% | -6.7% | +8.0% |
| 10→16 | -1.4% | -0.6% | -0.7% | -6.8% | -7.6% | -5.2% | +5.7% |
| 11→16 | — | — | — | — | — | — | — |
| 12→16 | — | — | — | — | — | — | — |
| 13→16 | — | — | — | — | — | — | — |
| 14→16 | — | — | — | — | — | — | — |
| 15→16 | — | — | — | — | — | — | — |

An em dash means no matching real grammar was benchmarked; it does not mean zero change. In particular, none of 3→8, 4→8, 7→8, or 11–15→16 has end-to-end GCP measurements here.

## Independent decisions and column costs

Column ratios below are the asymptotic non-straddling storage costs: `floor(64 / required_width) / floor(64 / stored_width)`. Finite columns can differ slightly because of final-word padding. These ratios are not whole-tree memory estimates.

| Transition | Column storage | Assessment |
|---|---:|---|
| 2→8 | 4.000× | Keep compact: modest walk gains for substantial tree growth. |
| 3→8 | 2.625× | No GCP evidence; keep compact provisionally. |
| 4→8 | 2.000× | No GCP evidence; keep compact provisionally. |
| 5→8 | 1.500× | Reasonable to widen when walks matter; measured separately from 16-bit transitions. |
| 6→8 | 1.250× | Reasonable to widen when walks matter. |
| 7→8 | 1.125× | Provisional widening based on lower column cost; no GCP evidence. |
| 9→16 | 1.750× | Optional walk-speed tradeoff; substantial memory cost. Keep compact by default. |
| 10→16 | 1.500× | Evaluate separately from 9→16; only C++ measured. Keep compact by default. |
| 11→16 | 1.250× | Unresolved empirically; keep compact provisionally. |
| 12→16 | 1.250× | Unresolved empirically; keep compact provisionally. |
| 13→16 | 1.000× | Widen without increasing column size; end-to-end speed unmeasured. |
| 14→16 | 1.000× | Widen without increasing column size; end-to-end speed unmeasured. |
| 15→16 | 1.000× | Widen without increasing column size; end-to-end speed unmeasured. |

The evidence supports an 8-bit threshold around 5 for workloads with frequent walks. The 16-bit threshold of 13 has a different justification: no added column space. The measurements do not establish the optimum among 10, 11, 12, and 13; evidence for 11 and 12 is missing. A shared cutoff or shared distance from the next byte boundary is not assumed.

## Reproduction

```sh
python tools/squatter/summarize-byte-transitions.py lib/squat/experiments/byte-rounding-cloud-results-2026-09-14.json --output lib/squat/experiments/byte-transitions-cloud-results-2026-09-14.json
```

The [derived data](byte-transitions-cloud-results-2026-09-14.json) includes affected grammars, per-case paired ratios, and configuration pairs. Hardware, source/input hashes, the excluded TSX correctness case, validation details, and raw timing samples remain in the original cloud artifact.
