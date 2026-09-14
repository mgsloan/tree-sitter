# Grammar-wide supertype dictionary benchmark — 2026-09-14

The grammar cache is not a general conversion-speed improvement in this sample. It provides stable cross-tree IDs and faster large-tree membership reads, but introduces substantial cold initialization and higher retained memory for the two grammars that use dictionaries. Warm packing is mostly close to baseline and does not show a consistent win.

## Main findings

- C# (nine supertypes) derives 416 masks; Standard ML (twelve) derives 1,408. Both therefore use 16-bit indexes for every tree. The measured files previously needed only 4–10 masks and used 8-bit indexes.
- Cold analysis costs approximately 59 ms for C# and 6.6–6.9 ms for Standard ML. The cache releases its entry with the last tree/context, so sequential one-shot pack/delete or load/delete pays this repeatedly. Retaining a packing context or another packed tree avoids that analysis.
- Reused-context packing of tiny C#/SML trees is about 4.5% faster. Large-file conversion does not consistently improve; some paired rounds vary appreciably. Large-file membership walks are consistently about 33% faster.
- Dictionary sharing does not reduce retained memory in these inputs: the shared table is larger than the old per-tree dictionaries, and the wider column dominates large trees. Direct-mask grammars retain the same slab bytes but add eight private runtime bytes per tree.
- Tiny Python/C++ one-shot packing is about 12% slower. The implementation now scans symbol metadata once to count supertypes before allocation and again to populate the list; this is a plausible contributor, not a separately isolated measurement.

## Cold and warm latency for dictionary grammars

Times below are microseconds per operation, before → after. Cold operations have no live packed tree or context for that grammar. Warm one-shot packing/loading keeps another packed tree alive; reused-context packing keeps the context alive as well. Parsing is excluded.

| Input | Cold context create/delete | Cold pack/delete | Cold copied load/delete | Warm pack/delete | Reused-context pack/delete | Warm copied load/delete |
|---|---:|---:|---:|---:|---:|---:|
| csharp-tiny | 3.97 → 59,755.74 | 3.58 → 59,148.94 | 2.19 → 58,927.19 | 3.56 → 3.58 | 2.42 → 2.31 | 2.16 → 2.39 |
| csharp-large | 3.94 → 58,816.31 | 792.09 → 59,868.01 | 625.40 → 60,689.24 | 783.58 → 782.81 | 735.01 → 750.25 | 621.21 → 609.71 |
| sml-tiny | 1.68 → 6,910.71 | 2.47 → 6,941.16 | 1.54 → 6,934.85 | 2.49 → 2.60 | 1.92 → 1.84 | 1.57 → 1.66 |
| sml-large | 1.71 → 6,623.09 | 4,483.31 → 11,242.66 | 3,278.05 → 10,457.61 | 4,363.04 → 4,714.84 | 4,267.17 → 4,498.45 | 3,273.60 → 3,286.10 |

## All warm-path comparisons

Change in elapsed process CPU time; negative is faster. These are ratios of the median process medians, not confidence intervals. The JSON retains all samples and paired-round ratios. Small differences should be treated cautiously.

| Input | Nodes | Warm pack | Reused-context pack | Warm copied load | All-supertype membership walk |
|---|---:|---:|---:|---:|---:|
| json-tiny | 18 | -0.5% | -0.9% | +2.4% | +1.9% |
| json-large | 405 | -1.8% | -0.7% | -1.8% | +0.7% |
| python-tiny | 16 | +7.9% | +2.7% | +10.7% | +6.3% |
| python-large | 1,748 | -3.6% | -10.0% | -3.9% | +4.4% |
| cpp-tiny | 21 | +10.8% | +1.2% | +13.5% | -2.4% |
| cpp-large | 15,689 | +0.5% | +0.5% | -1.9% | +7.7% |
| tsx-tiny | 44 | +2.9% | -1.9% | +2.8% | -4.7% |
| tsx-large | 3,840 | +2.3% | -1.3% | +0.3% | +10.4% |
| csharp-tiny | 25 | +0.8% | -4.5% | +10.6% | +0.4% |
| csharp-large | 7,018 | -0.1% | +2.1% | -1.9% | -32.7% |
| sml-tiny | 19 | +4.3% | -4.5% | +6.0% | +1.8% |
| sml-large | 35,988 | +8.1% | +5.4% | +0.4% | -33.3% |

A membership walk traverses every node and calls `sq_node_has_supertype` for every grammar supertype. This isolates supertype-heavy reads; it is not a whole-query throughput benchmark. For the large files, paired current/baseline membership ratios were csharp-large: 0.626–0.694, sml-large: 0.660–0.700.

Large reused-context packing had wider paired ranges: csharp-large: 0.974–1.266, sml-large: 1.001–1.077. These results do not justify claiming a large-file packing speedup.

## Retained and peak memory

Requested bytes tracked through allocator wrappers; not RSS or shared-library size. One/16-tree measurements exclude contexts and parser allocations. All 16 trees pack the same input and remain alive together. The new cache is included once. Peak is conversion peak with a previously empty dictionary cache, excluding the mainline tree.

| Input | Slab bytes before → after | One tree before → after | 16 trees before → after | First-pack peak before → after |
|---|---:|---:|---:|---:|
| csharp-tiny | 736 → 728 | 1,920 → 9,400 | 30,720 → 38,200 | 7,582 → 10,919,095 |
| csharp-large | 158,360 → 167,656 | 159,544 → 176,328 | 2,552,704 → 2,709,048 | 174,268 → 10,919,095 |
| sml-tiny | 496 → 496 | 1,176 → 28,888 | 18,816 → 46,648 | 6,334 → 969,879 |
| sml-large | 711,608 → 759,528 | 712,288 → 787,920 | 11,396,608 → 12,191,160 | 734,512 → 969,879 |

