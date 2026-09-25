# Generic forests

Step 2 of 3: [side data](side-data.md) → forests →
[injections](injections-design.md). Assume step 1 is implemented, including
optional materialized points and independently owned side data.

Decision draft for `crates/squatter`, not implemented API. Rust excerpts show
proposed types and signatures; routine constructors and errors are omitted.
Prototype formats remain at version 0, with no migration support.

A forest owns zero or more independent packed trees in caller-supplied order.
Each region is a contiguous run of trees sharing an exact grammar; the same
grammar may occur in several regions.
Every core slab uses the forest representation, including single-tree slabs.
It has no language resolver, discovery policy, logical layer graph, host tree,
or application query configuration. Callers can use it without the third step.

## Packing and tree views

```rust
pub struct Forest {
    data: Box<TreeData>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeId(u32);   // local to one forest
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionId(u32); // local to one forest
#[derive(Clone, Copy)]
pub struct Tree<'forest> {
    forest: &'forest TreeData,
    id: TreeId,
}
#[derive(Clone, Copy)]
pub struct ForestRegion<'forest> {
    forest: &'forest TreeData,
    id: RegionId,
}

pub struct PackInput<'tree> {
    pub grammar: &'tree Grammar,
    pub root: tree_sitter::Node<'tree>,
}

impl PackContext {
    pub fn pack_forest(
        &mut self,
        inputs: &[PackInput<'_>],
        options: PackOptions,
        cancel: Option<&AtomicBool>,
    ) -> Result<(Forest, Vec<TreeId>), ForestError>;
}

pub enum ForestError {
    Cancelled,
    Core(Error),
}

impl Forest {
    pub fn tree(&self, id: TreeId) -> Option<Tree<'_>>;
    pub fn trees(&self) -> impl Iterator<Item = Tree<'_>>;
    pub fn regions(&self) -> impl Iterator<Item = ForestRegion<'_>>;
    pub fn has_points(&self) -> bool;
}

impl<'forest> ForestRegion<'forest> {
    pub fn id(&self) -> RegionId;
    pub fn grammar(&self) -> &'forest Grammar;
    pub fn trees(&self) -> impl Iterator<Item = Tree<'forest>>;
}

impl<'forest> Tree<'forest> {
    pub fn id(self) -> TreeId;
    pub fn grammar(self) -> &'forest Grammar;
    pub fn root_node(self) -> Node<'forest>;
    pub fn has_points(self) -> bool;
}
```

Packing preserves input order; the returned vector maps that order to physical
tree IDs. Empty input is valid. Adjacent inputs with the same exact grammar
share a region; a grammar change starts another region. Each tree gets a
group-aligned interval. Region boundaries and IDs are deterministic for the same
ordered inputs and grammar bindings. Grouping never combines native parser inputs.

The caller may group inputs by grammar or sort them before packing, but neither
is required. Discovery can append trees as parsing completes, including when
grammars recur through injection nesting. Direct-parser output can be encoded
into the forest after each parse without retaining all parses for later grammar
grouping. This does not require the parser itself to stream individual nodes.
Source-order indexes can be built separately from physical packing order; no
byte-order sorting or index is required by the initial forest representation.

Each input packs the supplied node and its descendants as an independent tree;
the node need not be a whole-tree root. Preserve its displayed kind/alias and
subtree contents. Its packed root has no parent, siblings, parent field, or
supertype context inherited from excluded ancestors. Relationships and supertype
context within the subtree remain intact. Queries on the detached tree need not
match queries that depended on its original ancestors.

`PackOptions::symbol_presence` requests one completed cache containing every
region's presence data; `points` requests completed point data for the forest.
Both default to true as in step 1. Presence and points each use their own
allocation, even when filled during forest packing. Failure to construct
requested side data fails the operation. These flags do not change core grouping,
IDs, or serialized bytes.

Packing preserves the supplied node's coordinate frame. The core stores its
byte coordinates; requested point data preserves its native point coordinates.
Callers place relative trees before packing, for example with
`root_node_with_offset(origin_byte, origin_point)`, and check that translation
before constructing the positioned node. There is no separate `byte_origin`
parameter or additional placement during node access. Nodes can span gaps in
native included ranges; the forest does not retain parser requests. Trees may
come from unrelated sources and use independent byte and point coordinate frames,
including within one region. Neither core storage nor point data requires a
shared source or a forest-wide coordinate frame.

