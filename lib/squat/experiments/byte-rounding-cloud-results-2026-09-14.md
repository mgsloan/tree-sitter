# Byte rounding on GCP: queries and complete tree walks — 2026-09-14

For real highlighting and tags query timings, see the [follow-up benchmark](highlight-tags-cloud-results-2026-09-14.md), which also reports each width transition separately.

This follow-up makes query execution and complete tree walks the primary evidence. Isolated reads, unpacking, and scans from the [earlier local experiment](byte-rounding-results-2026-09-14.md) are supporting evidence only. The production layout remains unchanged.

## Individual transitions

See the [per-transition analysis](byte-transitions-cloud-results-2026-09-14.md) for one row per width transition, with independent treatment of the 8-bit and 16-bit thresholds. The tables below are combined-policy comparisons; their totals must not be attributed to either transition alone.

## Recommendation

Keep **5–7 → 8 bits and 13–15 → 16 bits** as the balanced choice when complete walks matter. On GCP, the 5-bit cutoff reduces the three walk times by **1.8–4.0% with points** and **1.8–3.4% without points**, for **1.3% / 1.9% more retained storage**. The three query workloads change by less than 1% in either direction: there is no convincing query-speed improvement. The unchanged-layout control also moves by about 1%, so smaller differences should not decide the policy.

For a query-dominated workload, **only 13–15 → 16** is the more conservative choice. All those widths already store four values per 64-bit word, so this enables primitive 16-bit access without increasing column size. None of the sampled grammars requires 13–15 bits, however; this cloud run does not demonstrate an end-to-end speedup at those widths.

The 6-bit cutoff saves a little space (0.8% / 1.2% overhead) but generally gives up some walk improvement. Lowering the cutoff to 2 bits costs 3.3% / 5.2% while adding relatively little walk speed. Widening 9/10-bit symbols to 16 is a stronger walk-speed option—roughly 4.8–8.1% faster walks—but costs 4.7% / 6.9%; parent/child and field queries remain approximately flat. Keep 9–12 bits compact by default.

## Combined-policy results

Negative percentages mean less execution time. Query workloads and complete walks are reported separately; no composite score mixes them with microbenchmarks. Memory includes the packed tree and its language runtime.

### Points enabled

| Cutoffs | Type queries | Parent/child queries | Field queries | Cursor walk | Uncached iterator | Cached iterator | Memory |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 7→8, 13→16 (identical-layout control) | -0.9% | +0.4% | +0.3% | -1.1% | +0.1% | -0.0% | +0.0% |
| 6→8, 13→16 | +0.5% | +1.0% | +0.2% | -3.1% | -1.3% | -2.7% | +0.8% |
| 5→8, 13→16 | -0.8% | -0.0% | -0.3% | -4.0% | -1.8% | -3.3% | +1.3% |
| 2→8, 13→16 | -0.4% | +0.3% | -0.3% | -4.8% | -2.7% | -3.6% | +3.3% |
| 5→8, 9→16 | -2.3% | +0.5% | -0.5% | -6.0% | -4.8% | -6.6% | +4.7% |

### Points disabled

| Cutoffs | Type queries | Parent/child queries | Field queries | Cursor walk | Uncached iterator | Cached iterator | Memory |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 7→8, 13→16 (identical-layout control) | -0.1% | -0.2% | +1.1% | +0.4% | -0.1% | -0.1% | +0.0% |
| 6→8, 13→16 | -0.2% | +0.4% | +0.3% | -1.9% | -2.2% | -2.8% | +1.2% |
| 5→8, 13→16 | -0.5% | +0.9% | +0.8% | -1.8% | -3.2% | -3.4% | +1.9% |
| 2→8, 13→16 | -0.5% | +0.2% | +0.5% | -3.7% | -3.6% | -5.0% | +5.2% |
| 5→8, 9→16 | -1.7% | -0.2% | -0.7% | -6.1% | -8.1% | -7.6% | +6.9% |

## Workloads and validation

