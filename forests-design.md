# Generic forests

Step 2 of 3: [side data](side-data.md) → forests →
[injections](injections-design.md). Assume step 1 is implemented, including
optional materialized points and independently owned side data. Also assume
[API alignment](../main/api-differences-to-fix.md) and the
[query revamp](../main/query-revamp.md) are implemented. Forests extend their
navigation and selection APIs and inherit their resolved query contracts.

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
    data: Box<ForestData>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct TreeIx(u32);

impl TreeIx {
    pub const fn from_raw(value: u32) -> Self;
    pub const fn get_raw(self) -> u32;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionIx(u32);

impl RegionIx {
    pub const fn get_raw(self) -> u32;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Tree<'forest>(Node<'forest>);

#[derive(Clone, Copy)]
pub struct ForestRegion<'forest> {
    forest: &'forest ForestData,
    index: RegionIx,
}

pub struct PackRegion<'tree> {
    pub language: Language,
    pub roots: Vec<tree_sitter::Node<'tree>>,
}

impl Packer {
    pub fn pack_forest(
        &mut self,
        inputs: Vec<PackRegion<'_>>,
        options: PackOptions,
    ) -> Result<(Forest, Vec<TreeIx>), Error>;
}

impl Forest {
    pub fn tree(&self, index: TreeIx) -> Option<Tree<'_>>;
    pub fn trees(&self) -> impl Iterator<Item = Tree<'_>>;
    pub fn regions(&self) -> impl Iterator<Item = ForestRegion<'_>>;
    pub fn has_points(&self) -> bool;
}

impl<'forest> ForestRegion<'forest> {
    pub fn index(&self) -> RegionIx;
    pub fn language(&self) -> &'forest Language;
    pub fn trees(&self) -> impl Iterator<Item = Tree<'forest>>;
}

impl<'forest> Tree<'forest> {
    pub fn language(&self) -> &'forest Language;
    pub fn root_node(&self) -> Node<'forest> {
        self.0
    }
    pub fn has_points(&self) -> bool;
    pub fn walk(&self) -> TreeCursor<'forest>;
}

