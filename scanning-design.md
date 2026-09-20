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

## Operation reference

SIMD below means explicit SSE2 kernels on x86_64. Other architectures use scalar
fallbacks. "Inherited" means upstream predicates may use SIMD. Brace notation
groups byte and point counterparts, such as `overlapping_{bytes,points}`.

| Operation | Role / representation | SIMD | Important behavior |
| --- | --- | --- | --- |
| `preorder()` | Initial source; slot ranges or masks | No | Descending physical slots; clips waste and subtree boundaries. |
| `postorder()` | Initial source; singleton masks | No | Walks topology; delays ancestors using O(depth) storage. |
| `all()` | Initial source | No | Currently preorder; order is unspecified by the API. |
| `.rev()` | Reverses the source through adapters | Inherited | Choose before consumption. Reverse postorder can require O(nodes) pending storage. |
| `descendants_matching_kinds(...)` | Convenience source + mask filter | Conditional | Preorder with a kind filter; includes the root if matching. |
| `overlapping_{bytes,points}` | Range restriction + mask filter | Yes¹ | Includes zero-width nodes inside the half-open query. |
| `within_{bytes,points}` | Range restriction + mask filter | Yes¹ | Both endpoints inside, including boundaries; empty queries match zero-width nodes. |
| `containing_{bytes,points}` | Range restriction + mask filter | Yes¹ | Inclusive endpoint containment, including empty queries and equal spans. |
| `starting_in_{bytes,points}` | Range restriction + mask filter | Yes¹ | Tests only the start against a half-open interval. |
| `ending_in_{bytes,points}` | Range restriction + mask filter | Yes¹ | Tests the exclusive end coordinate itself against a half-open interval. |
| `containing_{byte,point}` | Position restriction + mask filter | Yes¹ | `start <= position < end`; excludes zero-width nodes. |
| `starting_at_{byte,point}` | Position restriction + mask filter | Yes¹ | Exact start equality; includes zero-width nodes. |
| `ending_at_{byte,point}` | Position restriction + mask filter | Yes¹ | Exact end equality; includes zero-width nodes. |
| `filter_kind_ids(array)` / `filter_field_ids(array)` | Mask filter | Yes² | Array length specializes the kernel; larger arrays share column loads across targets. |
| `filter_kind_ids(&IdSet)` / `filter_field_ids(&IdSet)` | Mask filter | Conditional² | SIMD for 1–4 IDs; larger sets use scalar membership checks. |
| `filter_field_id(id)` | Mask filter | Yes² | Fixed-width equality; zero means no field. |
| `filter_extra(bool)` / `filter_missing(bool)` | Mask intersection | No | Intersects stored flag bitmaps without per-node decoding. |
| `filter_supertype_id(id)` | Membership checks → mask | Conditional³ | SIMD for direct membership masks; dictionary lookup remains scalar. |
| `.nodes()` / `for node in scan` | Node consumer | Inherited | Unfiltered preorder uses contiguous slot ranges; filtered scans extract mask bits. |
| `.count()` / `.nodes().count()` | Aggregate consumer | Inherited | Counts slots/populations without constructing nodes. Fresh postorder counts use preorder groups, currently without range seeking. |
| `.groups()` | Group-and-mask consumer | Inherited | Returns nonempty fragments; postorder may revisit a group. Its `.count()` counts fragments. |
| `group_matches.nodes()` | Individual mask consumer | No | Extracts nodes in fragment order; supports consumption from either end. |
| `.nodes().fold(...)` | Specialized node consumer | Inherited | Drains each fragment in a local loop, reducing iterator bookkeeping. |
| Other adapters after `.nodes()` | Ordinary node iteration | No group kernel | `map`, `filter`, `find`, etc. operate on individual node handles. |

¹ Range kernels compare encoded deltas directly: SIMD with at least three
candidates, scalar with one or two. Group bounds can accept or reject everything
first, and preorder can reject groups before constructing candidate masks.
Oversized point coordinates use decoded scalar comparisons.