- GCP `squatter-benchmark`, `us-central1-a`, `e2-standard-2`; this boot reports AMD EPYC 7B12 (two vCPUs, SMT siblings), pinned to CPU 0. The idle wrapper holds the VM active across the entire sequential run. Guest platform and CPU details are in the raw artifact.
- Same frozen runtime revision `ab9143b2fa3aff3fbde2062eb85988be7c6c7042`, GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`, no LTO, group16/alignment8. The fixed local binaries and grammar libraries were uploaded and run using the guest ELF loader. Other sessions’ runtime changes are excluded.
- Nineteen cases from eleven grammars: 41 source files and 3,917,333 visible nodes, including eight files over 1 MB. Sources and grammar libraries were verified against their hashes before the run. The original 42-file corpus lost one large TSX case because its ordered field-query output differs from Tree-sitter even in the exact-width baseline; the entire case was excluded before cloud timing. This mismatch limits the query correctness coverage and is not a rounding regression.
- Type queries capture the three most frequent named node types. Parent/child queries select the twelve most frequent named relationships among the first 512 distinct relationships encountered. Field queries do the same with actual field constraints. Both parent and child are captured. Trees without qualifying fields reuse the structural suite; that fallback does not provide independent evidence about field filtering.
- Queries are generated deterministically from each Tree-sitter tree. Every ordered capture is compared with Tree-sitter in the first round for every binary/input/configuration: pattern, capture index, and byte range. Subsequent rounds omit this repeated validation, but retain timing checksum checks. Compilation and correctness checking are outside timing; execution and capture consumption are inside. These are predicate-free queries, not a complete editor highlighting workload or a measurement of host predicate evaluation.
- Cursor walks navigate first-child/next-sibling/parent from the root. Cached and uncached iterators also visit the entire tree. All three create and destroy their traversal object inside timing and consume every node’s symbol, grammar symbol, field, byte ranges, flags, and points when enabled. Their attribute checksums must equal the scalar reference. Prebuilt-handle reads are not counted as full walks.
- Sampled symbol/field widths are 2, 5, 6, 8, 9, and 10 bits. No real grammar in this sample exercises 7 or 11–15 bits.
- Six policies, points on/off, three randomized paired rounds, three calibrated CPU-time samples per operation per round. Within a case take the median paired ratio across rounds; average geometrically within grammar and then across eleven grammars. Memory uses corresponding arithmetic means.
- Retain the earlier control rule: reject the whole paired block when the identical-layout control differs by over 10% in at least two operations, or is over 25% faster / 33⅓% slower in any operation. Original rejected blocks and repairs remain in the raw artifact.

## Control audit and sensitivity

The initial 114 paired blocks produced seven rejections. Reruns produced one further rejection; the final summary has exactly three accepted blocks for each of 19 cases in each points configuration: **114 accepted / 122 total blocks**, or 732 binary/case measurements. All original data remain in the artifact.

A separate summary keeps **all 114 original blocks**, with no filtering or repair substitution. For the 5-bit policy, its walk estimates differ from the accepted summary by less than 0.5 percentage points; its query changes also remain within about 1%. The recommendation does not depend on removing the noisy blocks. The raw artifact includes this unfiltered sensitivity summary.

## Reproduction

The [raw artifact](byte-rounding-cloud-results-2026-09-14.json) contains input, source, and binary hashes, guest details, every timing, and per-case summaries. Build with the committed harness and the experiment patch, using the recorded corpus paths:

```sh
python tools/squatter/benchmark-byte-rounding.py --prepare --revision ab9143b2fa3aff3fbde2062eb85988be7c6c7042 --output build/rounding-replay --points 1,0 --variants exact,r7,r6,r5,r2,r5_9
```

Upload the binaries and inputs, rewrite manifest paths to their guest locations, and omit `tsx-large-7` as recorded in the cloud manifest. In the uploaded directory:

```sh
squatter-idle run -- python3 tools/squatter/benchmark-byte-rounding.py --output . --end-to-end --validate-once --loader /lib64/ld-linux-x86-64.so.2 --cpu 0 --points 1,0 --variants exact,r7,r6,r5,r2,r5_9 --rounds 3 --repeats 3 --tag cloud-primary
python3 tools/squatter/summarize-byte-rounding.py cloud-primary.json cloud-repair*.json --output summary.json --audit-output audit.json
```

Omit the repair glob if no repair runs exist. Repair runs use the same driver with the affected `--cases` and `--points`, all six variants, and a fresh tag.

To reproduce the sensitivity check, summarize only `cloud-primary.json` with `--keep-all`.
