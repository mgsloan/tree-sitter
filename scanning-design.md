# Rust scanning API

The group-based Rust scan API. Read attributes from the returned node handles.

The scan reads stored columns directly and retains a compact mask of matching
slots for each group. There is no unpack cache: neither traversal, filters, nor
projection retain decoded column arrays. A kernel may decode values into
registers while evaluating a group.

## Public API

```rust
let scan = node.preorder()
    .overlapping_bytes(from..to)
    .filter_kind_ids([identifier, call_expression])
    .filter_field_ids([name_field, value_field]);

for node in scan.nodes() {
    // ordinary Node access
}
```

Choose a traversal before applying range restrictions or filters:

- `preorder()` visits parents before children, with siblings left to right.
- `postorder()` visits children left to right before their parent.
- `all()` selects the more efficient traversal for the representation. Its order
  is unspecified; the current reverse-preorder storage selects preorder.

All three include the root. `all()` does not promise a stable choice across
representations or releases. Use an explicit order when order matters.

Reverse either explicit order with `.rev()`, for example
`node.postorder().rev().filter_kind_ids(&kinds).nodes()`.

The scan is a typed pipeline with three terminal operations:

- `nodes()` returns an ordinary iterator of `Node` handles.
- `count()` counts matching slots without constructing nodes.
- `groups()` returns an iterator of group references and matching masks.

Scans remain inside the root's subtree, preserve the selected order, and yield
each matching node once. IDs belong to the tree's language. Public kind IDs are
the default; grammar IDs, if supported, use a separate method.

Kind and field sets use OR; successive filters use AND. An empty set matches nothing.
Field matching tests the child's field relative to its parent, without implying
a parent-kind constraint. Field ID zero matches nodes without a field. Supertypes
test actual node membership rather than a global expansion into concrete kinds.

Both set filters accept `[u16; N]`, `&[u16; N]`, or `&IdSet`. Arrays preserve `N`
through the typed pipeline, specializing kernels while IDs remain runtime values.
The array is copied into the predicate; duplicates yield no duplicate nodes.
`N` counts supplied entries, including duplicates and invalid IDs. `IdSet` is the
reusable dynamic alternative; `KindSet` remains an alias for compatibility.
`Node::descendants_matching_kinds` and the shared `NodeLike` method accept the
same selections. The singular field/supertype filters remain available.

Ordinary mapping follows `nodes()`. Specialized consumers can process groups
directly. There is no `map_cached` operation or type-level column-cache machinery.

## Typed composition

`overlapping_bytes` changes traversal rather than appending a predicate:

```rust
Scan<'tree, Preorder<'tree>>
Scan<'tree, Postorder<'tree>>
Scan<'tree, PreorderOverlappingBytes<'tree>>
Filtered<Source, Predicate>
```

Each type stores only its own arguments and inner source. The initial API
requires range restriction before filters. Bounds cannot change after scanning
has started.

The internal group protocol is schematically:

```rust
trait GroupScan<'tree> {
    type Reversed: GroupScan<'tree, Reversed = Self>;
    type Slots: Iterator<Item = u32> + ExactSizeIterator;
    const DESCENDING: bool;
    fn slots(matches: Mask) -> Self::Slots;
    fn reverse(self) -> Self::Reversed;
    fn group(&self) -> &GroupRef<'tree>;
    fn next_mask(&mut self) -> Option<Mask>;
    fn next_slots(&mut self) -> Option<Self::Slots>;
}

trait Predicate {
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask;
}
```

A source retains its column metadata and current group index. Advancing returns
only a nonempty mask. Filters borrow the current group to refine that mask and
continue when it becomes empty. This avoids copying column metadata through
adapters, including postorder's singleton fragments. `groups()` copies metadata
into public results so they can outlive the iterator.

Unfiltered preorder node consumers request slot ranges directly. Filters and
group consumers still use masks; `next_slots()` converts those masks to sparse
slot iterators with a static extraction direction.