impl<'forest> std::ops::Deref for Tree<'forest> {
    type Target = Node<'forest>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
```

Each input defines one region containing one or more roots with the same exact
grammar. Empty input is valid; an input with no roots is invalid. Adjacent inputs
remain separate regions even when their grammars match. Packing preserves region
and root order. The returned vector maps roots in flattened input order to
physical tree indices. Each root becomes an independent tree with a group-aligned
interval. Region boundaries and IDs are deterministic for the same ordered
inputs and grammar bindings. Grouping never combines native parser inputs.

The caller may group roots by grammar or sort them before packing, but neither
is required. Discovery can append one-root inputs as parsing completes,
including when grammars recur through injection nesting. Native trees must stay
alive until `pack_forest` returns because the inputs contain borrowed nodes.
Source-order indexes can be built separately from physical packing order; no
byte-order sorting or index is required by the initial forest representation.

`PackOptions::progress_callback` reports the current or last native byte offset
and returns `ControlFlow::Break(())` to cancel. Offsets can decrease when packing
moves to another tree and do not measure forest-wide work completed. Poll between
roots and regions as well as during traversal; an empty forest polls once with
offset zero. Cancellation returns `Error::Canceled` without publishing a
partial forest.

Each root packs the supplied node and its descendants as an independent tree;
the node need not be a whole-tree root. Preserve its displayed kind/alias and
subtree contents. Its packed root has no parent, siblings, parent field, or
supertype context inherited from excluded ancestors. Relationships and supertype
context within the subtree remain intact. Queries on the detached tree need not
match queries that depended on its original ancestors.

`PackOptions::symbol_presence` requests one completed cache containing every
region's presence data; `points` requests completed point data for the forest.
Both default to true as in step 1. Presence and points each use their own
allocation, even when filled during forest packing. Failure to construct
requested side data fails the operation. Points must be requested during packing: their
delta limits affect core grouping and physical IDs. Presence does not affect grouping.

Packing preserves the supplied node's coordinate frame. The core stores its
byte coordinates; requested point data preserves its native point coordinates.
Callers place relative trees before packing, for example with
`tree_sitter::Tree::root_node_with_offset(origin_byte, origin_point)`, and check
that translation before constructing the positioned node. There is no separate `byte_origin`
parameter or additional placement during node access. Packed trees do not expose
offset views. Nodes can span gaps in native included ranges; the forest does not retain parser requests. Trees may
come from unrelated sources and use independent byte and point coordinate frames,
including within one region. Neither core storage nor point data requires a
shared source or a forest-wide coordinate frame.

Tree indices and region indices identify runtime metadata only within their owner.
`SlotIx` addresses a physical slot within the forest, including group waste;
wasted slots do not produce nodes. `NodeId` combines a tree index and slot index
and is local to one forest. None of these indices or IDs is stable across
rebuilding/reordering. The caller chooses a main tree if its application has one;
physical order makes no tree the document root.

```rust
// placement checked before constructing the positioned root
let positioned_second = second.root_node_with_offset(second_origin, second_point);
let third_subtree = third.root_node().named_child(0).unwrap();
let inputs = vec![
    PackRegion { language: language_a.clone(), roots: vec![first.root_node()] },
    PackRegion { language: language_b, roots: vec![positioned_second] },
    PackRegion { language: language_a, roots: vec![third_subtree] },
];
let options = PackOptions {
    symbol_presence: false,
    points: false,
    ..PackOptions::default()
};
let (forest, input_trees) = packer.pack_forest(inputs, options)?;
let second_root = forest.tree(input_trees[1]).unwrap().root_node();
assert_eq!(second_root.start_byte(), positioned_second.start_byte());
```

Retain convenience packing for a single native tree, returning a one-tree
`Forest`. `Tree<'forest>` replaces the standalone owning `Tree` from step 1; it
always borrows one tree. There is one ownership implementation and no separate
single-tree slab format. Serialization and side-data attachment belong to
`Forest`. A borrowed tree's nodes outlive the temporary handle, up to the
lifetime of its forest borrow.

The `Tree` wrapper provides the same read-only `TreeLike` entry point as
`tree_sitter::Tree`, so generic client code can specialize for either backend.
It contains the root `Node` directly and dereferences to it, exposing all node
operations. `root_node()` returns that node; `language()` and `has_points()`
delegate to it, as does `walk()`. There is no separate `Tree::index()` method:
`tree.id().tree()` obtains the tree index through the root node. The wrapper adds
no allocation or indirection to node access. Its private field preserves the
root-node invariant.

Preserve the aligned node and cursor APIs, including `&self` receivers on shared
operations, infallible `walk()`, and cursor-taking child enumeration. Child lookup
and counts use `ChildIx` or `NamedChildIx`; child iterators remain plain iterators
without an initial counting pass. Native input nodes retain Tree-sitter's
primitive child indices, as in the packing example above.

## Packing bounds

Check grammar compatibility and coordinate bounds once per root. Trust
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
extent once per root. The ending column does not bound columns on earlier
lines. A conservative column bound is the supplied start column plus subtree
byte size, computed with checked or widened arithmetic and required to fit
`u32`. This can reject representable multiline inputs near the column limit;
accept that conservatism rather than adding per-node overflow checks. Later
lines retain Tree-sitter's valid columns without the initial column translation.
Skip native point checks when points are not requested.

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
    Owned(Vec<u64>),
    Retained(Box<dyn Send + Sync>),
}

struct ForestData {
    storage: Storage,
    columns: Layout<ColumnPointer>,
    bytes: NonNull<u8>,
    byte_length: usize,
    trees: SmallVec<[TreeData; 1]>,
    regions: SmallVec<[RegionData; 1]>,
    presence_cache: Option<PresenceCache>,
    point_data: Option<PointsData>,
}

struct RegionData {
    slots: Range<SlotIx>,
    trees: Range<TreeIx>,
    language: Language,
    presence: Option<NonNull<u8>>, // bitmap segment in the forest cache
}

struct TreeData {
    region: RegionIx,
    slots: Range<SlotIx>,
}

