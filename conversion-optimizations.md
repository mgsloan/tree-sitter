# Faster mainline-to-Squatter conversion

Start with frame allocation, repeated parent metadata work, and presence-index
construction. These offer concrete ways to remove work while preserving the
current representation. Then investigate group writes and allocation/copying.
The priorities below are hypotheses from reading the current working tree on
2026-09-10, not measured speedups. No new benchmarks were run for this document.

The scope is `sq_tree_pack`: converting an existing mainline `TSTree` into the
immutable Squatter representation. `sq_tree_parse` additionally parses and
destroys the mainline tree, so conversion improvements will have a smaller
effect on that complete operation.

## What conversion currently does

The implementation is in [pack.c](lib/squat/pack.c), with allocation and column
layout in [slab.c](lib/squat/slab.c) and auxiliary indexes in
[index.c](lib/squat/index.c).

1. Estimate group capacity from the root's visible descendant count, assuming
   75% occupancy, and allocate a zeroed slab plus runtime metadata. The
   descendant-count getter reads a cached count; this is not a counting walk.
2. Walk raw subtrees iteratively in reverse preorder, including hidden grammar
   wrappers. Each nonleaf frame allocates an array of child positions and fills
   it left-to-right before visiting children right-to-left. Every frame also
   allocates a supertype mask if the grammar has any supertypes, including leaves.
3. Resolve aliases, inherited fields, sibling flags, and incoming supertype masks.
   Emit visible nodes into a one-group `Pending` buffer. Fit checks track extrema
   and may close a group early when a delta would overflow.
4. Write group bases and packed columns. Capacity exhaustion allocates another
   slab and copies used column words; physical slot IDs remain unchanged.
5. Optionally compact, build symbol presence for trees with more than 32 groups,
   and append the supertype dictionary for grammars with more than eight
   supertypes. Presence construction currently walks all packed nodes twice.

Reverse preorder, one-group buffering, named fixed-width stores, and prefix-word
copying are already implemented. The historical
[v4 report](lib/squat/experiments/storage-v4-results-2026-09-10.md) measured
18–21% less compact-packing time than v3; the subsequent
[named-column report](lib/squat/experiments/named-columns-results-2026-09-10.md)
measured another 28–31% with points and 21–23% without points against its v4
baseline. Those are separate historical comparisons, not estimates of remaining
headroom in today's code.

## First experiments

| Priority | Change | Work removed | Main tradeoff |
|---|---|---|---|
| 1 | Reuse frame scratch; inline small supertype masks | Per-subtree allocation/free and mask copying | Scratch high-water memory |
| 2 | Compute child metadata once per parent | Repeated supertype scans and field-map setup | Larger frames or grammar tables |
| 3 | Count symbols during emission; fill index by groups | One full packed-node walk, then accessor overhead | Per-symbol scratch |
| 4 | Pack columns and flags in batches | Repeated word read/modify/write and lane arithmetic | Boundary-handling complexity |
| 5 | Fuse final compaction and auxiliary allocation | Repeated allocation and possible whole-slab movement | More involved finalization |
| 6 | Reuse language metadata and tune capacity | Repeated setup and avoidable growth | Explicit lifetime and memory policy |

### 1. Remove per-subtree scratch allocations

`init_frame` calls `calloc` for each mask and `malloc` for each nonempty child
position array; popping a frame frees both. Allocation count therefore scales
with raw nodes, not merely visible nodes or maximum depth.

Use builder-owned scratch with stack marks: reserve a frame's position slice on
entry and rewind on exit. Grow geometrically, retaining storage until conversion
finishes. A chunked arena provides stable addresses; a contiguous growable array
must store offsets in frames and reacquire pointers after growth. The same rule
applies to pointers into the growable `Frame` stack.

Give the one-child case an inline position, or derive its position directly from
the parent. A small inline array may cover most remaining frames, but measure
child-count distributions first: enlarging every frame can hurt deep trees.

Specialize masks by supertype count:

- Zero: retain the existing no-mask path.
- One through eight: carry the final byte mask by value. There is already no
  dictionary lookup in this case, but the current traversal still allocates a
  heap mask per frame.
- Nine through 64: carry one `uint64_t` by value and intern at emission.
- More than 64: use arena-backed word slices, preserving the current behavior.