Predicates must return a subset of their input mask. Built-in predicates are
pure; custom callbacks with observable side effects are outside the interface.

Generic composition permits inlining and specialization without dynamic dispatch
or allocations for adapters. It does not guarantee SIMD. The hot column reads
and predicate kernels must be visible to the optimizer; per-node opaque C calls
would limit optimization.

## Masks and group identity

Use a `u64` bitmap behind `Mask` for all supported 16/32/64-slot groups. The
width is chosen for traversal throughput; smaller masks did not improve the
measured workloads consistently (see `iteration-optimization-findings.md`). Mask
width is independent of SIMD register width and does not change the storage format.

Bit positions identify physical slots within the group. Waste slots, slots
outside the selected subtree, and unused high bits are always zero. Group
references provide the physical-slot mapping and tree lifetime.

Physical storage is reverse preorder. The source type determines group iteration
and set-bit extraction order. Public group matches retain an extraction direction
so each fragment can be consumed independently.

Postorder is not reverse preorder: reversing preorder also reverses sibling
order. Postorder can revisit a physical group with disjoint masks, since nodes
from another group may intervene. A group result represents an ordered fragment,
not necessarily every match in that physical group. Flattening fragments must
produce exactly the selected node order. The prototype emits singleton fragments
for postorder; batching compatible runs is deferred.

`Mask` supports intersection, emptiness, population count, and extraction of the
next matching slot in traversal order. SIMD kernels may use native vector masks
internally, but return the common bitmap representation. This allows predicates
over different column widths and existing flag bitmaps to compose directly.

Packing a vector comparison into a bitmap has a target-dependent cost. A future
combined kernel may intersect compatible vector results before packing once.
The group interface does not require exposing native SIMD mask types.

## Reverse iteration

`scan.rev()` reverses the selected order while preserving group filtering and
population counts. It can appear before or after filters. It does not change
preorder into postorder. `all().rev()` reverses whichever order `all()` chose,
without making that choice part of the public contract.

Choose direction before calling `nodes()` or `groups()`. `.rev()` changes the
source type, including through filters and range restrictions; reversing twice
restores the original type. Forward and reverse postorder retain only their own
traversal state. Node and group iterators advance in one direction, with no
`next_back()` or checks for opposite ends meeting. An individual group's node
iterator can still consume its single mask from either end.

Preorder needs group and subtree bounds. Forward postorder walks descending
slots, delaying ancestors until their subtree ends, with O(depth) stack space.
Reverse postorder keeps pending earlier siblings and may use O(nodes) space on a
wide tree. It expands a node only after yielding it and returns the last child
directly, so unary paths need no pending allocation. Neither traversal retains
decoded columns or buffers matching node handles.

## Byte-range traversal

`PreorderOverlappingBytes` locates the earliest group that might contain an
overlapping node, preserving overlapping ancestors. For each group it starts
with the valid subtree-slot mask.

- While candidates may end before or at the range start, intersect with an
  end-position comparison mask.
- When candidates may start at or beyond the range end, intersect with a
  start-position comparison mask.
- Skip a comparison when group bounds prove every candidate passes it.
- Stop when ordering bounds prove that no later group can overlap.

Use group bases and packed deltas directly. Where possible, translate an absolute
range bound into a comparison against stored deltas, handling bounds outside the
representable interval before narrowing. Do not unpack coordinates into a cache.

For nonempty node and scan ranges, overlap means
`node.start < range.end && node.end > range.start`. Empty scan ranges match
nothing. Zero-width nodes do not overlap a byte range, even when their position
lies strictly inside it. Point selection requires a separate operation.

Node ends are not monotonic in preorder. An early ancestor may extend across the
requested range, and a later node can end earlier than a previous node. Seeking,
skipping, and omitting comparisons must rely on conservative group/subtree bounds,
not a representative node. A coordinate encoding base is usable as an ordering
bound only where the layout guarantees that property.