The immutable shared dictionary plus hash table costs 7,480 requested bytes for C# and 27,704 bytes for Standard ML. Analysis peaks at about 10.4 MiB and 0.93 MiB respectively. The large default slabs grow csharp-large: 5.87%, sml-large: 6.73%.

Compaction does not remove the width penalty. For `repack=true`, large slabs change as follows:

| Input | Compact slab before | Compact slab after | Change |
|---|---:|---:|---:|
| csharp-large | 142,816 | 150,944 | +5.69% |
| sml-large | 585,336 | 623,608 | +6.54% |

Raw results also include context creation, context-with-tree, and trimmed-context-with-tree retained memory for every input.

## Method and provenance

- Baseline: commit `ab9143b2fa3aff3fbde2062eb85988be7c6c7042` plus [adaptive-width baseline patch](supertype-cache-baseline.patch), reproducing tree-local dictionaries with 16-bit widening. Current: frozen working-tree sources with grammar-wide cache, format version 8.
- Compiler: `gcc (GCC) 15.3.0`; `-O3 -g -fno-omit-frame-pointer`, no LTO. Same runtime sources, grammars, inputs, and flags for both variants.
- Host: Intel Core Ultra 7 165U, Linux x86-64. Each benchmark process is pinned to logical CPU 0. Process CPU time excludes descheduling but not frequency variation; the host was not reserved or frequency-locked.
- Three paired rounds, alternating baseline/current launch order. Each operation calibrates a batch to at least 15 ms (or 65,536 operations), then records five samples. Timing runs use ordinary allocations; memory runs use a separate wrapped executable.
- Points enabled, 16-slot groups, eight-byte alignment. Timings use default packing options; memory tests additionally measure compaction. No parse time, parallel throughput, or end-to-end query timing is included.
- JSON, Python, C++, and TSX contain 1, 4, 7, and 7 metadata supertypes respectively and retain the direct-mask path. C# and Standard ML contain 9 and 12.
- Node counts, physical group counts, and aggregate supertype membership agree between variants for every case. Each variant loads its own validated serialized slab before timing; the format versions intentionally differ.
- Full source/binary/input/grammar hashes, machine information, 72 timing records, and 48 memory records are in [raw results](supertype-cache-results-2026-09-14.json). Frozen builds and input manifest are under `build/supertype-cache-bench/`.

| Input | Source bytes | Source |
|---|---:|---|
| json-tiny | 20 | `/home/mgsloan/cozy/tree-squatter/pareto/build/supertype-cache-bench/inputs/json-tiny.json` |
| json-large | 20,638 | `/home/mgsloan/cozy/code-corpora/train/nodebb/node_modules/spdx-license-list/licenses/GFDL-1.2.json` |
| python-tiny | 27 | `/home/mgsloan/cozy/tree-squatter/pareto/build/supertype-cache-bench/inputs/python-tiny.py` |
| python-large | 8,557 | `/home/mgsloan/cozy/code-corpora/train/fastapi/tests/test_tutorial/test_path_operation_configurations/test_tutorial002.py` |
| cpp-tiny | 31 | `/home/mgsloan/cozy/tree-squatter/pareto/build/supertype-cache-bench/inputs/cpp-tiny.cpp` |
| cpp-large | 36,579 | `/home/mgsloan/cozy/code-corpora/test/entt/test/entt/entity/snapshot.cpp` |
| tsx-tiny | 52 | `/home/mgsloan/cozy/tree-squatter/pareto/build/supertype-cache-bench/inputs/tsx-tiny.tsx` |
| tsx-large | 14,185 | `/home/mgsloan/cozy/code-corpora/test/Chart.js/node_modules/.pnpm/jest-message-util@27.5.1/node_modules/jest-message-util/build/index.js` |
| csharp-tiny | 42 | `/home/mgsloan/cozy/tree-squatter/pareto/build/supertype-cache-bench/inputs/csharp-tiny.cs` |
| csharp-large | 77,973 | `/home/mgsloan/cozy/code-corpora/train/BenchmarkDotNet/src/BenchmarkDotNet/Helpers/CodeAnnotations.cs` |
| sml-tiny | 16 | `/home/mgsloan/cozy/tree-squatter/pareto/build/supertype-cache-bench/inputs/sml-tiny.sml` |
| sml-large | 98,023 | `/home/mgsloan/cozy/code-corpora/train/melsman--mlkit/src/Compiler/Lambda/LambdaExp.sml` |

The tiny files are generated snippets. The large files are real corpus files. The checked-in probe is [supertype-cache.c](supertype-cache.c), and the runner is [benchmark-supertype-cache.py](../../../tools/squatter/benchmark-supertype-cache.py). With the recorded input paths and grammar libraries available, rebuild and rerun using:

```sh
python3 tools/squatter/benchmark-supertype-cache.py \
  --prepare --output build/supertype-cache-bench \
  --inputs build/supertype-cache-bench/inputs.json --cpu 0 --rounds 3
```

## Implications

Retain a packing context when using the current implementation. Stable cross-tree IDs work, but this implementation should not be treated as an across-the-board performance optimization. The next useful changes to investigate are cheaper grammar analysis, a tighter mask bound that avoids widening these grammars unnecessarily, and removing the extra metadata-count scan for direct-mask grammars. A different cache lifetime can amortize cold analysis but retains its memory longer. None of those follow-up changes were applied during this benchmark.
