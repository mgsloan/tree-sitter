# Range scans

Range scans combine seeking, subtree skipping, and group predicates. Preorder
seeks both start bounds when the relation permits it. Forward preorder byte scans
also skip descendant groups when their ancestors end too early. Remaining groups
are rejected or accepted using coordinate bounds. Groups that cannot be decided
from their bounds compare stored deltas directly, using SIMD for dense candidate
masks on x86_64.
Postorder enumeration applies the same predicates after visiting each node,
without range-based traversal pruning.

This document describes the Rust scanning API in
[scan.rs](crates/squatter/src/scan.rs).
[scanning-design.md](scanning-design.md) covers the broader API. The descendant
seeks in [node.c](lib/squat/node.c) and query-cursor range restrictions are separate
implementations; these scans do not call them.

## Relations and boundaries

Each relation has byte and point variants. Bytes use `usize`; points use
`tree_sitter::Point`, ordered by row and then column. Columns count bytes, not
characters. A point range is a continuous source interval, not a rectangle.

Let `start..end` be a node's span, `from..to` a query range, and `position` a query
position. For nonempty query ranges:

| Byte method | Point method | Predicate |
| --- | --- | --- |
| `overlapping_bytes` | `overlapping_points` | `start < to && (end > from \|\| start >= from)` |
| `within_bytes` | `within_points` | `from <= start && end <= to` |
| `containing_bytes` | `containing_points` | `start <= from && to <= end` |
| `starting_in_bytes` | `starting_in_points` | `from <= start && start < to` |
| `ending_in_bytes` | `ending_in_points` | `from <= end && end < to` |
| `containing_byte` | `containing_point` | `start <= position && position < end` |
| `starting_at_byte` | `starting_at_point` | `start == position` |
| `ending_at_byte` | `ending_at_point` | `end == position` |

The overlap predicate includes zero-width nodes inside the half-open query. For
nonempty nodes it reduces to ordinary overlap, `start < to && from < end`, since
`start <= end`. The `start >= from` alternative admits empty nodes at `from`.

All reversed query ranges match nothing. Empty query ranges have relation-specific
behavior:

| Relation | Result for `position..position` |
| --- | --- |
| Overlapping, starting in, ending in | No matches |
| Within | Zero-width nodes exactly at `position` |
| Containing | Every node with `start <= position && position <= end` |

For a zero-width node at `position`, overlap/start/end membership requires
`from <= position < to`; within requires `from <= position <= to`. Such a node
contains only the empty range at its position. It never matches single-position
containment, but can match either exact-endpoint method.

Consequently, `containing_bytes(position..position)` includes nodes ending at
`position`, while `containing_byte(position)` excludes them. `ending_in_*` tests
the exclusive end coordinate itself, not the last occupied byte. Both containment
relations include equal spans and return all qualifying nested nodes.

## Storage and execution

Nodes occupy reverse-preorder slots in groups of 16, 32, or 64; the default is 16.
Forward preorder walks groups and slots in descending physical order. Node starts
are nondecreasing in preorder, but node ends are not: an ancestor can precede many
descendants whose ends are earlier than its own.