This keeps temporary storage proportional to active traversal state rather than
staging an entire decoded tree. Record allocation counts and peak scratch bytes:
a single very wide node can still require a large position array.

### 2. Hoist parent metadata out of the child loop

For every child, the loop clears/copies `child_mask`, determines the parent's own
symbol, and scans the complete supertype list. The outgoing mask is identical
for every child of that frame: inherit the incoming mask only for a hidden
parent, then add the parent's own supertype bit. Compute it once for a nonleaf
frame and pass it by value or as an immutable slice.

A symbol-to-supertype-bit table can also replace the linear scan. Build it once
per conversion initially; an explicit reusable language context can amortize it
across files later. Include alias symbols, since the parent's alias takes
precedence when choosing its own symbol.

Cache the parent's child pointer, alias-sequence pointer, and field-map bounds.
Field resolution currently restarts a linear field-map scan for each non-extra
child. Options are a temporary structural-child-to-field array or a reusable
production lookup. Preserve the first matching non-inherited entry. Do not
assume entries are sorted by child index without checking the generator's
contract; a backwards child walk alone does not justify a backwards map cursor.

Specialize zero-field and no-alias cases. Keep these semantics intact: extras
do not consume structural child indexes and interrupt field inheritance; visible
nodes begin a new field relationship; hidden wrappers propagate incoming fields.
Use the documented [field policy](lib/squat/experiments/field-lookup-review.md)
as the reference, including alias-visible wrappers and ERROR behavior.

### 3. Build presence without two public-node walks

`sq_build_presence` first counts public display-symbol occurrences, then chooses
sparse slot lists versus group bitmaps and walks the tree again to populate
them. Count occurrences when a visible node is successfully accepted by `emit`
or when `close_group` commits it. Do not count a node again when a fit failure
causes an emission retry.

The count must use the same public-symbol mapping as `sq_node_symbol`, followed
by `sq_encode_symbol`. `Pending.symbol` stores an encoded raw display symbol;
using it directly is incorrect where public-symbol mapping merges symbols.

With counts already available, allocate the final index and fill it with one
descending physical-group scan. Decode only live symbol lanes, map them to
public IDs, and update the appropriate entry. Group iteration can avoid repeated
`SQNode` construction, waste checks, and accessor calls. Deduplicate symbols
within a group if bitmap updates are significant in profiles.

Preserve the existing threshold: a symbol uses a bitmap when its occurrence
count exceeds `ceil(group_count / 32)`, otherwise a sparse list. Sparse lists
must remain in descending slot order, with `SQ_NONE` padding. Directly appending
them during reverse-preorder emission would produce the opposite order.

A more ambitious builder could accumulate sparse occurrences and promote them
to bitmaps, but the threshold depends on final group count. Start with counting
plus one scan; it avoids tree-sized staging and keeps the serialized index
unchanged. The existing no-index path for at most 32 groups should remain cheap.

### 4. Batch group writes and specialize packed widths

`close_group` writes one node at a time across many columns. Each flag update is
a byte read/modify/write. Each variable-width symbol, grammar, and field update
calls `sq_set_packed`, which computes the lane and updates a 64-bit word.

Accumulate each flag column into a group mask and store its bytes together.
Try column-at-a-time loops over the existing `Pending` array, keeping the
destination pointer and base outside the loop. This may improve scalar code
before any SIMD is introduced. Compare against changing `Pending` to separate
arrays; transposition and larger scratch can erase a vectorization gain.

For variable-width IDs, select a writer once per tree, or keep running word/lane
state for each column. Construct complete words and write once where possible.
Packed words have `floor(64 / bits)` non-straddling lanes, so word boundaries
need not coincide with group boundaries. Preserve prior lanes in partial words,
advance over abandoned slots, and keep padding deterministic. Use offsets if
builder growth can invalidate destination pointers.

An exact eight-bit fast path can preserve the format. Rounding six- or seven-bit
columns up to eight bits changes the layout derived from the grammar and needs
an explicit format experiment; measure the additional bytes and query effects.
Retain native-endian lane handling, including the existing big-endian mapping.

