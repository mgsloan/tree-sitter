# Byte-rounded symbol and field storage — 2026-09-14

The [GCP query and full-walk follow-up](byte-rounding-cloud-results-2026-09-14.md) is now the primary evidence for the recommendation. These earlier local measurements remain supporting evidence.

Recommend **5–7 → 8 bits** and **13–15 → 16 bits** as the balanced default. Keep 2–4 and 9–12 bits packed at their minimum widths. Existing 8- and 16-bit widths stay unchanged. The production runtime is unchanged; the implementation is supplied as an isolated [experiment patch](byte-rounding.patch).

The 5-bit cutoff improves cached attribute traversal by 3.6% with points and 4.2% without points, and random ID reads by approximately 10–11%, for 1.3%/1.9% more retained tree memory. Packing and the tested queries are approximately unchanged. These are elapsed-time reductions, based on grammar-balanced paired measurements.

For 13–15 → 16, all representations hold four values per 64-bit word, so there is **no column-size increase**. Standalone random reads take about 36–37% less time and unpacking about 63% less time. Equality scans are approximately unchanged (one 13-bit comparison was 5% slower).

## Whole-tree tradeoffs

Time changes below zero are improvements. Within each workload, take the median of three paired candidate/baseline ratios; average time ratios geometrically within a grammar, then across eleven grammars. Memory is the corresponding arithmetic mean of per-workload retained-byte ratios, balanced by grammar. This prevents the extra large JSON/TypeScript inputs from dominating.

### With points

| Cutoffs | Retained memory | Scalar attributes | Cached traversal | Random IDs | Group scans | Packing | Queries |
|---|---:|---:|---:|---:|---:|---:|---:|
| 7→8; 13→16 (no-change control here) | +0.0% | -0.2% | -0.2% | -0.3% | -0.4% | -0.3% | -0.1% |
| 6→8; 13→16 | +0.8% | -2.5% | -2.5% | -7.3% | -5.6% | +0.2% | -0.4% |
| **5→8; 13→16** | +1.3% | -3.7% | -3.6% | -10.8% | -6.9% | +0.2% | -0.3% |
| 2→8; 13→16 | +3.3% | -3.9% | -3.6% | -15.3% | -11.1% | -0.3% | -0.4% |
| 6→8; 9→16 | +4.2% | -7.2% | -5.7% | -21.2% | -5.2% | -2.1% | -1.1% |
| 5→8; 9→16 | +4.7% | -8.0% | -6.7% | -24.5% | -6.7% | -1.8% | -1.3% |

### Byte-only

| Cutoffs | Retained memory | Scalar attributes | Cached traversal | Random IDs | Group scans | Packing | Queries |
|---|---:|---:|---:|---:|---:|---:|---:|
| 7→8; 13→16 (no-change control here) | +0.0% | -0.1% | -0.1% | +0.2% | -0.7% | +0.1% | +0.0% |
| 6→8; 13→16 | +1.2% | -1.8% | -3.8% | -9.3% | -6.3% | -0.1% | -0.1% |
| **5→8; 13→16** | +1.9% | -2.6% | -4.2% | -9.9% | -5.7% | -0.0% | -0.5% |
| 2→8; 13→16 | +5.2% | -3.7% | -5.0% | -15.4% | -10.8% | +0.1% | -0.6% |
| 6→8; 9→16 | +6.2% | -7.3% | -7.2% | -22.6% | -5.3% | -1.8% | -1.5% |
| 5→8; 9→16 | +6.9% | -8.1% | -7.9% | -26.1% | -6.0% | -1.8% | -1.5% |

The 5-bit policy’s largest retained-size increases were 2.7% with points and 4.5% without points. Lowering the cutoff to 2 bits buys relatively little additional traversal speed for much more space.

Widening 9/10-bit symbols to 16 is a viable read-heavy option: the combined 5→8 / 9→16 policy improves cached traversal by approximately 7–8% and random reads by 25–26%, with 4.7% more retained memory with points and 6.9% without them. It offers less speed improvement per added byte than the recommended policy. There is no unique optimal cutoff independent of workload and memory budget.

## Why these boundaries

Non-straddling words have discrete capacity steps. The following are measured single-column results on 262,144 uniformly populated IDs. Size ratios include packed-word padding. Time ratios are rounded/exact; lower is faster.

| Required → stored bits | Column bytes | Random reads | Bulk unpack | Group equality scans |
|---|---:|---:|---:|---:|
| 2 → 8 | 4.00× | 0.61× | 0.66× | 0.72× |
| 3 → 8 | 2.62× | 0.61× | 0.62× | 0.73× |
| 4 → 8 | 2.00× | 0.65× | 0.62× | 1.31× |
| 5 → 8 | 1.50× | 0.58× | 0.60× | 0.91× |
| 6 → 8 | 1.25× | 0.62× | 0.50× | 0.92× |
| 7 → 8 | 1.12× | 0.61× | 0.50× | 0.80× |
| 8 → 8 | 1.00× | 1.00× | 0.98× | 1.00× |
| 9 → 16 | 1.75× | 0.64× | 0.43× | 1.15× |
| 10 → 16 | 1.50× | 0.64× | 0.42× | 1.06× |
| 11 → 16 | 1.25× | 0.65× | 0.40× | 1.01× |
| 12 → 16 | 1.25× | 0.64× | 0.41× | 0.95× |
| 13 → 16 | 1.00× | 0.63× | 0.37× | 1.05× |
| 14 → 16 | 1.00× | 0.63× | 0.37× | 1.00× |
| 15 → 16 | 1.00× | 0.64× | 0.37× | 1.00× |