The [cloud group-size comparison](iteration-optimization-findings.md#range-scans-with-16--32--and-64-slot-groups-2026-09-19)
finds substantial narrow-overlap and count gains at 32/64 slots. At 32, default
slabs are slightly smaller; at 64, they grow 30–50% on the measured corpora.
Broad reverse byte overlap regresses, so group size remains a workload tradeoff.

Every subtree occupies a contiguous physical slot interval, possibly containing
group waste. `Preorder` clips each group's live slots to that interval. Range
bounds can reject a whole group before this mask is constructed. Otherwise the
mask excludes waste and nodes outside the chosen root before slot comparisons
run. A `u64` holds candidate and result masks for all supported group sizes.

The pipeline is:

```rust
root.preorder()
    .overlapping_bytes(from..to)
    .filter_kind_ids([identifier])
    .nodes()
```

1. `Columns::new` gets slab slices and column offsets through
   `sq_tree_scan_columns` once. A point selection makes one additional call to
   `sq_tree_scan_point_layout` for its offsets.
2. `Scan::selected` constructs `Selection<Coordinates, Relation>`. Unless the
   relation is empty, it asks the traversal to restrict its group bounds.
3. Composed selections and filters ask their source for the next matching fragment.
   A selective kind filter uses the persisted symbol index to skip directly to a
   possible group within the remaining bounds. Before bitmap jumps, forward byte
   scans try subtree rejection at the current boundary; otherwise the jump could
   bypass a useful ancestor span. Sparse entries jump directly and supply exact
   slot masks. Per-target cursors reuse sparse posting positions across groups,
   with bounded local probing and binary search after large jumps. Bitmap entries
   require a symbol-column comparison.
   Forward preorder byte scans check whether the selected group's end bound excludes
   its subtrees. Preorder passes a deferred mask constructor to the selection, so
   rejected groups need no waste lookup or subtree clipping. Other traversals
   supply their ordinary fragments. Later filters refine survivors in call order.
4. `nodes()` extracts matching slots, `groups()` exposes matching fragments, and
   `count()` sums mask population counts without creating node handles.

Coordinate systems and relations are generic types, so there is no per-node
relation switch or dynamic dispatch. Range kernels read the slab directly;
they neither call node accessors through C nor retain decoded column arrays.
Endpoint predicates translate query bounds into group-relative delta intervals.
On x86_64, masks with at least three candidates use SSE2; one or two candidates
use the scalar set-bit loop. Other architectures use that scalar loop for all
masks. Both paths compare encoded deltas without reconstructing each position.

`ColumnDeltas` keeps the slab slice, offset, and length until a comparison needs
the deltas. Rejected and wholly accepted groups avoid delta-slice bounds checks.
The eventual slice remains checked; no additional unchecked slab reads are used.

Only unrestricted traversals expose selection methods: a scan accepts one range
or position selection, before other filters. Reversal preserves that selection.
`all()` currently selects preorder but does not promise that order as an API
contract.

Kind-index preparation samples bitmap density and keeps the flat kernel for
dense selections. Missing indexes also use that fallback. This adds no slab data;
the index is already built by default for trees exceeding 32 groups. Dynamic kind
sets with more than four IDs inspect only surviving slots when at most four remain,
so a selective range or preceding filter avoids scanning every used slot.
Dynamic sets of up to sixteen IDs prepare their stored symbol values once and
share the fixed-array SIMD kernel. With three or more targets, dense groups load
each symbol chunk once for all comparisons; sparse candidate masks retain their
membership shortcut.
The [index and sparse-mask measurements](iteration-optimization-findings.md#indexed-symbol-filters-and-sparse-candidate-masks-2026-09-20)
cover rare/absent IDs, dense-scan regressions, and filter combinations at all three
group sizes.
The [prepared-filter measurements](iteration-optimization-findings.md#prepared-symbol-filters-and-composed-counts-2026-09-20)
cover the dense count recovery, shared SIMD kernels, and multiple-filter pipelines.
The [sparse-cursor measurements](iteration-optimization-findings.md#sparse-symbol-index-cursors-2026-09-20)
cover posting reuse, reverse scans, composed filters, and the added predicate state.

### Coordinate columns and conservative bounds

Packing in [pack.c](lib/squat/pack.c) stores starts as base plus delta and ends as
base minus delta. The current column abstractions expose these bounds:

| Column | Per-slot encoding | Minimum used by scans | Maximum used by scans |
| --- | --- | --- | --- |
| Byte start | `base + u8_delta` | `base` | `base + 255` |
| Byte end | `base - u16_delta` | `base - 65535` | `base` |
| Stored point start | `base + expanded_delta` | `base` | `base + expanded_65535` |
| Stored point end | `base - expanded_delta` | `base - expanded_65535` | `base` |

Bound arithmetic saturates. Byte start bases are actual group minima; byte end
bases are actual group maxima. Their opposite bounds describe the encoding's
capacity, not measured extrema. A group with ends near byte 100,000 can therefore
have a scan minimum of 34,465 even if every end is above 99,900.

Stored point bases contain independent row and column minima for starts, or
maxima for ends. These pairs need not be actual node positions. A stored `u16`
delta holds an 8-bit row difference and an 8-bit column difference. Absolute
positions, when needed for seeking or fallback comparisons, are reconstructed
using:

```text
point key      = (row << 32) | column
expanded delta = ((delta >> 8) << 32) | (delta & 255)
```

Unsigned `u64` comparisons implement point order. `retain_points` attempts to
pack the query bounds once per group, then translates them into delta bounds.
If any query component exceeds `u32`, it uses `UnpackedPositions` and full `Point`
comparisons instead of truncating.
The stored-point versus absent-point decoder is also selected once per group.
Without stored points, point filters reuse byte columns with coordinates
`(0, byte_offset)`; they do not reconstruct source lines.

Bounds cover the entire physical group, even when the candidate mask selects
only a subtree fragment or one node. That is safe but can weaken rejection and
acceptance.

### Direct delta comparisons

`PositionColumn::retain` accepts inclusive, exclusive, or unbounded endpoints.
Byte and stored-point columns convert them into a half-open interval of encoded
deltas. `delta_bounds` reverses that interval for end columns, whose values
decrease as their deltas increase. It uses `u32` thresholds so 256 and 65,536
can represent the boundary after the largest `u8` or `u16` value.

Byte thresholds subtract the column base from the query, reversing the
subtraction for ends. `byte_cutoff` handles out-of-range queries before
narrowing. `point_cutoff` derives row and column differences separately: query
columns outside the encoded column interval admit all or none of that delta
row, while earlier delta rows still qualify. This avoids expanding each `u16`
to a `u64` absolute point.

`retain_deltas` returns immediately for empty or unrestricted delta intervals.
Its scalar path compares individual encoded values. The SSE2 path subtracts the
interval's lower bound and biases the result's sign bit, allowing a signed
comparison to test the unsigned interval. Single-value intervals use equality.
Byte starts use 8-bit lanes; byte ends and stored points use 16-bit lanes.
Comparison results become a bitmap, intersected with the original candidates
to exclude waste and slots outside the subtree.

The generic column implementation remains a decoded scalar fallback for query
points with components larger than `u32`.

### Seeking

`Preorder::restrict` binary-searches group start minima. Since those minima
decrease as physical group indices increase, it raises `groups.start` to exclude
the groups at the late-source end. Reverse preorder uses the same restriction.
Each relation supplies an upper bound on possible node starts:

| Relation | Necessary start bound |
| --- | --- |
| Overlapping, starting in, ending in | `start < to` |
| Within | `start <= to` |
| Containing a range | `start <= from` |
| Any single-position relation | `start <= position` |

End-only relations can use this seek because `start <= end`. The inclusive bound
for within preserves zero-width nodes at the query's upper boundary.

Byte seeking reads the stored start base. Stored point seeking reconstructs the
start of the group's earliest preorder node, at physical slot `used - 1`.
Independent point base minima are conservative within a group but are not
necessarily ordered between groups, so they cannot replace that reconstruction.

`starting_in_*`, `starting_at_*`, and `within_*` also supply an inclusive lower
start bound. A second binary search excludes earlier groups. It retains the
first group whose minimum falls below the bound, since later starts in that
group may still qualify. Slot predicates finish both boundary groups. This works
in either preorder direction and preserves repeated starts across groups.
An inclusive lower bound at coordinate zero needs no search. Bound variants and
point probes are forced inline to avoid general bound handling and copies of
column metadata inside binary search.

Overlap, containment, and end-only relations have no lower start cutoff: earlier
ancestors may still qualify. Their forward byte scans use the subtree shortcut
below; point and reverse scans still walk the earlier groups. End maxima are not
ordered, so they cannot be binary-searched like starts.

### Skipping rejected subtrees

Before constructing a forward preorder byte group's ordinary predicate, the
selection can prove that every node in the group ends too early:

| Relation | Sufficient rejection bound |
| --- | --- |
| Overlapping, ending in | `maximum_end < from` |
| Containing a range | `maximum_end < to` |
| Containing a position | `maximum_end <= position` |
| Ending at | `maximum_end < position` |

Descendants end no later than their ancestors. The group's last preorder node
occupies its first physical slot, so its subtree extends into lower-index groups.
Its stored span base is a conservative minimum span. Subtracting that base from
the slot gives a safe boundary for skipping whole descendant groups, rounded up
to retain any partially covered group and clipped to the scan's remaining bounds.

Groups whose spans all fit in a byte have a zero span base. They reject without
reading span deltas or attempting short jumps. This keeps the common path cheap;
using exact spans after ordinary predicate evaluation was slower in cloud tests.
No index, allocation, or format change is required.

Overlap uses a strict end comparison because a parent ending at `from` can have
zero-width descendants there. Rejection by other filters does not establish this
subtree proof. Composed counts use the selection's proof before other predicates.
Reverse preorder counts use forward skipping over their remaining group interval;
reverse enumeration keeps its original group walk.
Queries without a nontrivial lower end bound use a separate group loop without
subtree checks, including overlap queries starting at zero.

Point selections retain flat group traversal after seeking. The equivalent
pruning kernel improved narrow overlap queries but regressed broad point scans
and some counts. It remains disabled; two-sided point seeking is enabled.

### Group predicates

`Overlapping::retain` has its own kernel. It rejects a group when its minimum
start is at least `to`, or its maximum end is strictly before `from`. It accepts
all candidates when both of these facts hold:

- Every start is before `to`.
- Every end is after `from`, or every start is at least `from`.

Otherwise it filters starts before `to`, then unions ends after `from` with
remaining starts at or after `from`, omitting conditions already proved by the
bounds. Maximum end equal to `from` does not prove rejection: zero-width nodes
at `from` may match.

`Within`, `Containing`, and `ContainingPosition` use `retain_pair`. Each endpoint
condition is monotonic, so testing it at the column's minimum and maximum can
prove that all or no candidates pass. If either condition rejects the whole
group, the mask is empty. If both accept the whole group, the mask is unchanged.
Otherwise it filters the first column and passes survivors to the second,
skipping conditions already proved and stopping when no candidates remain.

`StartingIn` and `EndingIn` use `retain_interval` on one column. Disjoint bounds
reject the group; bounds wholly inside the half-open query accept it; other
groups need a delta interval comparison. `StartingAt` and `EndingAt` use
`retain_equal`, which rejects out-of-bounds positions and otherwise compares
against a single-value delta interval, or rejects an unrepresentable position.

These predicate shortcuts avoid endpoint decoding. The separate subtree proof
above can also avoid group traversal. There is no hierarchy of range summaries
or range-specific index used by the scan.
`Predicate::retain_group` can use them before invoking the candidate-mask
constructor. Preorder's `next_matching` supports this for both enumeration and
counts; composed predicates retain their order and pass the deferred mask only
to the first predicate.

### Postorder and counts

Postorder's `restrict` methods are no-ops. Both directions walk the entire
subtree and produce singleton fragments before applying the range predicate.
The same group metadata and bounds may therefore be read repeatedly. Forward
postorder delays ancestors on an O(depth) stack; reverse postorder keeps pending
siblings and can require O(nodes) space on a wide tree.

A fresh postorder `count()` avoids that topology walk by constructing `Preorder`
and passing it the combined pure predicates. However, it does not call
`Preorder::restrict`: the selection has already become a predicate. Thus a fresh
postorder range count visits all groups, including those an explicit preorder
range count would exclude by binary search. This applies to reverse postorder
and fresh `nodes().count()` too. After enumeration starts, counting preserves the
remaining traversal state and includes any pending fragment.

Empty selections short-circuit before traversal or counting. Here “empty” means
the relation cannot match: an empty within or containing query still runs.
`groups().count()` counts fragments, not matching nodes.

### Cost

For a subtree with `G` physical groups, let `V` be groups visited after seeking
and subtree skipping, `C` the candidate slots in groups needing delta comparisons,
and `K` the matches.
Preorder range enumeration costs O(log G + V + C + K); SIMD reduces comparison
constants, and counting omits the output term. With fixed group size, the worst
case remains O(nodes). A small `K` does
not imply a small `V`.

Postorder enumeration remains O(nodes), plus its topology storage. Fresh
postorder counts cost O(G + C) without topology storage. Byte and point scans
share these bounds, but point setup, decoding, and oversized-query fallback
change their constants.

## Improvements

The following are proposals, not current behavior or measured speedups. Reducing
the number of visited groups is likely more valuable for narrow queries than
optimizing comparisons in groups that should never be visited.

### 1. Preserve seeking when counts change traversal

Give fresh postorder counts a way to transfer their selection to the replacement
preorder source before counting. This can reuse the existing binary search with
no new slab metadata. Preserve the current path for partially consumed scans.
Avoid seeking a second time for sources already restricted during construction.

### 2. Clip slots at the seek boundaries

`starting_in_*`, `starting_at_*`, and `within_*` now seek both group boundaries.
They could also seek the exact slots inside the retained boundary groups:

- Starting in: `from <= start < to`.
- Starting at: `start == position`.
- Within: `from <= start <= to`, followed by the end test.

Because starts are ordered, start-only matches form a contiguous live-slot
interval. Finding its slot boundaries could replace the remaining per-slot start
checks. Within still needs end filtering inside that interval.

Overlap, containment, and end-only relations cannot use `start >= from` as a
lower cutoff. It would discard long ancestors that start earlier and still
qualify. Group end maxima are not monotonic either, so binary-searching those
maxima directly is invalid.

### 3. Tighten bounds without changing storage

Read exact start extrema from the first and last live slots, or from the
candidate-mask extremes when useful. Starts are ordered within each group too.
This replaces `base + 255` and the looser point encoding bounds with actual
coordinates. Use the relation and mask density to decide whether the extra
loads are worthwhile.

End minima can also use the invariant `end >= start`: the greater of the current
end minimum and a valid start minimum is a stronger bound. Avoid scanning all
end deltas merely to compute bounds for a single predicate; that can cost as much
as evaluating it. Exact end extrema become more attractive when reused by an
index or several operations.

Point setup has smaller opportunities: prepare packed query bounds once per scan,
and specialize stored versus absent points outside the group loop. Point seeking
can compare base rows first and reconstruct the earliest start only when rows
tie, as the existing C descendant seek does. Keep the oversized-coordinate path.

### 4. Extend subtree pruning

Forward preorder byte scans use end bounds and span bases to skip rejected
subtrees. Further pruning could use individual nodes, both endpoints, or whole-subtree
acceptance. Point pruning needs a kernel that preserves broad-scan performance:

- Overlap can reject a subtree ending strictly before `from` or starting at or
  after `to`.
- Within can reject a subtree ending before `from` or starting after `to`. Once
  its root is within the query, every descendant passes the range predicate.
- Containment can reject a subtree whose root does not contain the requested
  range or position.

For overlap, a nonempty parent ending exactly at `from` does not match, but its
zero-width descendants there can match. For within, a parent extending outside
the query can still have matching descendants. Subtree rejection needs its own
proof; it cannot generally reuse the node's rejection result.

This could particularly improve postorder, which currently pays topology costs
for every rejected node. Integrate pruning before expanding a subtree, preserve
the selected order, and reuse the traversal's masks for accepted subtrees without
further range checks. Other predicates must still run. A hybrid should retain
flat group traversal where descending through topology would cost more than
scanning nearby groups.

The C descendant seeks already provide examples of binary search, delta
thresholds, and ancestor traversal. They return one descendant and have different
empty-boundary rules, so calling a seek and walking one ancestor chain is not a
general replacement. In particular, inclusive empty-range containment can match
both sides of a shared sibling boundary, including zero-width siblings.

### 5. Tune delta kernels and composition

Direct delta comparisons and SSE2 kernels are implemented. The current dense
threshold is three candidates; measure other thresholds and architectures before
specializing further. Combining endpoint vector results before packing a bitmap
could avoid intermediate masks, particularly when both endpoints need testing.
Preserve overlap's extra start condition for zero-width nodes.

Benchmark complete kernels, including bounds, loads, unsigned comparisons, mask
extraction, and candidate clipping; comparison throughput alone is insufficient.

### 6. Reorder cheap predicates where it saves decoding

Selections currently precede kind, field, and flag filters. For a broad range
and a selective cheap filter, computing that filter's mask first could avoid
many endpoint reads. Separate traversal bounds from local predicate execution
so range seeking still happens first, then consider ordering pure built-in
predicates by cost and selectivity. This needs measurement: a narrow range often
rejects a whole group more cheaply than another filter would.

### 7. Add range summaries only if traversal remains the bottleneck

For large trees with frequent narrow overlap or end queries, a hierarchy over
blocks of groups could store conservative end maxima and skip whole blocks
whose ends are too early. Optional minima would support more whole-block
acceptance and rejection. A hierarchy localizes the effect of long ancestors
whose ends keep an individual block relevant.

This trades additional memory, construction work, and possibly slab-format
changes for fewer group visits. An optional derived index would allow measuring
the tradeoff before changing persistence. Existing start order and subtree spans
should be exploited first.

## Validation and measurement

[scanning.rs](crates/squatter/tests/scanning.rs) already compares all relations
with scalar node-attribute predicates across traversal directions, terminals,
subtrees, stored/absent points, empty and reversed queries, malformed input,
zero-width nodes, oversized bounds, and repacked borrowed slabs. Relevant tests
are `range_and_position_relations` and `zero_width_overlap_boundaries`.

Pruning changes should additionally target long ancestors crossing many groups,
many equal starts, adjacent siblings at an empty-query boundary, and partial
groups containing qualifying nodes outside the scan root. Check groups of
16/32/64 and counts after partial enumeration. Unit tests in `scan.rs` also
compare delta filtering against decoded positions across every `u16` encoding,
including signed-lane boundaries, exact matches, and queries between encoded
point rows.

[scanning-bench.rs](crates/squatter-bench/src/bin/scanning-bench.rs) benchmarks
byte and point overlap with node, count, fold, and scalar variants. Queries
default to the middle 1% of each source; `--range-start-percent` and
`--range-percent` control their position and width. It does not characterize the
other relations or range-restricted postorder. Extend it with out-of-root queries,
exact positions, empty containment/within, deep trees, wide trees, and selective
filters.

Measure query latency alongside input/output throughput. Instrument group visits,
groups rejected or accepted by bounds, and slots decoded in separate diagnostic
runs. This distinguishes better pruning from faster work on the same candidates.
The reported input-nodes/s denominator includes skipped nodes, so a high rate
does not mean all those nodes were examined.

[iteration-optimization-findings.md](iteration-optimization-findings.md) records
earlier overlap and iterator measurements, including sensitivity to code layout.
Those results do not establish performance for every current relation. Compare
construction plus consumption, keep byte/point and count/enumeration workloads
separate, and check both narrow and broad ranges before retaining an optimization.