Tree/region IDs identify descriptors only within their owner. Physical node IDs
identify slots, including group waste; wasted slots do not produce nodes. None of
these IDs is stable across rebuilding/reordering. The caller chooses a main tree
if its application has one; physical order makes no tree the document root.

```rust
// placement checked before constructing the positioned root
let positioned_second = second.root_node_with_offset(second_origin, second_point);
let third_subtree = third.root_node().named_child(0).unwrap();
let inputs = [
    PackInput { grammar: &grammar_a, root: first.root_node() },
    PackInput { grammar: &grammar_b, root: positioned_second },
    PackInput { grammar: &grammar_a, root: third_subtree },
];
let options = PackOptions {
    symbol_presence: false,
    points: false,
    ..PackOptions::default()
};
let (forest, input_trees) = packer.pack_forest(&inputs, options, None)?;
let second_root = forest.tree(input_trees[1]).unwrap().root_node();
assert_eq!(second_root.start_byte(), positioned_second.start_byte());
```

Retain convenience packing for a single native tree, returning a one-tree
`Forest`. `Tree<'forest>` replaces the standalone owning `Tree` from step 1; it
always borrows one tree. There is one ownership implementation and no separate
single-tree slab format. Serialization and side-data attachment belong to
`Forest`. A borrowed tree's nodes outlive the temporary handle, up to the
lifetime of its forest borrow.

## Packing bounds

Check grammar compatibility and coordinate bounds once per `PackInput`. Trust
Tree-sitter's subtree containment; do not add a coordinate-validation pass or
repeat these checks for every descendant. Packing does not verify coordinates
against source text or prove that byte and point positions correspond.

For bytes, compute the supplied node's start plus its native subtree byte size
using checked or widened arithmetic, and require the result to fit `u32`.
Do not use `root.end_byte()` as the overflow check: that accessor already adds
the size in native-width arithmetic. The checked input bound then covers byte
arithmetic throughout traversal. Placement that wrapped or truncated before the
node reached packing cannot reliably be detected; checking that earlier
translation belongs to the caller.

When copying native points, check the supplied start row plus native subtree row
extent once per input. The ending column does not bound columns on earlier
lines. A conservative column bound is the supplied start column plus subtree
byte size, computed with checked or widened arithmetic and required to fit
`u32`. This can reject representable multiline inputs near the column limit;
accept that conservatism rather than adding per-node overflow checks. Later
lines retain Tree-sitter's valid columns without the initial column translation.
Skip native point checks when points are not requested. Explicit point building
from `LineIndex` uses source-derived coordinates instead.

Check physical slot limits when opening/reserving each group, including all
waste slots, so per-node slot increments and span subtraction stay within the
established bounds. Input node counts alone do not bound physical slots because
packing can close partially filled groups. Capacity growth also checks group
counts, column offsets, and allocation sizes. Per-node delta-fit checks remain
ordinary encoding decisions: a value that does not fit closes the current group
and is retried in a new one, without repeated coordinate validation.

## Storage ownership and read paths

Keep allocation ownership separate from resolved reader addresses. Private
fields below are illustrative; they do not prescribe allocation coalescing or
the serialized descriptor layout.

```rust
use smallvec::SmallVec;

enum Storage {
    Owned(AlignedAllocation),
    Backed(Box<dyn StableSlab>),
}

struct TreeData {
    columns: Layout<ColumnPointer>,
    bytes: NonNull<u8>,
    byte_length: usize,
    trees: SmallVec<[TreeDescriptor; 1]>,
    regions: SmallVec<[RegionData; 1]>,
    presence_cache: Option<PresenceCache>,
    point_data: Option<PointData>,
    storage: Storage,
}

struct RegionData {
    descriptor: RegionDescriptor,
    grammar: Grammar,
    presence: Option<NonNull<u8>>, // bitmap segment in the forest cache
}

impl Forest {
    pub fn as_bytes(&self) -> &[u8];
    pub fn from_bytes(grammars: &[Grammar], bytes: &[u8]) -> Result<Self, Error>;
    pub fn from_backing(
        grammars: &[Grammar],
        backing: impl StableSlab,
    ) -> Result<Self, Error>;
}
```