impl Forest {
    pub fn as_bytes(&self) -> &[u8];
    pub fn from_bytes(languages: &[Language], bytes: &[u8]) -> Result<Self, Error>;
    pub fn from_retained(
        languages: &[Language],
        owner: impl StableSlab,
    ) -> Result<Self, Error>;
}
```

Packing and `from_bytes` use `Owned(Vec<u64>)` for core storage. The vector manages
allocation, capacity, and destruction; initialized words are exposed as bytes.
Capacity may exceed the logical slab length. Verify that `u64` alignment meets
the slab's alignment requirement on supported targets.

`from_retained` accepts `impl StableSlab` to establish that its bytes remain valid,
stable, and immutable.
Box the concrete owner, obtain its bytes and cache their address and length,
then erase the owner to `Box<dyn Send + Sync>` in `Storage::Retained`. The stored
owner only keeps the bytes alive and is dropped with the storage; it exposes no
byte-access method.
Presence and point sidecars use the same construction and ownership pattern.

Both loading paths return `Forest`, with no public `RetainedForest` variant.
Resolve column addresses, decode region descriptors,
and reconstruct runtime tree metadata at construction. A single-tree forest
keeps both vectors inline in `ForestData`, without separate descriptor-vector
allocations. Larger forests spill to heap storage. Nodes index these vectors;
`as_bytes()` uses the cached base and length. Neither path matches on `Storage`
or calls `StableSlab::bytes()`.
Side-data readers likewise cache their payload addresses so storage selection
stays out of point and presence access.

Point reads index the cached payload base in the forest's `PointsData` directly
by global slot, without a region lookup. `RegionData` caches a presence pointer
because those bitmap segments use region-relative group indices; it does not
need a separate points pointer.

The enum occupies owner metadata and is used during construction, destruction,
and explicitly storage-dependent operations. Retained storage can require a
box and dynamic destruction, but adds no per-node dispatch or pointer hop.
Reader addresses remain valid and immutable while borrowed. Packing may relocate
storage only before publication and must refresh resolved addresses afterward.
Side-data replacement refreshes its reader metadata under exclusive forest
access. Moving `Forest` does not move `ForestData` or its retained storage.

Externally borrowed bytes require a separate lifetime-bearing wrapper if that
loading API is retained; they cannot enter `Forest` through `StableSlab` without
a retained owner. This does not require another tree representation.

## Nodes and tree lookup

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct NodeId(u64);

impl NodeId {
    pub const fn new(tree: TreeIx, slot: SlotIx) -> Self {
        Self(((tree.get_raw() as u64) << 32) | slot.get_raw() as u64)
    }

    pub const fn tree(self) -> TreeIx {
        TreeIx::from_raw((self.0 >> 32) as u32)
    }

    pub const fn slot(self) -> SlotIx {
        SlotIx::from_raw(self.0 as u32)
    }

    pub const fn get_raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy)]
pub struct Node<'forest> {
    forest: &'forest ForestData,
    id: NodeId,
}

pub struct TreeCursor<'forest> {
    node: Node<'forest>,
    // traversal state
}

pub struct QueryCursor {
    // reusable scratch, independent of any forest
}

impl<'forest> Node<'forest> {
    pub fn id(&self) -> NodeId;
    pub fn slot(self) -> SlotIx;
    pub fn walk(&self) -> TreeCursor<'forest>;
}

impl<'forest> TreeCursor<'forest> {
    pub fn node(&self) -> Node<'forest>;
}
```

Forest nodes expose the composite `NodeId` through both inherent `Node::id()`
and `NodeLike::id()` (`type Id = NodeId`). This supersedes the API alignment
plan's slot-only identity and omission of an inherent `id()` for standalone trees.
Keep `slot()` for physical addressing. The composite ID carries the tree context
needed for descriptor lookup and caller-owned source selection.

`NodeId` stores `TreeIx` in bits 63–32 and `SlotIx` in bits 31–0. The slot remains
forest-global, not relative to the tree. Constructing an ID only combines the
indices; constructing a node ensures the slot is live and belongs to that tree.
The ID contains no forest identity or lifetime. Node equality and hashing use
both forest identity and `NodeId`.

Column access follows `Node → ForestData.columns → column bytes`, indexed by
`id.slot()`, without going through a `Tree` handle or tree descriptor. A forest
reference and a 64-bit ID can fit in 16 bytes on a 64-bit target; verify the
implemented layout.