Also try LTO across the packer and vendored runtime as a separate build
experiment. `emit` makes many tiny `ts_node_*` calls, and each child uses
`ts_node_new`. Alternatively, read one `Subtree` and position snapshot through
internal helpers. The packer already depends on internal runtime structures,
but direct reads must preserve aliases, inline subtree representation, and
`has_error = error_cost > 0`. Check generated code before maintaining duplicate
accessor logic.

### 5. Finalize the allocation once

With `repack=true`, finalization can perform a full column relocation in
`sq_resize`, then grow the colocated allocation for presence, then grow it again
for the dictionary. Ordinary `realloc` may extend in place, so these latter
operations are potential copies, not guaranteed copies. The experimental
64-byte-aligned allocator explicitly allocates and copies on growth.

Once the final group count and dictionary count are known, calculate the entire
final layout first. For compact construction, allocate one exact-sized result,
copy used column words directly to final offsets, fill presence there, and
append the dictionary. For noncompact construction, keep group capacity and
reserve both auxiliary sections with one growth operation. Avoid calling
`sq_resize` when requested capacity already equals current capacity.

Do not conflate this with public `sq_tree_repack`: that API currently copies and
validates an existing slab before resizing it. Optimizing that separate API does
not automatically speed up `sq_tree_pack(..., repack=true)`.

Only investigate selective zeroing after counters show it matters. Existing
bit writes assume initialized words, and unused lanes and padding are persisted.
Removing `calloc` indiscriminately can expose uninitialized bytes. A bulk writer
could explicitly initialize every output word and zero only unwritten regions;
remember that allocator-provided zero pages can make `calloc` cheaper than a
manual clearing pass suggests.

### 6. Reuse setup and improve capacity estimates

Default capacity is `visible_nodes / (3 * SQ_GROUP_SIZE / 4) + 1`. Measure actual
occupancy and growth frequency by grammar, file size, and valid/error-heavy
input. Try conservative per-grammar estimates or caller-provided
`initial_group_capacity`. Over-reservation costs zeroing, retained memory, and
possibly a later compaction; under-reservation costs full column copies.

An exact sizing pass repeats traversal and fit logic, so reserve it for cases
where measured copying dominates. A chunked group builder followed by one final
column assembly is another option, but adds a mandatory copy even when today's
estimate would have needed none. Both deserve separate large-file experiments.

For batches of small files, consider an explicit reusable pack context owning
grammar-derived metadata, position scratch, frame storage, and symbol counters.
`sq_allocate` currently rescans grammar symbols for supertypes on each tree.
Reuse can amortize that scan and the tables proposed above. Define language
ownership, concurrent use, reset behavior, and a way to release oversized
scratch. Avoid an implicit global cache keyed only by a language pointer.

For more-than-eight-supertype grammars, `intern_mask` linearly searches up to
256 masks and reallocates the dictionary for each new entry. A small hash table
plus geometric dictionary capacity can remove both costs. Keep first-seen IDs
stable for byte compatibility, and preserve the 256-entry error. Benchmark this
only where dictionary size and lookup comparisons justify the extra machinery.

## Larger or workload-dependent ideas

- **Leaf fast path:** emit a raw leaf without pushing and initializing a full
  frame. It still needs the incoming alias, field, mask, sibling state, and
  current physical boundary. Measure after scratch reuse to isolate the gain.
- **Byte-only reverse positions:** without points, compute child starts by
  subtracting byte sizes and padding from a correctly established end position.
  This could eliminate position arrays. With points, multiline extents lose the
  previous column, so the same subtraction is invalid. A checkpointed or
  single-line fast path would need a separate proof and benchmark.
- **Skip empty hidden structure:** a hidden subtree with no visible descendants
  may be skipped after its enclosing position and structural bookkeeping, if
  aliases cannot expose descendants. Visible-child counts are already consulted
  for sibling state; verify their alias semantics before using them to prune.
- **Fit-check tuning:** count group-close reasons and try ordering the common
  rejection tests first, or computing candidate extrema with fewer branches.
  Keep accepted extrema unchanged on rejection. Speculative full-group packing
  is harder: inserting waste changes physical ancestor spans, and an emitted
  node's span must be recomputed after a failed fit closes the previous group.