Packing and `from_bytes` allocate core storage; `from_backing` retains immutable
storage with the existing `StableSlab` contract. Both return `Forest`, with no
public `BackedForest` variant. Resolve column addresses and populate the tree and
region vectors at construction. A single-tree forest keeps both vectors inline
in `TreeData`, without separate descriptor-vector allocations. Larger forests
spill to heap storage. Nodes index these vectors; `as_bytes()` uses the cached
base and length. Neither path matches on `Storage` or calls `StableSlab::bytes()`.
Side-data readers likewise cache their payload addresses so backing selection
stays out of point and presence access.

The enum occupies owner metadata and is used during construction, destruction,
and explicitly storage-dependent operations. Retained backing can require a
box and dynamic destruction, but adds no per-node dispatch or pointer hop.
Reader addresses remain valid and immutable while borrowed. Packing may relocate
storage only before publication and must refresh resolved addresses afterward.
Side-data replacement refreshes its reader metadata under exclusive forest
access. Moving `Forest` does not move `TreeData` or its retained storage.

Externally borrowed bytes require a separate lifetime-bearing wrapper if that
loading API is retained; they cannot enter `Forest` through `StableSlab` without
a retained owner. This does not require another tree representation.

## Nodes and tree lookup

```rust
#[derive(Clone, Copy)]
pub struct Node<'forest> {
    forest: &'forest TreeData,
    tree: TreeId,
    slot: SlotIx, // physical slot within the forest
}

pub struct Cursor<'forest> {
    node: Node<'forest>,
    // traversal state
}

pub struct QueryCursor {
    // reusable scratch, independent of any forest
}

pub struct QueryExecution<'cursor, 'forest> {
    cursor: &'cursor mut QueryCursor,
    root: Node<'forest>,
    // query, source, and matching state
}

impl<'forest> Node<'forest> {
    pub fn walk(self) -> Result<Cursor<'forest>, Error>;
}

impl<'forest> Cursor<'forest> {
    pub fn node(&self) -> Node<'forest>;
}
```

Column access follows `Node → TreeData.columns → column bytes`, without going
through a `Tree` handle or tree descriptor. A forest reference and two 32-bit
indices can fit in 16 bytes on a 64-bit target; verify the implemented layout.
Node equality and hashing use forest identity and physical slot. Construction
ensures the slot is live and belongs to the carried tree ID.

Tree-dependent operations index `TreeData::trees` by `TreeId` for group bounds
and `RegionId`, then index `TreeData::regions` for grammar and presence metadata.
No containing-tree search by slot is needed. Nodes, cursors, scans, and query
executions use these same lookups rather than retaining a separate resolved
context or descriptor references. Returned nodes and captures retain the compact
forest/tree/slot representation. The reusable query cursor retains no forest
borrow between executions. Column pointers are stored once in `TreeData`.

## Ownership, navigation, and queries

Forest owns all columns, retained grammar handles, and attached side data. Views,
nodes, cursors, and scans borrow the owner; no view frees or retains a slab on its
own. Sidecar handles live in the owner descriptor, outside the core slab.
The forest presence cache and point data have independently loadable allocations.
Set/drop requires exclusive access to the forest and never shifts or rewrites
its core columns, descriptor tables, groups, or IDs. Built/copied sidecars never
share an allocation with the core or another sidecar. Mapped sidecars retain
backing owners as in step 1. Region presence views borrow slices of the forest
cache and do not own allocations. Dropping the presence cache clears those views
and frees its single allocation or releases its backing handle, independently
of point data. Presence attachment and removal operate on the whole forest.

```rust
impl TreeLike for Tree<'_> {
    type Node<'tree> = Node<'tree> where Self: 'tree;

    fn root(&self) -> Node<'_>;
}

// existing APIs operate on a forest node without a second query framework
let root = forest.tree(input_trees[0]).unwrap().root_node();
let nodes = root.preorder();
let execution = cursor.execute(&query, root, bytes);
```