4→8 doubles the column and slows this equality-scan probe by 31%. At 5→8, both reads and scans improve, while the whole-tree size cost remains modest. At 13→16, the storage penalty disappears entirely. The sequential probe in the raw data shows much larger gains because primitive loads allow compiler vectorization; those numbers should not be interpreted as whole-tree API speedups.

## Scope and validation

- Frozen committed baseline: `ab9143b2fa3aff3fbde2062eb85988be7c6c7042`. Uncommitted work from other sessions was excluded.
- Intel Core Ultra 7 165U, pinned to P-core CPU 2; GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`, no LTO. Group size 16, alignment 8, default packing and symbol-presence indexing. Both point modes were measured.
- 42 files, 17,630,275 source bytes, 4,756,709 visible nodes: three smaller files per grammar and nine large inputs. Grammars: Bash, C, C++, CSS, Go, HTML, JSON, Python, TSX, TypeScript, YAML. Paths, grammar-library hashes, input hashes, and frozen source hashes are in the raw artifact.
- The real grammars exercise symbol/field widths 2, 5, 6, 8, 9, and 10. Evidence for 7 and 11–15 bits comes from the column probes. The 13-bit upper cutoff is the conservative zero-space-cost choice; this sample does not establish a whole-tree optimum for 11/12-bit grammars.
- The exploratory sweep tested ten policies. Confirmation tested seven policies over three randomized-order rounds in each point mode, with three calibrated process-CPU samples per operation per invocation. Calibration targets at least 8 ms per sample. Microbenchmarks use seven samples and a 5 ms target.
- The 7→8 / 13→16 policy is an identical-layout control for every real grammar in this sample. Four disturbed blocks were rejected and rerun in full. The control rule rejects a whole block if control/baseline is outside 0.9–1.1 for at least two modes, or outside 0.75–4/3 for any mode. The final dataset contains exactly three accepted blocks per workload and point mode: 120 accepted of 124 collected blocks. All collected confirmation measurements, including excluded blocks, remain in the artifact.
- Every build passed unit checks. Each benchmark validates repacking/loading, cached/scalar attribute checksums, and every ordered query capture against Tree-sitter. Cross-policy checksums agree. The recommended policy also passed full comparison suites, including edge cases and mutations, for all eleven grammars in both point modes; existing documented inherited-field exceptions remain accounted for.
- Retained bytes are requested storage for the packed tree and runtime prefix, excluding parser/input storage and allocator bookkeeping. Compact slab sizes are also recorded. Timings cover one-shot packing plus deletion; scalar attributes over node handles; cached forward iteration; random symbol/field/grammar-ID reads; symbol and field group equality scans; and captures for the three most frequent named symbol types. Queries are predicate-free symbol queries, so these results do not characterize every structural or field-constrained query.
- Rounding applies to display symbols, sparse grammar overrides, and field IDs. Coordinate, flag, waste, and supertype representations remain fixed. Production adoption requires a new format version. Both layout construction and query-filter compilation must use `sq_storage_width`; precomputing query masks from the minimum width produces incorrect results. The experiment assigns distinct slab magic values to the cutoff combinations.
- This is a local convenience sample on one CPU. The small packing/query differences are near the control’s variation and should be treated as approximately flat.

## Reproduction

The driver uses the existing corpus extracts and grammar libraries in this checkout. Use a fresh output directory and the pinned revision:

```sh
python tools/squatter/benchmark-byte-rounding.py --prepare --output build/byte-rounding-repro \
  --revision ab9143b2fa3aff3fbde2062eb85988be7c6c7042 --points 1,0 \
  --variants exact,r7,r6,r5,r4,r2,r10,r9,r6_9,r6_10,r5_9
python tools/squatter/benchmark-byte-rounding.py --output build/byte-rounding-repro \
  --micro --repeats 7 --tag micro
python tools/squatter/benchmark-byte-rounding.py --output build/byte-rounding-repro \
  --points 1,0 --variants exact,r7,r6,r5,r2,r6_9,r5_9 --rounds 3 --repeats 3 --tag confirmation
python tools/squatter/summarize-byte-rounding.py build/byte-rounding-repro/confirmation.json \
  --output build/byte-rounding-repro/summary.json --audit-output build/byte-rounding-repro/audit.json
```

Rerun any rejected blocks with `--cases`, `--points`, and a fresh `--tag`, then pass all confirmation/repair files to the summarizer. The accepted count should be at least three for every workload and point mode.

Artifacts: [raw data and audit](byte-rounding-results-2026-09-14.json), [C harness](byte-rounding.c), [experiment patch](byte-rounding.patch), [driver](../../../tools/squatter/benchmark-byte-rounding.py), [summarizer](../../../tools/squatter/summarize-byte-rounding.py).