² ID kernels use scalar equality for singleton candidates. Empty sets match
nothing. Sets use OR internally; successive filters use AND.

³ Grammars with at most eight supertypes store membership bits directly in `u16`
lanes. Masks with at least three candidates use SIMD; one or two use scalar bit
tests. Larger grammars store dictionary IDs and use scalar membership lookup.

One range/position restriction is allowed before other filters. Preorder seeks
only the upper start boundary; postorder enumeration has no range pruning.
Masks always exclude waste and nodes outside the selected subtree.

## Typed composition

Range restrictions combine traversal pruning with group predicates. Current scan
types include:

```rust
Scan<'tree, Preorder<'tree>>
Scan<'tree, Postorder<'tree>>
Scan<'tree, PreorderOverlappingBytes<'tree>>
Restricted<Source, Selection<Coordinates, Relation>>
Filtered<Source, Predicate>
```

Each type stores only its own arguments and inner source. Restrictions
select the coordinate system and relation through types, allowing specialized
kernels without a per-node relation switch. Apply range or position restrictions
before other filters. Bounds cannot change after scanning has started.

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

Preorder's `next_matching` lets a predicate inspect group bounds before building
the live-slot mask. `Predicate::retain_group` receives a deferred mask constructor;
range selections invoke it only for groups that survive conservative rejection.
Other predicates default to constructing the mask immediately. Counts use this
same path, and composition keeps the first predicate's opportunity to reject
before mask construction. Postorder retains its ordinary fragments.

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

## Range and position selection

Byte range methods accept `Range<usize>`; point range methods accept `Range<Point>`.
Points compare by row, then column, using the same coordinates as
`start_position()` and `end_position()`. A point range is a continuous source
interval, not a rectangle.

For a nonempty node `start..end` and nonempty query `from..to`:

| Byte filter | Point filter | Matches when |
| --- | --- | --- |
| `overlapping_bytes` | `overlapping_points` | `start < to && from < end` |
| `within_bytes` | `within_points` | `from <= start && end <= to` |
| `containing_bytes` | `containing_points` | `start <= from && to <= end` |
| `starting_in_bytes` | `starting_in_points` | `from <= start && start < to` |
| `ending_in_bytes` | `ending_in_points` | `from <= end && end < to` |

Reversed query ranges match nothing for every relation. For empty queries,
`within_*` matches zero-width nodes at the query position, and `containing_*`
matches when `start <= position && position <= end`, including a node's end
boundary and a zero-width node at that position. Other relations match nothing.
Containment includes equality: a node with the query's exact span qualifies for
both `within_*` and `containing_*`.
All matching nodes are returned, including nested nodes; `within_*` does not
select only the outermost qualifying nodes.

Zero-width nodes at `position` follow these rules:

- `overlapping_*`, `starting_in_*`, and `ending_in_*` match when
  `from <= position && position < to`.
- `within_*` matches when `from <= position && position <= to`,
  including both boundaries under endpoint containment.
- `containing_*` matches only the empty query `position..position`.

`ending_in_*` selects the exclusive end coordinate itself, not the last occupied
byte or character. A node ending at `to` is excluded, and one ending at `from`
is included. This applies equally to byte and point coordinates.

Overlap selects syntax touching a region; containment selects syntax wholly
inside it or enclosing it. Start/end membership selects position-anchored records
or assigns each node to one of adjacent windows without duplicates. For example,
a node spanning `80..120` overlaps both `0..100` and `100..200`, lies within
neither, starts in the first, and ends in the second.

Single-position queries are separate from empty ranges:

| Byte position | Point position | Matches when |
| --- | --- | --- |
| `containing_byte` | `containing_point` | `start <= position && position < end` |
| `starting_at_byte` | `starting_at_point` | `start == position` |
| `ending_at_byte` | `ending_at_point` | `end == position` |

Single-position containment excludes zero-width nodes; exact start/end matching
includes them. Unlike range containment of `position..position`, single-position
containment also excludes nodes ending at that position. No single-position
overlap or within variants are planned.