Node traversal, child/sibling/parent navigation, scan iterators, structural
matching, traversal depth, anchors, and pending-match state never cross a tree
boundary. Root parent and sibling navigation terminate there. Node identity and
equality must distinguish independent trees even when their bytes overlap.

Start with existing per-tree query cursors behind the region iterator. The caller
selects a query for each tree and decides how to order results. Equal grammars
do not imply equal application query configurations. Skip unqueried regions and
unselected trees; a tree requiring fallback must not disable fast execution for
unrelated trees. Region iteration follows physical input order, which need not
be source order; a grammar may appear more than once.

Forests do not retain or prove source provenance. Source is needed for explicit
point-data construction and text queries, not node access. The caller selects
the source and byte/point query bounds appropriate to each tree. Equal byte or
point values in different trees need not identify the same source position;
cross-tree source ordering and range indexes require caller-supplied context.
Point accessors and point-bounded queries use that tree's attached coordinates
or its row-zero frame from step 1. `has_points()` reports availability, not a
common source or document frame. Query source bytes do not supply missing points.

## Extend side data to forests

Generalize step 1's builders and set/drop methods to whole-forest presence and
points, using the same owned side-data types. Single-tree forests use these
same APIs:

```rust
impl PresenceCache {
    pub fn build_forest(
        forest: &Forest,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, SideDataError>;

    pub fn from_forest_backing(
        forest: &Forest,
        backing: impl StableSlab,
    ) -> Result<Self, SideDataError>;

    pub fn copy_from_forest_bytes(
        forest: &Forest,
        bytes: &[u8],
    ) -> Result<Self, SideDataError>;
}

impl PointData {
    pub fn build_forest(
        forest: &Forest,
        sources: &[&LineIndex],
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, SideDataError>;

    pub fn from_forest_backing(
        forest: &Forest,
        backing: impl StableSlab,
    ) -> Result<Self, SideDataError>;

    pub fn copy_from_forest_bytes(
        forest: &Forest,
        bytes: &[u8],
    ) -> Result<Self, SideDataError>;
}

impl Forest {
    pub fn set_presence_cache(&mut self, cache: PresenceCache) -> Result<(), SideDataError>;
    pub fn set_point_data(&mut self, points: PointData) -> Result<(), SideDataError>;
    pub fn drop_presence_cache(&mut self);
    pub fn drop_point_data(&mut self);
}
```

The forest presence cache concatenates the serialized region caches in region
order into one allocation. Each segment retains the existing region header and
bitmap layout, including alignment; no outer header or offset table is needed.
A one-region forest uses the same bytes as a standalone region cache. An empty
forest has an empty concatenation.

```text
presence cache
  [region 0 header + bitmaps][region 1 header + bitmaps][region 2 header + bitmaps]
```

Each segment's size follows from its group count and grammar symbol count.
Construction computes the checked total size, allocates once, and fills each
segment directly. Loading walks the expected regions, checks each header,
dimensions, and segment extent, and requires the concatenation to consume the
input exactly. Reject truncated, extra, or incompatible segments. Resolve region
payload pointers once on attachment; reads do not walk earlier segments.
Mapped loading retains one backing owner; copied loading uses one aligned
allocation and copies the concatenation without rebuilding bitmaps.

Bitmaps have one bit per physical group within their region; tree views use a
region-relative group offset. Regions with the same grammar share grammar
handles and prepared tables but have separate bitmap segments. All segments
are attached or dropped together. Without a forest presence cache, every region
uses ordinary symbol scanning.

Point data covers the entire forest, preserving a separate coordinate frame for
each tree. Sources need not match, even within a region. Ignore wasted slots.
Attachment is all-or-nothing; `Forest::has_points()`, `Tree::has_points()`,
and its nodes report the same availability. Without point data they all use
row-zero access, including forests containing trees from unrelated sources.