- **Optional work:** callers that do not benefit from symbol filtering can
  already set `symbol_presence=false`; callers accepting spare capacity can
  leave `repack=false`, the default. A byte-only build already removes point
  computation and storage, but also removes point APIs. Benchmark each choice
  against the actual downstream workload before changing defaults.
- **Batch concurrency:** independent files can be packed concurrently with
  separate builders. This targets throughput, with higher peak memory. Splitting
  one tree is substantially harder because group boundaries, ancestor spans,
  dictionary IDs, and indexes need reconciliation.
- **Parser integration:** direct packed construction could avoid the intermediate
  mainline allocation and destruction, but parsing relies on hidden structure,
  mutable/shared subtrees, and error recovery. Treat this as a separate parser
  design project after profiling the existing conversion path.

## How to choose what to implement

Use [layout.c](lib/squat/experiments/layout.c) as the starting isolated benchmark:
it parses once and times seven calls to `sq_tree_pack` with `repack=true`,
excluding parsing and deletion. Extend it to cover both repack settings and
both presence settings. Run point-enabled and byte-only builds separately.
The Rust benchmark's `cold-parse` includes a fresh parse, conversion, and
mainline-tree disposal, so it answers a different question.

Collect coarse phase times and counters in a diagnostic build: raw/visible
nodes, child-count distribution, mask copies and scans, dictionary comparisons,
allocation counts, scratch peak, fit failures by column, group occupancy,
slab growth, bytes copied, and presence time. Use sampling to attribute time
inside traversal; timing every node can distort the work being measured.

Compare one change at a time on the existing eleven-grammar bounded corpus and
large files, including mutations, deep nesting, wide child lists, long lines,
aliases, extras, empty/missing nodes, ERROR nodes, and supertype-heavy grammars.
Alternate baseline/candidate order, record compiler flags and input hashes, and
report per-file and per-grammar results alongside aggregate time. Repeatedly
packing one parsed tree measures warm input; add fresh-tree or rotating-tree
runs and complete parse-plus-pack measurements before claiming application gains.

For each candidate, report conversion time, complete construction time, retained
bytes, and peak construction memory. Extend the existing
[allocation probe](lib/squat/experiments/memory.c) for phase/copy counters as
needed; historical allocation totals do not substitute for a current baseline.
Recheck queries when changing presence policy, column layout, or group size.

For format-preserving changes, run the existing unit, differential traversal,
query, sanitizer, and persistence checks. Compare serialized bytes under fixed
options/capacity with [slab-compatibility.c](lib/squat/tests/slab-compatibility.c).
If a capacity policy intentionally changes the header/layout, require semantic
and round-trip equivalence instead of identical bytes. Exercise forced growth,
compaction, partial packed words, dictionary overflow, and allocation cleanup.

The first implementation sequence should be scratch reuse and small masks,
parent-mask hoisting, then emission-time symbol counts. Profile again before
committing to SIMD, a new temporary layout, or a format change.

## What has landed

Priorities 1 through 5 are implemented, along with the bulk column copy during
growth and compaction. Two later rounds removed whole-frame initialization, the
per-node header chase in `distance`, the lane divisions and store-forwarding
stall in staging, the repeated inline/heap tests in the `subtree.h` accessors,
and the aliasing-forced reloads in `close_group`. See the
[conversion report](lib/squat/experiments/conversion-results-2026-09-12.md) for
what each round measured and what remains in the profile.

Conversion is now instruction-bound rather than memory-bound: IPC is about 3.3
and LLC load misses are roughly 0.19 per visible node. Prefetching and SIMD are
therefore not the next step.

Priority 6's first half is done. `sq_allocate`'s per-tree supertype scan is still
proportional to the grammar, but it now reads `symbol_metadata` directly instead
of calling the accessor once per symbol, which is worth about 10% on batches of
small files and nothing on large ones. `load_bytes` got the same change, so
opening a cached slab benefits too. An explicit reusable pack context is now
implemented in C and Rust. It retains grammar tables and traversal scratch,
resets transient state on every call, requires exclusive access, and offers
trimming to release oversized scratch. The remaining part of priority 6 is
capacity tuning. See the
[follow-up report](lib/squat/experiments/conversion-results-2026-09-13.md).