Tree-dependent operations index `ForestData::trees` by `id.tree()` for slot bounds
and `RegionIx`, then index `ForestData::regions` for grammar and presence metadata.
No containing-tree search by slot is needed. Nodes, cursors, scans, and query
executions use these same lookups rather than retaining a separate resolved
context or descriptor references. Returned nodes and captures retain the compact
forest/ID representation. Cursor, scan, and query state carry nodes or `NodeId`s
instead of storing separate tree and slot indices. The reusable query cursor
retains no forest borrow between executions. Column pointers are stored once in
`ForestData`.

## Ownership, navigation, and queries

Forest owns all columns, retained grammar handles, and attached side data. Views,
nodes, cursors, and scans borrow the owner; no view frees or retains a slab on its
own. Sidecar handles live in the owner descriptor, outside the core slab.
The forest presence cache and point data have independently loadable allocations.
Set/drop requires exclusive access to the forest and never shifts or rewrites
its core columns, descriptor tables, groups, or IDs. Built/copied sidecars never
share an allocation with the core or another sidecar. Mapped sidecars retain
storage owners as in step 1. Region presence views borrow slices of the forest
cache and do not own allocations. Dropping the presence cache clears those views
and frees its single allocation or releases its storage owner, independently
of point data. Presence attachment and removal operate on the whole forest.

```rust
impl TreeLike for Tree<'_> {
    type Node<'tree> = Node<'tree> where Self: 'tree;

    fn root_node(&self) -> Node<'_>;
}

// existing APIs operate on a forest node without a second query framework
let root = forest.tree(input_trees[0]).unwrap().root_node();
let nodes = root.preorder();
let selected = root.all().overlapping_bytes(viewport);
let execution = cursor.execute(&query, &selected, text_provider);
```

Child/sibling/parent navigation, structural matching, traversal depth, anchors,
and pending structural state never cross a tree boundary. Root parent and sibling
navigation terminate there. Subtree scans stay within their scope; forest and
region scans may enumerate candidates across selected trees. Node identity and
equality distinguish independent trees even when their bytes overlap.

Extend `DescribeSelection` and `ScanSelection` with contiguous tree-range scopes
backed by forest metadata. Forest and region scopes describe their tree intervals;
subtree scopes continue to use a root node. Use the same `matches`, `captures`,
`execute`, and options variants for all supported scopes. Preserve generic scan
builders for direct scanning; normalize their descriptions once per execution
and prepare scan state outside the per-node matching loop. Descriptions borrow
filter storage and preserve repeated restrictions as intersections. They describe
selections, not partially consumed iterators.

All selected trees must use the query's exact language; reject incompatible
selections. A mixed-language forest requires caller-selected compatible scopes.
Equal grammars do not imply equal application query configurations. Arbitrary
tree sets and unions of overlapping subtrees remain deferred. Region iteration
follows physical input order, which need not be source order; a grammar may
appear more than once.

Initially process a tree-range selection one tree at a time using the existing
matcher. Shared candidate scanning is a later optimization, not a prerequisite
for multi-tree selection. Skip unqueried regions and unselected trees; a tree
requiring fallback must not disable fast execution for unrelated trees. Preserve
execution-wide match identity and removal behavior across tree transitions.

Candidate restrictions select eligible query-start nodes. Structural matching
may inspect other nodes within the same subtree or tree scope, including children
excluded by candidate filters. Inherit the revamp's start-node rules for supported
rootless and sibling-sequence patterns, range composition, limits, and callback
cancellation/resumption. Scan restrictions do not become whole-match containment
or Tree-sitter query-range semantics. Preserve the revamp's capture order,
provisional snapshots, duplicate behavior, and completed-capture coverage; add no
cross-tree source-order guarantee. Result merging remains caller-owned.

Forests do not retain or prove source provenance. Point data comes from parser
coordinates during packing, not from a later source lookup. Text queries require
caller-supplied source bytes. The caller selects
the source and byte/point query bounds appropriate to each tree. Equal byte or
point values in different trees need not identify the same source position;
cross-tree source ordering and range indexes require caller-supplied context.
Point accessors and point-bounded queries use that tree's attached coordinates
or its row-zero frame from step 1. `has_points()` reports availability, not a
common source or document frame. Query source bytes do not supply missing points.