Packing copies points from each native input. For later source-derived
construction, `build_forest` takes one `LineIndex` reference per tree in
`forest.trees()` order; reject a source-count mismatch. Each source must match
its tree's byte coordinate frame. Entries may reference different sources or
reuse one source and line index for multiple trees. The resulting sidecar is
one allocation indexed by physical slots and retains no source references.

The caller supplies point data for the matching forest and each tree's intended
source/frame. Point access performs no source lookup and needs no retained source
bytes. Independent coordinate frames do not require separate point allocations
or owners. Per-tree point attachment/removal remains outside this interface.

Side data uses the `as_bytes` representation from step 1. The backing constructors
read mapped payloads directly and retain their owners; the copy constructors
allocate aligned storage and memcpy the same layout. Neither path decodes fields
into another representation or reconstructs indexes. Release loading and
attachment only check target kind, region counts, dimensions, alignment, and
payload sizes. Content scans run only in debug builds, as in step 1. The caller
supplies data built for the matching forest and region order; count checks alone
do not prove that pairing. Reordering trees/groups while rebuilding a core
requires fresh side data or a correct remapping. Loading or setting side data
does not rebuild the core. Failed attachment leaves current side data unchanged.
Workers can build through immutable forest borrows; completed values retain no
borrow. Set/drop requires exclusive owner access after those borrows end.
Set replaces existing data on success; drop frees owned storage or
releases the mapped backing handle, returns nothing, and is a no-op when absent.
Point attachment/removal preserves layout and IDs but switches each tree between
its attached point coordinates and row-zero access; presence attachment/removal
preserves query results.

## Representation and serialization

Symbol and grammar-symbol columns share one width across the slab, chosen from
all participating grammars before packing: one byte if every grammar has at most
254 symbols and aliases, otherwise two bytes. This includes both remapped error
IDs. The grammar-symbol column is first in the optional tail and is omitted only
when every emitted node has equal symbol and grammar-symbol IDs. Compressed
byte coordinates remain in shared columns, in each tree's supplied frame;
points and presence remain outside the slab.

```text
forest
  region 0, grammar A: [tree 0 groups]
  region 1, grammar B: [tree 1 groups]
  region 2, grammar A: [tree 2 groups][tree 3 groups]
```

These intervals are independent trees, not child relationships. Proposed private
descriptor fields, not public APIs or a frozen ABI:

```rust
struct ForestHeader {
    // version/configuration, table counts/offsets, group count,
    // column and auxiliary locations
}

struct RegionDescriptor {
    grammar_index: u32,
    first_tree: u32,
    tree_count: u32,
}

struct TreeDescriptor {
    first_group: u32,
    group_count: u32,
    region: RegionId,
}

impl Forest {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error>;
}
```

Region group bounds follow from its first and last trees. Tree node bounds follow
from group size; the final occupied slot is the root under reverse-preorder
encoding. Avoid redundant root/boundary tables unless measurements justify them.
Store the region ID in each tree descriptor for direct grammar lookup. It must
agree with the region's tree interval. Loading decodes the serialized descriptors
into `TreeData::trees` and `TreeData::regions`; `SmallVec` internals, grammar
handles, and runtime reader pointers are never serialized.
Multiple regions may use the same grammar index. Region grouping constrains
grammar interpretation only, not source identity or coordinate order.

Serialize fields explicitly in little-endian form. Release loading performs only
cheap header/count/size checks, including checked arithmetic for table and column
extents. Populating runtime vectors requires reading descriptor metadata, not a
full content-validation pass. Do not scan nodes, indexes, or coordinates for
validity, or add a separate descriptor-validation pass in release builds.

Under `#[cfg(debug_assertions)]`, scan descriptors and contents: check alignment,
offsets, column/index/coordinate bounds, and grammar references. Regions partition
the tree table; trees partition used groups; topology stays inside each tree.
Check tree-to-region IDs against the region intervals. These remain
representation invariants; release loading does not revalidate them
by scanning the stored contents.
An empty forest has no tree/region intervals. Loading retains supplied grammar
handles; each grammar index selects a caller-supplied grammar. The caller must
supply the matching grammars; persistent compatibility checks are separate work.

