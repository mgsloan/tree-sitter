# Column pointers and a smaller persistent header

The proposed split makes sense: a runtime descriptor contains reconstructed addresses and grammar metadata, followed by—or separately referencing—a relocatable persisted payload. Cached pointers show a modest benefit in an addressing probe. The more immediately attractive changes are simplifying persisted metadata and caching the active-group index bias. A production pointer-table change still needs whole-walk/query measurements.

## What is already reconstructed

`SQTree.layout` already caches all per-column offsets outside the persisted slab. `sq_layout(language, capacity)` computes them. They are not serialized individually. A normal read uses the slab base, a cached column offset, and an index into the column.

The current 32-byte persisted `SQHeader` includes two region offsets and two auxiliary-section offsets. Those can be derived. This is independent of whether the runtime descriptor uses pointers or offsets.

## A practical 16-byte header

With the exact grammar supplied externally, a new format could use:

```c
struct PersistedHeader {
  uint32_t format_and_flags;
  uint32_t group_count;
  uint32_t group_capacity;
  uint32_t supertype_dictionary_count;
};
```

This is a proposal, not a format change made by this experiment. Version/features identify the group size, column alignment, point support, and presence of the optional symbol index. The existing native-endian representation also needs an explicit portability policy if cross-endian persistence is wanted.

- `group_count` is the number of live groups, including partially occupied groups.
- `group_capacity` is the **actual allocated capacity**, not the initial estimate. It can grow while packing. Columns occupy active suffixes, so their indexes and sizes depend on this value.
- Grammar plus version/features determines column widths, symbol/field domains, supertype count, and the meaning of IDs. Merely matching symbol counts is insufficient; callers must supply the exact grammar, as they do today.
- Column offsets follow from capacity and the canonical alignment rules. The symbol index, when enabled and emitted, begins after those columns. Its length follows from grammar and live group count. The dictionary follows the index.
- Dictionary entry count depends on combinations encountered in this tree, not just the grammar. Keeping it explicit costs no extra alignment space in this 16-byte design. A format supplied with the total blob length could instead infer it from the remaining tail bytes.

That removes `groups_byte_offset`, `nodes_byte_offset`, `symbol_presence_byte_offset`, and `supertype_dictionary_byte_offset`. Validation still computes section lengths with checked arithmetic and verifies that the supplied blob length matches exactly. Presence needs an explicit flag because the index can be disabled even on a large tree.

A format that **always persists compact trees** can omit capacity because it equals count. If dictionary count is inferred from blob length, an 8-byte header is plausible. Supporting existing noncompact persistence makes 16 bytes a useful conservative choice. A nominal 12-byte header would still need padding before 8-byte-aligned columns. With the experimental 64-byte column alignment, reducing the header alone does not reduce the aligned column start.

## Runtime pointers and allocation layout

On x86-64 the pointer table has a modest but real cost:

| Build | Columns | Current u32 offset arrays | Pointer arrays | Increase if replacing arrays |
|---|---:|---:|---:|---:|
| Points | 23 | 92 B | 184 B | 92 B |
| Byte-only | 15 | 60 B | 120 B | 60 B |

Whole-object padding can change the final increase. Keeping both representations would instead add the full pointer-table size. The current runtime tree object is 136 B with points and 104 B without. A table larger by roughly 60–100 B is immaterial for a large tree, but relevant to the tiny-tree overhead seen in the memory benchmark. Saving 16 persisted bytes does not by itself offset that runtime increase.

A single allocation containing `[runtime prefix][persisted header][columns][auxiliary sections]` could remove an allocation and improve locality. Only the suffix would be returned for serialization. This allocation arrangement is independent of using pointers: an offset descriptor could live in that prefix too. A mapped payload can instead have a separately allocated descriptor; pointer caching does not require a writable prefix in the file or remapping tricks.

Coallocation has a lifecycle cost. Growing or compacting the payload can move the prefix, and public `SQNode` handles contain a pointer to `SQTree`. Finalizing before exposing handles, or keeping the descriptor stable while the builder grows its payload, avoids invalidating them. Copying a finished slab into a final combined allocation introduces a construction copy/peak unless the builder is redesigned.

Pointer caches must be reconstructed on load/copy and refreshed after allocation, resize, symbol-index append, and dictionary append. Cached index bias must also change when the live group count changes during packing. An immutable finalized descriptor makes these rules simpler.

One packed-field trap: pointers cannot generally point directly to the first active logical lane with no residual index. Nine-bit IDs have seven lanes per u64, so moving the live suffix by 16 slots need not land on a word boundary. The waste column can begin within a byte as well. Use pointers to backing-column starts plus an index bias, or track a residual lane per packed column.

## Addressing experiment

`column-addressing.c` (available at commit `98967f593`) compares four read-only views of the same actual packed trees. The common view embeds a copy of `SQTree`, so the offset baseline has the same direct descriptor access as production; it does not pay an extra tree-pointer indirection. The pointer arrays are appended to that descriptor. The four variants are:

1. Existing column offsets and header-derived index bias.
2. Column pointers, retaining the same header-derived bias.
3. Offsets with cached group/slot bias.
4. Pointers with cached group/slot bias.