`TextProvider` documentation must explain source disambiguation: obtain `TreeIx`
from `node.id().tree()` and use it to select the caller-owned source for that tree.
The index is local to the node's forest. A byte-slice provider suffices only when
that slice matches every selected tree's byte coordinate frame. Providers retain
responsibility for resolving node coordinates into their source or text chunks;
the forest retains neither the provider nor source bytes after execution.

Byte and point restrictions apply in each selected tree's coordinate frame.
Caller-owned source indexes may select contiguous tree intervals before applying
viewport restrictions. Pruning must account for tree ends when ranges overlap
or nest; region membership and sorted starts alone do not establish safe pruning.

## Extend side data to forests

Generalize step 1's presence builder and side-data set/drop methods to forests,
using the same owned side-data types. Single-tree forests use these same APIs:

```rust
impl PresenceCache {
    pub fn build(forest: &Forest) -> Result<Self, SideDataError>;

    pub fn from_retained(owner: impl StableSlab) -> Result<Self, SideDataError>;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SideDataError>;
    pub fn as_bytes(&self) -> &[u8];
}

impl PointsData {
    pub fn from_retained(owner: impl StableSlab) -> Result<Self, SideDataError>;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SideDataError>;
    pub fn as_bytes(&self) -> &[u8];
}

impl Forest {
    pub fn set_presence_cache(&mut self, cache: PresenceCache) -> Result<(), SideDataError>;
    pub fn set_point_data(&mut self, points: PointsData) -> Result<(), SideDataError>;
    pub fn drop_presence_cache(&mut self);
    pub fn drop_point_data(&mut self);
}
```

`build` derives new data from an immutable forest borrow. `from_bytes` copies
serialized data into owned storage, matching `Forest::from_bytes`;
`from_retained` retains immutable storage without copying. Both loaders validate
the serialized format without a forest. Setters check compatibility with the
destination forest and perform forest-dependent debug validation before attachment.

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
segment directly. Loading walks the serialized segments, checks each header,
dimensions, and segment extent, and requires the concatenation to consume the
input exactly. Attachment checks segment counts and dimensions against the forest's
regions, rejecting extra, missing, or incompatible segments. Resolve region payload
pointers once on attachment; reads do not walk earlier segments.
Mapped loading retains one storage owner; copied loading uses one aligned
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

Packing copies points from each native input. Group boundaries must account for
point deltas. The resulting sidecar is one allocation indexed by physical slots
and retains no source references.

The caller supplies persisted point data for the exact matching forest and each tree's intended
source/frame. Point access performs no source lookup and needs no retained source
bytes. Independent coordinate frames do not require separate point allocations
or owners. Per-tree point attachment/removal remains outside this interface.

Side data uses the `as_bytes` representation from step 1. Retained constructors
read mapped payloads directly and retain their owners; copy constructors
allocate aligned storage and memcpy the same layout. Neither path decodes fields
into another representation or reconstructs indexes. Release loading checks format,
dimensions, alignment for retained storage, and payload sizes. Attachment compares
the target kind, region counts, and dimensions with the forest, and checks
point-delta overflow. Other content checks run only in debug builds. The caller supplies data built for the matching
forest and region order; count checks alone
do not prove that pairing. Reordering trees/groups while rebuilding a core
requires fresh side data or a correct remapping. Loading or setting side data
does not rebuild the core. Failed attachment leaves current side data unchanged.
Workers can build presence caches through immutable forest borrows while other readers remain
active; completed values are `Send` and retain no forest or source borrow.
Set/drop requires exclusive owner access after all reader borrows end, including
nodes, tree views, and cursors that would otherwise be used after attachment.
Set replaces existing data on success; drop frees owned storage or
releases the mapped storage owner, returns nothing, and is a no-op when absent.
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
    // version/configuration, region count/table offset, group count,
    // column and auxiliary locations
}

struct RegionDescriptor {
    grammar_index: u32,
    end_slot: SlotIx,
}