Point filters pack query bounds into `u64` keys, with row in the high word and
column in the low word, once per group. They translate these bounds into encoded
`u16` delta intervals, handling row and column differences separately. Byte
filters likewise translate bounds into `u8` start or `u16` end delta intervals.
On x86_64, masks with at least three candidates use SSE2; smaller masks and other
architectures use scalar delta comparisons. Bounds exceeding `u32` retain full
`Point` comparisons to avoid truncation. No source text or conversion to byte
offsets is needed. When the tree has no stored points, use the existing
node-position convention `(0, byte_offset)` for the same comparisons.

## Range traversal

Pruning depends on the relation and coordinate system. Start/end membership
needs only its corresponding endpoint column; overlap and containment need both.
Use group bases and packed deltas directly. Where possible, translate an absolute
bound into a comparison against stored deltas, handling bounds outside the
representable interval before narrowing. Do not unpack coordinates into a cache.

Delta columns retain their slab slice, offset, and length until decoding or
comparison is required. Group rejection and acceptance use only bases, avoiding
delta-slice bounds checks on those paths. Reads still use checked slices.

Preorder restrictions use a binary search over group start minima to remove
groups beyond the relation's upper bound on node starts. Point bases store
independent row and column minima; seeking reconstructs the earliest live node's
position to obtain a bound ordered across groups. Each relation refines valid
subtree-slot masks with conservative group bounds and endpoint comparisons.
Further seeking and early termination require ordering guarantees.

Zero-width overlap changes boundary rejection. A group whose maximum end equals
the query start may contain matching zero-width nodes, so only a maximum end
strictly before the query start proves rejection. Likewise, a nonempty parent
ending at the query start does not overlap, but its zero-width descendants at
that boundary may overlap. Do not prune such a subtree solely because its parent
fails the overlap predicate.

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

Direct supertype masks use SSE2 bit tests for at least three candidates, with
scalar checks for smaller masks. This applies to grammars with at most eight
supertypes; dictionary-based membership remains scalar.

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

Base column metadata crosses the C boundary once per scan. Raw pointers become borrowed
slices there: bytes for the little-endian slab and native integers for grammar
tables. Scans and returned groups inherit `Send + Sync` from these immutable
borrows and their tree handle. Single-kind and field equality use dense SIMD
where available; extra and missing filters intersect stored flag bits. Supertype
filters read the stored membership mask or prepared grammar dictionary. There is
no unpack cache.

All range and single-position methods above are implemented. Overlap includes
zero-width nodes inside the half-open query range. Empty queries match only for
range containment, using inclusive endpoint comparisons.
Point-column offsets cross a separate C bridge only when a point filter is
attached, leaving ordinary scans' column metadata unchanged. Stored-point and
byte-fallback decoders are selected once per group.

Preorder byte traversal uses conservative group bounds to skip groups and avoid
unnecessary comparisons. It preserves ancestors whose ends cross the requested
range. Postorder checks its singleton fragments with the same relation, without
range-based traversal pruning. Ordered scans require `.nodes()` for ordinary
iterator adapters; `for node in scan` remains available through `IntoIterator`.

## Deferred work

- Reduce reverse postorder's pending topology storage. Greedy fragment batching
  did not improve the measured workloads enough to retain.
- Measure dense versus sparse thresholds per predicate.
- Combine cheap predicates before empty-mask checks where that improves throughput.
- Add grammar-ID and parent-kind predicates if there are concrete use cases.
- Consider specialized value projection without adding an unpack cache.

Correctness checks should cover every relation in both coordinate systems,
zero-width nodes at either boundary and inside the range, empty/reversed queries,
equal spans, multiline points, and trees without stored points. Also cover
overlapping ancestors, group waste, partial subtree groups, filter composition,
both traversal directions, and agreement between node enumeration and population
counts. Performance experiments should compare complete group kernels, including
mask packing, against scalar scans at different selectivities.