There are three workloads: individual out-of-line start-byte getters; a loop of inlined start-byte reads; and reads of every raw node/group column. Each runs in preorder and a deterministic shuffled order. All decoded columns are individually checked against the current implementation before timing; workload checksums agree too.

The sample is the largest bounded file for each of 11 grammars plus the nine saved files ≥1 MiB. Both point modes passed on all 20 original files. This is a local exploratory microbenchmark on an Intel Core Ultra 7 165U, pinned to CPU 2, GCC 15.3.0 `-O3 -g`. Each case has 12 repetitions with balanced rotation of the four variants, using equal batch counts calibrated to at least 3 ms for the baseline. It does not measure cache setup, deserialization, complete traversal attributes, queries, or many concurrently hot tree descriptors.

Preorder ratios below are geometric means of per-file median time ratios. **Below 1.0 is faster than offsets.** Shuffled results and every timing sample are in the [data](column-addressing-results-2026-09-10.json).

| Build | Sample | Workload | Pointers | Offsets + cached bias | Pointers + cached bias |
|---|---|---|---:|---:|---:|
| Points | bounded | byte_getter | 0.994 | 0.911 | 0.987 |
| Points | bounded | byte_bulk | 0.912 | 1.001 | 0.914 |
| Points | bounded | all_columns | 0.947 | 1.040 | 0.975 |
| Points | large | byte_getter | 0.993 | 0.912 | 0.990 |
| Points | large | byte_bulk | 0.910 | 1.001 | 0.916 |
| Points | large | all_columns | 0.945 | 1.012 | 0.957 |
| Byte-only | bounded | byte_getter | 0.996 | 0.910 | 0.983 |
| Byte-only | bounded | byte_bulk | 0.916 | 1.001 | 0.913 |
| Byte-only | bounded | all_columns | 0.947 | 1.000 | 0.967 |
| Byte-only | large | byte_getter | 0.992 | 0.913 | 0.984 |
| Byte-only | large | byte_bulk | 0.911 | 1.000 | 0.914 |
| Byte-only | large | all_columns | 0.978 | 0.995 | 1.039 |

Pointer-only individual getters are essentially tied with offsets (less than 1% difference). Pointers improve sequential bulk byte reads by about 8–9% and the broader all-column kernel by 2–6%. Gains are not additive: the pointer-plus-bias combination regresses 3.9% in byte-only large-file all-column reads. Treat these as evidence for a prototype, not whole-engine speedup estimates or reliable rankings of small differences across machines.

Disassembly supports a concrete mechanism. Cached column pointers eliminate address-building instructions in the getter. GCC hoists the node-byte column address in the offset bulk loop, but leaves a separate slab-base-plus-scaled-index `lea` for the group column inside the loop. The pointer loop uses a direct base-plus-scaled-index load. Thus the compiler does not fully erase the difference in this build. A bulk API receiving already-resolved local column pointers could obtain that benefit without keeping a table in every tree; existing SIMD unpack functions already receive column pointers.

Caching the bias removes the dependency chain through `tree->data` and the header's two counters. It improves ordinary getter probes even with offsets. A single cached group bias may fit the current tree object's four bytes of tail padding; this experiment caches both group and slot biases, so that smaller production design still needs measurement. The offset-plus-bias getter improvement is consistently about 9% across these four sample/build combinations, but complete API workloads still need measurement. That variant also regresses 4% in bounded point-enabled all-column reads.

## Recommendation

Pursue these as independent decisions:

- A versioned 16-byte persisted header is straightforward and removes redundant state. It is a small per-tree storage saving; preserving or changing file compatibility is a separate decision.
- First prototype a cached active-group bias in the existing runtime descriptor. It addresses a measurable dependency with little or potentially no object-size increase.
- Keep a pointer-based descriptor as a benchmark candidate, particularly for ordinary random node access. Compare it against offsets **with the same cached bias**, and include complete cached/uncached iterator and query workloads plus many small trees. The existing offset approach is reasonable for SIMD consumers that resolve a column once per batch.
- Decide whether to coallocate the runtime prefix after measuring its allocation/lifetime tradeoff. It is not necessary to get the pointer lookup benefit.

Production addressing and the persisted format remain at the measured baseline in this consideration; the committed changes are the experiment and findings.

## Reproduction

From the repository root, with the saved corpus bundles present:

```sh
make -C lib/squat BUILD=../../build/squat-memory/points \
  CFLAGS="-O3 -g -DSQ_INCLUDE_POINTS=1" \
  ../../build/squat-memory/points/column-addressing
make -C lib/squat BUILD=../../build/squat-memory/bytes \
  CFLAGS="-O3 -g -DSQ_INCLUDE_POINTS=0" \
  ../../build/squat-memory/bytes/column-addressing
python3 lib/squat/experiments/column-addressing.py
```

The driver was retired when named fields replaced the column tables. Run these
commands from a checkout of `98967f593` to reproduce the historical experiment.

Raw output is under `build/squat-addressing/`. The committed JSON preserves all timings grouped by case, medians, source/binary hashes, commands, and runtime/table sizes. Baseline implementation: `96436c793`; experiment: `d4a97f1d9`. `objdump -d -M intel build/squat-memory/points/column-addressing` exposes the named getter and bulk kernels.