impl Forest {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error>;
}
```

The slab contains only region descriptors; there is no serialized tree table.
Each descriptor stores its region's exclusive `end_slot`. The first region
starts at slot zero; later regions start at the preceding descriptor's end.
Loading expands these boundaries into `RegionData::slots`, a half-open range
including waste slots. No serialized descriptor is retained in `RegionData`.
Exclusive ends let loading carry the previous end forward while providing the
boundary needed to begin each region's backward tree walk.

Both endpoints of every runtime range are on group boundaries. Regions are
nonempty and partition the used groups in physical order; the final end equals
the forest's used slot count. An empty forest has no regions and zero used
groups. Group bounds follow by dividing the endpoints by `GROUP_SIZE`; group
count is their difference divided by `GROUP_SIZE`. The exclusive end must fit
`SlotIx`, which is included in the group-reservation limit. Runtime slot ranges
allow direct membership checks without storing redundant group bounds or counts.

Populate runtime `ForestData::trees` by walking each region from root to root.
Each root's stored physical-slot span gives its tree's extent, including internal
waste. Reverse-preorder encoding lets the walk proceed backward:

1. Start at the region's exclusive end. The last group's waste count identifies
   the final occupied slot, which is the last tree's root.
2. Subtract that root's subtree span from its slot to find the tree's start.
   Record `[start, end)` and the region index in `TreeData`.
3. Continue backward with `end = start` until reaching the region's start.
4. Reverse the recovered entries for that region so tree indices follow physical
   input order. Record their index range in `RegionData::trees`.

The walk reads the root slot span and final-group waste count once per tree,
then jumps over that tree's descendants and groups. Reconstruction takes
O(number of regions + number of trees), independent of descendant count.
Packing can record the same metadata as trees complete. Runtime
tree metadata, region tree-index ranges, `SmallVec` internals, grammar handles,
and reader pointers are never serialized. Multiple regions may use the same
grammar index; grouping constrains grammar interpretation only, not source
identity or coordinate order.

Serialize fields explicitly in little-endian form. Release loading checks
header/count/size arithmetic and region extents. During tree reconstruction,
check root accesses and span arithmetic, require tree starts within the region
and strict backward progress, and bound the resulting tree count by `TreeIx`.
These checks make reconstruction bounded without a full content scan.
Do not traverse descendants or scan bitmap contents or coordinates for validity
in release builds.

Under `#[cfg(debug_assertions)]`, scan descriptors and contents: check alignment,
offsets, column/index/coordinate bounds, and grammar references. Reconstructed
trees partition each region's groups; topology stays inside each tree. Check
that serialized region ends and reconstructed region/tree bounds are on group
boundaries. Release loading relies on this alignment invariant. Check strictly
increasing region ends, the final end against the used slot count, and runtime
tree-to-region mappings against the region intervals. Full topology and content
validation remains debug-only. Loading retains supplied grammar
handles; each grammar index selects a caller-supplied grammar. The caller must
supply the matching grammars; persistent compatibility checks are separate work.

Symbols and fields remain grammar-local despite uniform widths. Prepared grammar
tables and supertype dictionaries can be shared per exact grammar; per-node
supertype encodings still require that grammar's interpretation. Grammar-symbol
overrides need tree/region scope and index relocation when copied. They remain
authoritative data.

Serialization contains the core alone; side-data loading and attachment are separate.
The core serializer requires no LMDB or application manifest. Copied and retained
storage use the same core layout and reader metadata. Integration with pinned
LMDB transactions remains persistence work; it supplies a `StableSlab` owner
without introducing another forest owning type.

## Implementation and verification

1. Add forest descriptors, caller-defined regions, and flattened root-to-tree mapping;
   accept native subtree nodes with per-root coordinate and per-group slot bounds.
2. Add compact forest/`NodeId` nodes with descriptor lookups through `SmallVec`s
   with inline capacity one for trees and regions.
3. Add concatenated forest presence caches and point data with independent
   per-tree sources.
4. Add checked serialization, retained grammar bindings, and storage-independent
   read paths for allocated and retained storage. Serialize only region
   descriptors and reconstruct runtime tree metadata from root spans.