Symbols and fields remain grammar-local despite uniform widths. Prepared grammar
tables and supertype dictionaries can be shared per exact grammar; per-node
supertype encodings still require that grammar's interpretation. Grammar-symbol
overrides need tree/region scope and index relocation when copied. They remain
authoritative data.

Serialization contains the core alone; side-data loading and attachment are separate.
The core serializer requires no LMDB or application manifest. Copies and retained
backings use the same core layout and reader metadata. Integration with pinned
LMDB transactions remains persistence work; it supplies a `StableSlab` owner
without introducing another forest owning type.

## Implementation and verification

1. Add forest descriptors, adjacent exact-grammar runs, and input-to-tree mapping;
   accept native subtree nodes with per-input coordinate and per-group slot bounds.
2. Add compact forest/tree/slot nodes with descriptor lookups through `SmallVec`s
   with inline capacity one for trees and regions.
3. Add concatenated forest presence caches and point data with independent
   per-tree sources.
4. Add checked serialization, retained grammar bindings, and storage-independent
   read paths for allocated and retained backing storage.

Verify empty, single-tree, mixed-grammar, and repeated-grammar inputs without any
discovery engine. Compare each packed tree with its native and one-tree forest
counterparts, including overlapping bytes, positioned roots, arbitrary subtrees,
errors, predicates, point bounds, and ties. Verify aliases and subtree contents
survive detachment while excluded parent/sibling/field/supertype context does not.
Exercise every root/tree boundary, wasted slots, input mapping, changed grammar
bindings, serialization round trips, malformed descriptors, overflow,
cancelled packing, and existing-reader lifetimes. Verify absent forest presence
caches, invalid region/dimension rejection, and sidecar serialization
round trips. Presence changes must preserve query results. Points must
match each tree's source conversion while attached and use row-zero coordinates
before attachment and after removal; test point-bounded queries in both states.
Byte-based matching and core contents must remain unchanged. Setting/replacing/
dropping independently built or loaded sidecars must preserve the core allocation
address, serialized bytes, descriptor offsets, groups, and IDs. Test creation
flags and immediate reclamation of each sidecar while the forest remains alive.
Check malformed counts/sizes in release and debug builds, and malformed contents
with matching counts in debug builds. Release load paths must contain no content
validation scan.

Verify input-order preservation for grouped, interleaved, and byte-unsorted
inputs, including an A/B/A/A grammar sequence producing three regions. Exercise
separate bitmap segments sharing one grammar. Pack trees from different sources
with overlapping byte and point ranges, including within the same region. Check
native point copying and rebuilding with distinct or repeated source references,
source-count rejection, per-tree text queries, and sidecar round trips without
source retention or a common coordinate-frame requirement.

Exercise input byte and row bounds, conservative column rejection for multiline
inputs, and point-free packing without native point checks. Test slot exhaustion
at group reservation, including partial-group waste, and layout overflow during
capacity growth. Coordinate checks must stay outside descendant traversal;
physical slot-limit checks belong at group reservation.

Verify node identity for overlapping trees and tree/region vector lookups in
nodes, cursors, scans, and queries at every tree boundary. Check inline storage
for empty and single-tree forests and spilled storage for larger forests,
including multiple trees in one region. Reuse query cursors across forests and
trees without retaining stale state. Check compact node layout. Exercise owned
and retained backing loads, moves of the forest owner, and backing release after
the last owner is dropped. Check that core and side-data reads use cached
addresses without backing dispatch and that side-data replacement cannot leave
stale reader pointers.

Verify presence serialization is exactly the concatenation of region encodings,
including empty, single-region, repeated-grammar, and differently sized regions.
Check one allocation for the owned bitmap payload and one backing owner for
mapped payloads. Exercise truncated segments, trailing bytes, extent overflow,
and whole-cache replacement/removal without stale region views. Release loading
may walk region headers but must not scan bitmap contents.

Later, compare per-tree/segmented queries with shared candidate scanning and
contiguous reassembly. Include viewport selection, predicates, merging, copying,
validation, and index construction in measurements. Candidate scanning may cross
tree boundaries; structural matching may not. Batching, result merging, and
placement/relocation APIs remain outside the initial forest interface.