## Predicate execution

Filters read stored columns directly:

- Kind and field predicates compare fixed-width IDs.
- Flag predicates intersect stored bitmaps.
- Supertype predicates test direct masks or a prepared dictionary-membership
  lookup. Such a grammar lookup is not a per-group unpack cache.
- Predicates over packed values decode only what their kernel needs.

Predicates prepare grammar-dependent state when attached to a scan: fixed-array
and dynamic single-kind IDs map to stored representations; supertype IDs resolve to
membership indices. This uses existing column metadata without another C call.

`retain_matches` permits both dense group evaluation and scalar evaluation of
surviving slots. Single-kind and field equality use SSE2 on x86_64, with a scalar
fallback elsewhere. Fixed arrays specialize equality by cardinality: one target
uses single equality, two combine equality masks, and larger arrays share column
loads across comparisons. Dynamic sets of two to four IDs combine equality masks;
larger dynamic sets use membership lookup. Singleton candidates and expensive
predicates use scalar checks. Flags intersect already-valid candidate masks, so
they need not reread group waste.

Filter order is initially call order. Empty masks short-circuit subsequent
filters. Choosing dense versus sparse evaluation stays inside the predicate,
where column costs are known; consumers need not make that choice.

## Terminal operations

`nodes()` retains a base slot and the current fragment's slot iterator. Unfiltered
preorder uses a range, avoiding per-node bit scans and mask updates; filtered
scans retain a mask. Extraction direction is constant for the source type.
Exhaustion is permanent. Specialized `fold` consumes each fragment in a local
loop, including a partially consumed fragment, before acquiring the next.

Unfiltered `count()` sums clipped live-slot spans; filtered counts sum
`Mask::count_ones()`. The node iterator's `count()` includes its partially consumed
fragment, so `scan.nodes().count()` also avoids constructing nodes. Hardware population
count depends on the compilation target.

Counts do not observe traversal order. An unconsumed postorder source counts
physical groups through preorder instead, composing the same pure predicates.
Once traversal has advanced, counting preserves the remaining topology. Predicate
composition keeps call order and short-circuits empty masks in both paths.

`groups()` exposes only nonempty groups, retaining subtree and range constraints.
Its standard iterator `count()` counts fragments, not nodes; `scan.count()` counts
nodes. Returned masks are values, and group references borrow the immutable tree,
not mutable iterator state.

## Prototype scope

Column metadata crosses the C boundary once per scan. Raw pointers become borrowed
slices there: bytes for the little-endian slab and native integers for grammar
tables. Scans and returned groups inherit `Send + Sync` from these immutable
borrows and their tree handle. Single-kind and field equality use dense SIMD
where available; extra and missing filters intersect stored flag bits. Supertype
filters read the stored membership mask or prepared grammar dictionary. There is
no unpack cache.

Preorder byte traversal uses conservative group bounds to skip groups and avoid
unnecessary comparisons. It preserves ancestors whose ends cross the requested
range. Postorder currently checks its singleton fragments with the same overlap
semantics. Ordered scans require `.nodes()` for ordinary iterator adapters;
`for node in scan` remains available through `IntoIterator`.

## Deferred work

- Reduce reverse postorder's pending topology storage. Greedy fragment batching
  did not improve the measured workloads enough to retain.
- Measure dense versus sparse thresholds per predicate.
- Combine cheap predicates before empty-mask checks where that improves throughput.
- Add point-range traversal and define behavior for trees without stored points.
- Add grammar-ID and parent-kind predicates if there are concrete use cases.
- Consider specialized value projection without adding an unpack cache.

Initial correctness checks should cover overlapping ancestors, group waste,
partial subtree groups, range boundaries, filter composition, and agreement
between node enumeration and population counts. Performance experiments should
compare complete group kernels, including mask packing, against scalar scans at
different selectivities.