5. Extend scan selection and query execution to contiguous tree ranges, initially
   matching one tree at a time. Preserve the prerequisite navigation/query APIs,
   with composite node identity as the explicit forest extension.

Verify empty, single-tree, mixed-grammar, and repeated-grammar inputs without any
discovery engine. Compare each packed tree with its native and one-tree forest
counterparts, including overlapping bytes, positioned roots, arbitrary subtrees,
errors, predicates, point bounds, and ties. Verify aliases and subtree contents
survive detachment while excluded parent/sibling/field/supertype context does not.
Exercise every root/tree boundary, wasted slots, input mapping, changed grammar
bindings, serialization round trips, malformed descriptors, overflow,
cancellation during traversal, between roots, and on empty input, and
existing-reader lifetimes. Verify absent forest presence caches, invalid
region/dimension rejection, and sidecar serialization
round trips. Presence changes must preserve query results. Points must
match each tree's source conversion while attached and use row-zero coordinates
before attachment and after removal; test point-bounded queries in both states.
Byte-based matching and core contents must remain unchanged. Setting/replacing/
dropping independently built or loaded sidecars must preserve the core allocation
address, serialized bytes, descriptor offsets, groups, and IDs. Test creation
flags and immediate reclamation of each sidecar while the forest remains alive.
Check malformed counts/sizes in release and debug builds, and malformed contents
with matching counts in debug builds. Release load paths must contain no content
validation scan beyond point-delta overflow and the metadata and root-boundary
checks needed for loading.

Verify end-only region descriptors reconstruct the same runtime slot ranges,
tree bounds, region mappings, and tree-index order as packing. Cover empty and
single-region forests, several trees in one region, partial final groups, and
repeated grammars. Reject misaligned or nonincreasing region ends and a final end
that disagrees with the used slot count in debug builds. Reject invalid root
accesses, span arithmetic, nonprogressing reconstruction, and tree-count overflow
during loading without traversing descendants.

Verify input-order preservation for grouped, interleaved, and byte-unsorted
inputs, including A/B/A regions with two roots in the last region. Exercise
separate bitmap segments sharing one grammar. Pack trees from different sources
with overlapping byte and point ranges, including within the same region. Check
native point copying, per-tree text queries, and sidecar round trips without
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
and retained loads, moves of the forest owner, and storage release after
the last owner is dropped. Check that core and side-data reads use cached
addresses without storage dispatch and that side-data replacement cannot leave
stale reader pointers.

Verify `NodeId` composition and extraction, including the high bits of each
32-bit index. Equal IDs from different forests must not make their nodes equal;
node construction must reject wasted slots and mismatched tree/slot pairs.
Check that `NodeLike::id()` and inherent `id()` return the same composite identity
and that `slot()` agrees with its slot component.

Verify subtree, region, and contiguous tree-range selections, including empty
ranges and rejection of incompatible languages. Compare direct-scan candidate
sets with query candidates, including repeated filters, and verify structural
matching can inspect nodes outside the candidate set without leaving its scope.
Exercise borrowed filter lifetimes, viewport pruning with overlapping/nested
trees, and providers selecting distinct sources through `node.id().tree()`.
Cover match removal across tree transitions, cancellation during and between
trees, and subsequent cursor reuse. Preserve the revamp's resolved resumption,
limit, range, and capture contracts across tree transitions; do not add stricter
capture ordering or finite-limit result-subset parity requirements.

Verify presence serialization is exactly the concatenation of region encodings,
including empty, single-region, repeated-grammar, and differently sized regions.
Check one allocation for the owned bitmap payload and one storage owner for
mapped payloads. Exercise truncated segments, trailing bytes, extent overflow,
and whole-cache replacement/removal without stale region views. Release loading
may walk region headers but must not scan bitmap contents.
Load sidecars without a forest, then attach them to compatible and incompatible
forests; rejected attachment must preserve existing side data.

Later, compare per-tree/segmented queries with shared candidate scanning and
contiguous reassembly. Include viewport selection, predicates, merging, copying,
validation, and index construction in measurements. Candidate scanning may cross
tree boundaries; structural matching may not. Shared candidate-scan batching,
result merging, and placement/relocation APIs remain outside the initial forest
interface.
