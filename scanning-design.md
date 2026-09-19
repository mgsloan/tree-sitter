# Rust scanning API

Initial proposal. This describes a new group-based scan API, not the existing
`Preorder` or `NodeIterator` implementation.

The scan reads stored columns directly and retains a compact mask of matching
slots for each group. There is no unpack cache: neither traversal, filters, nor
projection retain decoded column arrays. A kernel may decode values into
registers while evaluating a group.

## Public API

```rust
let scan = node.preorder()
    .overlapping_bytes(from..to)
    .filter_kind_ids(&kinds)
    .filter_field_id(field);

for node in scan.nodes() {
    // ordinary Node access
}
```

The scan is a typed pipeline with three terminal operations:

- `nodes()` returns an ordinary iterator of `Node` handles.
- `count()` sums population counts without constructing nodes.
- `groups()` returns an iterator of group references and matching masks.

Scans include the root, remain inside its subtree, preserve preorder, and yield
each matching node once. IDs belong to the tree's language. Public kind IDs are
the default; grammar IDs, if supported, use a separate method.

Kind sets use OR; successive filters use AND. An empty kind set matches nothing.
Field matching tests the child's field relative to its parent, without implying
a parent-kind constraint. Supertypes test actual node membership rather than a
global expansion into concrete kinds.

Ordinary mapping follows `nodes()`. Specialized consumers can process groups
directly. There is no `map_cached` operation or type-level column-cache machinery.

## Typed composition

`overlapping_bytes` changes traversal rather than appending a predicate:

```rust
Preorder<'tree>
PreorderOverlappingBytes<'tree>
Filtered<Source, Predicate>
```

Each type stores only its own arguments and inner source. The initial API
requires range restriction before filters. Bounds cannot change after scanning
has started.

The internal group protocol is schematically:

```rust
struct GroupMatches<'tree> {
    group: GroupRef<'tree>,
    matches: Mask,
}

trait GroupScan<'tree> {
    fn next_group(&mut self) -> Option<GroupMatches<'tree>>;
}

trait Predicate {
    fn retain_matches(&self, group: GroupRef<'_>, candidates: Mask) -> Mask;
}
```

A filtered source obtains a group from its inner source, refines its mask, and
continues if the result is empty. Predicates must return a subset of their input
mask. Built-in predicates are pure; custom callbacks with observable side effects
are outside the initial interface.

Generic composition permits inlining and specialization without dynamic dispatch
or allocations for adapters. It does not guarantee SIMD. The hot column reads
and predicate kernels must be visible to the optimizer; per-node opaque C calls
would limit optimization.

## Masks and group identity

Use a compact integer bitmap behind a `Mask` newtype. A `u64` is sufficient while
groups have at most 64 slots; the current 16-slot groups do not require widening
the storage format. Mask width is independent of SIMD register width.

Bit positions identify physical slots within the group. Waste slots, slots
outside the selected subtree, and unused high bits are always zero. Group
references provide the physical-slot mapping and tree lifetime.

Physical storage is reverse preorder. Group iteration and set-bit extraction
must follow logical preorder, rather than assuming lowest-bit-first iteration.
Keep that mapping in group/node iteration helpers.

`Mask` supports intersection, emptiness, population count, and extraction of the
next matching slot in traversal order. SIMD kernels may use native vector masks
internally, but return the common bitmap representation. This allows predicates
over different column widths and existing flag bitmaps to compose directly.

Packing a vector comparison into a bitmap has a target-dependent cost. A future
combined kernel may intersect compatible vector results before packing once.
The group interface does not require exposing native SIMD mask types.

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
nothing. The treatment of zero-width nodes needs a separate decision before
implementation; point selection should not be inferred from an empty range.

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

`retain_matches` permits both dense group evaluation and scalar evaluation of
surviving slots. Start with dense kernels for cheap comparisons and selected-slot
evaluation for expensive predicates. Dense kernels can initially use scalar Rust
and gain explicit SIMD where measurement justifies it.

Filter order is initially call order. Empty masks short-circuit subsequent
filters. Choosing dense versus sparse evaluation stays inside the predicate,
where column costs are known; consumers need not make that choice.

## Terminal operations

`nodes()` retains the current group and remaining mask. Each `next()` removes
one matching bit and creates its node handle. Exhaustion is permanent.

`count()` sums `Mask::count_ones()`. Override the node iterator's `count()` as
well, including its partially consumed mask, so `scan.nodes().count()` avoids
constructing nodes. Hardware population count depends on the compilation target.

`groups()` exposes only nonempty groups, retaining subtree and range constraints.
Its standard iterator `count()` counts groups, not nodes; `scan.count()` counts
nodes. Returned masks are values, and group references borrow the immutable tree,
not mutable iterator state.

## Deferred work

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
