# Generic forests

Step 2 of 3: [side data](side-data.md) → forests →
[injections](injections-design.md). Assume step 1 is implemented, including
optional materialized points and independently owned side data. Also assume
[API alignment](../main/api-differences-to-fix.md) and the
[query revamp](../main/query-revamp.md) are implemented. Forests extend their
navigation APIs and inherit their resolved query contracts, except that query
restrictions use Tree-sitter-style cursor setters rather than scan selections.

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
    pub const fn raw(self) -> u32;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionIx(u32);

impl RegionIx {
    pub const fn raw(self) -> u32;
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

Providing roots sorted by start byte can accelerate bounded queries. Ordered,
nonoverlapping roots also allow seeking directly to the first relevant tree.
Document these benefits on `PackRegion`; packing preserves input order and detects
these properties automatically. Overlapping, nested, and unsorted inputs remain
valid. Ordering does not establish shared source identity or point coordinates.

Discovery can append one-root inputs as parsing completes, including when grammars
recur through injection nesting. Native trees must stay alive until `pack_forest`
returns because the inputs contain borrowed nodes.

Each root packs the supplied node and its descendants as an independent tree;
the node need not be a whole-tree root. Preserve its displayed kind/alias and
subtree contents. Its packed root has no parent, siblings, parent field, or
supertype context inherited from excluded ancestors. Relationships and supertype
context within the subtree remain intact. Queries on the detached tree need not
match queries that depended on its original ancestors.

`PackOptions::symbol_presence` is a region predicate, evaluated after the core
layout is finalized. Its internal default selects regions with at least 64 groups;
`points` continues to default to true. The group threshold is an initial heuristic,
not a measured break-even point. Callers can select by language, size, or workload,
or use `|_| true` / `|_| false` for all / no regions.

Presence uses one separate allocation covering selected regions; points uses one
covering the entire forest. Failure to construct requested side data fails packing.
Points must be requested during packing: their delta limits affect core grouping
and physical IDs. Presence does not affect grouping.

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
Crate-private `SlotIx` addresses a physical slot within the forest, including
group waste; wasted slots do not produce nodes. `NodeId` combines a tree index
and slot index and is local to one forest. None of these indices or IDs is stable across
rebuilding/reordering. The caller chooses a main tree if its application has one;
physical order makes no tree the document root.

```rust
// placement checked before constructing the positioned root
let positioned_second = second.root_node_with_offset(second_origin, second_point);
let third_subtree = third.root_node().named_child(0).unwrap();
let inputs = vec![
    PackRegion {
        language: language_a.clone(),
        roots: vec![first.root_node()],
    },
    PackRegion {
        language: language_b,
        roots: vec![positioned_second],
    },
    PackRegion {
        language: language_a,
        roots: vec![third_subtree],
    },
];
let options = PackOptions {
    symbol_presence: &|_| false,
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

enum RegionOrder {
    Unordered,
    ByStart,
    NonOverlapping,
}

struct RegionData {
    slots: Range<SlotIx>,
    trees: Range<TreeIx>,
    language: Language,
    order: RegionOrder,
    presence: Option<NonNull<u8>>, // bitmap segment in the forest cache
}

struct TreeData {
    region: RegionIx,
    tables: NonNull<GrammarView>,
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
    pub(crate) const fn new(tree: TreeIx, slot: SlotIx) -> Self {
        Self(((tree.raw() as u64) << 32) | slot.raw() as u64)
    }

    pub const fn tree(self) -> TreeIx {
        TreeIx::from_raw((self.0 >> 32) as u32)
    }

    pub(crate) const fn slot(self) -> SlotIx {
        SlotIx::from_raw(self.0 as u32)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy)]
pub struct Node<'forest> {
    forest: &'forest ForestData,
    id: NodeId,
}

pub struct TreeCursor<'forest> {
    forest: &'forest ForestData,
    tree: TreeIx,
    slot: SlotIx,
    tables: &'forest GrammarView,
    // traversal state
}

pub struct QueryCursor {
    // reusable scratch, independent of any forest
}

impl<'forest> Node<'forest> {
    pub fn id(&self) -> NodeId;
    pub(crate) fn slot(self) -> SlotIx;
    pub fn walk(&self) -> TreeCursor<'forest>;
}

impl<'forest> TreeCursor<'forest> {
    pub fn node(&self) -> Node<'forest>;
}
```

Forest nodes expose the composite `NodeId` through both inherent `Node::id()`
and `NodeLike::id()` (`type Id = NodeId`). This supersedes the API alignment
plan's slot-only identity and omission of an inherent `id()` for standalone trees.
Crate-private `slot()` supports internal physical addressing. The composite ID
carries the tree context needed for descriptor lookup and caller-owned source
selection.

`NodeId` stores `TreeIx` in bits 63–32 and `SlotIx` in bits 31–0. The slot remains
forest-global, not relative to the tree. Constructing an ID only combines the
indices; constructing a node ensures the slot is live and belongs to that tree.
The ID contains no forest identity or lifetime. Node equality and hashing use
both forest identity and `NodeId`.

Column access follows `Node → ForestData.columns → column bytes`, indexed by
`id.slot()`, without going through a `Tree` handle or tree descriptor. A forest
reference and a 64-bit ID can fit in 16 bytes on a 64-bit target; verify the
implemented layout.

Tree-dependent operations index `ForestData::trees` by `id.tree()` for slot bounds,
cached grammar tables, and `RegionIx`. The region lookup remains necessary for
language ownership and presence metadata. Table pointers borrow the immutable
native grammar retained by each region's language; cloning a language preserves
the table address. Cursors and scans resolve tables once, and cursor resets
refresh that reference. Bundled attributes reuse one table reference and decoded
IDs. Returned nodes and captures retain the compact forest/ID representation.
Cursor navigation updates a separate 32-bit slot; the forest and tree stay fixed
until reset. Reading a cursor node combines the tree and slot into a `NodeId`.
The reusable query cursor retains no forest borrow between executions. Column
pointers are stored once in `ForestData`.

Trees occupy separate groups, including when loaded from core bytes. Scalar
preorder steps within a group decrement the slot directly; only group crossings
check the tree boundary and skip waste. First-child lookup checks the subtree
boundary, which also excludes neighboring trees.

## Ownership

Forest owns all columns, retained grammar handles, and attached side data. Views,
nodes, cursors, and scans borrow the owner; no view frees or retains a slab on its
own. Sidecar handles live in the owner descriptor, outside the core slab.

Presence and point data have independently loadable allocations. Set/drop requires
exclusive access to the forest and leaves core columns, descriptors, groups, and
IDs intact. Built/copied sidecars never share allocations with the core or each
other; mapped sidecars retain storage owners as in step 1. Cached regions borrow
slices of the forest cache; uncached regions have no presence pointer. Attaching
or dropping presence replaces the whole cache; dropping clears its region views
and releases its allocation independently of point data.

## Navigation

```rust
impl TreeLike for Tree<'_> {
    type Node<'tree> = Node<'tree> where Self: 'tree;

    fn root_node(&self) -> Node<'_>;
}

let root = forest.tree(input_trees[0]).unwrap().root_node();
assert!(root.parent().is_none());
let nodes = root.preorder();
```

Child/sibling/parent navigation and subtree scans stay within their tree and
scope. Root parent and sibling navigation terminate there. Node identity and
equality distinguish independent trees even when their bytes overlap.

## Queries

Queries start on a particular tree (or a node within it) or a borrowed region.
They do not accept whole forests, arbitrary tree sets, or scans. Scan structs
remain a separate API. Configure byte/point ranges and maximum start depth
through Tree-sitter-style cursor setters.

**Range semantics.** Ordinary byte/point bounds allow structural matching outside
the range. Rooted patterns start at overlapping nodes; non-rooted patterns use
Tree-sitter's parent-overlap rules. Completed matches retain captures outside the
range, while capture iteration skips their individual events. Structural state,
depth, and anchors remain confined to the selected subtree and tree.

For a C tree containing `int answer() { return 42; }`, a range covering only
`int` still allows a completed match capturing `answer`:

```rust
let root = forest.tree(input_trees[0]).unwrap().root_node();
let source = b"int answer() { return 42; }".as_slice();
let query = Query::new(
    root.language(),
    r#"
    (function_definition
      declarator: (function_declarator declarator: (identifier) @name))
    "#,
)?;
cursor.set_byte_range(0..3);
let mut execution = cursor.execute(&query, root, source);
let found = execution.next_match().unwrap();
assert_eq!(found.captures()[0].node.byte_range(), 4..10);
```

Maximum start depth restricts pattern starts, not the depth of their remaining
steps. Preserve the prerequisite limit, cancellation/resumption, provisional
snapshot, duplicate, and completed-capture contracts except where the range
semantics above supersede them. Containing-range setters are a separate feature;
Tree-sitter-style restriction setters do not require adding them in this step.

**Languages and execution.** Check the query's exact language against the tree's
or region's language once, before returning results. Regions are homogeneous, so
no per-node language checks are needed. Callers iterate compatible regions in a
mixed-language forest and manage application query configurations separately.

Initially run the existing matcher one tree at a time, skipping unselected trees
and regions. Fallback in one tree must not disable fast execution elsewhere.
Preserve execution-wide match identity and removal across trees. Regions follow
physical input order; a grammar may recur in several regions. Cross-tree source
ordering and result merging belong to the caller. Shared candidate scanning can
be added later.

**Source text and coordinates.** A `TextProvider` uses `node.id().tree()` to select
caller-owned text. For example, with sources indexed by this forest's `TreeIx`
and node byte ranges directly indexing their source:

```rust
let mut cursor = QueryCursor::new();
for region in forest.regions() {
    let query = Query::new(
        region.language(),
        r#"((identifier) @name (#eq? @name "answer"))"#,
    )?;
    let text_provider = |node: Node<'_>| {
        let source: &[u8] = sources[node.id().tree().raw() as usize];
        std::iter::once(&source[node.byte_range()])
    };
    let mut execution = cursor.execute(&query, &region, text_provider);
    while let Some(found) = execution.next_match() {
        for capture in found.captures() {
            names.push(capture.node.id());
        }
    }
}
```

This example assumes each grammar has an `identifier` node. Document source
selection in `TextProvider`: tree indices are forest-local, and providers resolve
coordinates into source bytes or chunks. A single byte-slice provider works only
when that slice matches every selected tree's byte frame. Forests retain neither
source provenance nor providers or source bytes after execution.

Points use attached parser coordinates or the step 1 row-zero frame.
`has_points()` reports availability, not a shared document frame; query text
cannot supply missing points.

**Ranges across trees.** Region executions apply cursor bounds to every selected
tree. When supplying bounds, the caller must ensure the region's trees share a
source and the relevant coordinate frame. Unbounded queries can use unrelated
sources, as above. The forest does not verify source identity.

Internal ordering metadata selects the byte-range scan strategy:

- `NonOverlapping`: binary-search root ends for the first possible overlap, then
  scan roots until their starts reach the viewport end. Selected trees form one
  contiguous interval. Selection costs O(log trees + selected trees).
- `ByStart`: inspect root bounds from the beginning, stopping when a root starts
  at or after the viewport end. Earlier roots may extend into the viewport, so
  seeking by start alone would miss matches.
- `Unordered`: inspect every root's bounds.

For nonoverlapping roots `0..20`, `30..40`, and `50..70`, viewport `35..55` selects
only the last two trees. For start-sorted roots `0..1000`, `20..40`, `500..600`,
and `700..800`, viewport `510..520` requires checking the first three roots and
selects the first and third; the fourth root ends the search.

All paths preserve Tree-sitter's empty-node and unbounded-range conventions,
including zero-width roots at the viewport start. Apply query restrictions within
each selected tree and retain structural access outside the viewport. With
`ByStart`, finishing an overlapping tree may advance past the start of the next
tree; root ordering does not give global node ordering. Point-only restrictions
use per-tree checks; byte ordering does not establish point ordering. When both
bounds are supplied, byte selection can prune trees before point checks.

## Extend side data to forests

Generalize step 1's presence builder and side-data set/drop methods to forests,
using the same owned side-data types. Single-tree forests use these same APIs:

```rust
pub struct PackOptions<'options> {
    pub symbol_presence: &'options dyn Fn(ForestRegion<'_>) -> bool,
    // remaining fields omitted
}

impl PresenceCache {
    pub fn build(forest: &Forest) -> Result<Self, SideDataError>;
    pub fn build_selected(
        forest: &Forest,
        select: impl Fn(ForestRegion<'_>) -> bool,
    ) -> Result<Self, SideDataError>;

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

`build` caches every region; `build_selected` caches only regions accepted by the
predicate. Both derive new data from an immutable forest borrow. Packing uses
the same selection/build path, with `PackOptions::symbol_presence` defaulting to
the group-count threshold above. Predicates may borrow caller configuration;
neither the forest nor the cache retains them.

```rust
let queried_language = language.tree_sitter_language();
let select_presence = |region: ForestRegion<'_>| {
    region.language().tree_sitter_language() == queried_language
};
let options = PackOptions {
    symbol_presence: &select_presence,
    ..PackOptions::default()
};
let (mut forest, input_trees) = packer.pack_forest(inputs, options)?;

// the same predicate can rebuild coverage on an existing forest
let cache = PresenceCache::build_selected(&forest, &select_presence)?;
forest.set_presence_cache(cache)?;
```

After finalizing the core layout, evaluate the predicate exactly once per region
in physical order, using its actual group count. Retain those decisions, compute
the checked payload size, allocate one zeroed buffer, and fill selected regions
directly. Visit each occupied slot in selected regions and set its group's bit
for that symbol; skip waste slots and unselected regions. This is a separate pass
over packed symbols, as in the existing single-tree builder. Construction remains
cancellable between regions and groups. If packing selects no regions, leave the
forest's cache absent. An explicit `build_selected` selecting none produces a
valid cache containing only absent-region records.

`from_bytes` copies serialized data into owned storage, matching `Forest::from_bytes`;
`from_retained` retains immutable storage without copying. Both loaders validate
the serialized format without a forest. Setters check compatibility with the
destination forest. Content validation is explicit through `validate_for` or
`Forest::validate()`.

The forest presence cache concatenates one record per region in physical order
into one allocation. Present records retain the existing region header and bitmap
layout. Absent records use a distinct format tag, retain the region dimensions,
and have no bitmap payload. Both preserve alignment; no outer header or offset
table is needed. A cached one-region forest uses the same bytes as a standalone
region cache. An empty forest has an empty concatenation.

```text
presence cache
  [region 0 header + bitmaps][region 1 absent header][region 2 header + bitmaps]
```

Each record's size follows from its presence tag, group count, and grammar symbol
count. Loading checks tags, dimensions, and record extents, and requires records
to consume the input exactly. Attachment checks record counts and dimensions
against the forest's regions, including absent records; reject extra, missing,
or incompatible records. Resolve present-region payload pointers once on
attachment and clear absent-region pointers. Reads do not walk earlier records.
Mapped loading retains one storage owner; copied loading uses one aligned
allocation and copies the concatenation without rebuilding bitmaps.

Bitmaps have one bit per physical group within their region; tree views use a
region-relative group offset. Regions with the same grammar share grammar
handles and prepared tables but have separate bitmap segments. Each uncached
region uses ordinary symbol scanning; absence never means that a symbol is absent.
All records are attached or dropped together. Changing coverage requires building
and replacing the whole cache. Loading preserves recorded coverage without
rerunning the default policy. Presence coverage changes performance only.

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
into another representation or reconstructs indexes. Loading checks format,
dimensions, alignment for retained storage, and payload sizes. Attachment compares
the target kind, region counts, and dimensions with the forest, and checks
point-delta overflow. Other content checks are explicit through `validate_for` or
`Forest::validate()`. The caller supplies data built for the matching
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
grammar index. Grammar grouping does not establish source identity.

`RegionOrder` is internal runtime metadata, computed from root byte bounds during
packing and recomputed while loading the tree metadata. Choose the strongest
applicable classification:

- `NonOverlapping` when every adjacent pair satisfies
  `previous.end_byte() <= next.start_byte()`. Starts and ends are nondecreasing;
  adjacent spans and repeated zero-width roots are allowed.
- Otherwise, `ByStart` when starts are nondecreasing. Equal starts, nesting, and
  crossing overlaps are allowed, with no constraint on end order.
- Otherwise, `Unordered`.

A single-tree region is `NonOverlapping`. Classification needs only adjacent root
bounds and constant scratch space, adding O(number of trees) work. It is neither
caller-supplied nor serialized and requires no descendant traversal or source text.

Serialize fields explicitly in little-endian form. Safe loading performs the same
memory-safety checks in every build profile. Check header/count/size arithmetic,
aligned region and tree bounds, nonempty groups, nested subtree spans, live sibling
destinations, compact ID and supertype dictionary bounds, and coordinate arithmetic.
Reconstruct trees backward from root spans with strict progress inside each region
and bound the resulting count by `TreeIx`. Ordering classification reads root bounds.

The unsafe `from_bytes_unchecked`, `from_bytes_borrowed_unchecked`, and
`from_retained_unchecked` APIs retain header/layout checks and tree reconstruction
but skip node validation. Their callers must establish the safety invariants for
the supplied grammar bindings.

`Forest::validate()` explicitly checks full core contents and calls `validate_for`
on each attached presence or point cache. Reserved IDs, field IDs, sibling flags,
direct supertype masks, root metadata, and byte-range ordering are content checks.
Loading never runs these checks implicitly, including in debug builds.
Loading retains supplied grammar handles; each grammar index selects a
caller-supplied grammar. The caller must supply the matching grammars;
persistent compatibility checks are separate work.

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

## Implementation order

1. Add forest descriptors, caller-defined regions, and flattened root-to-tree mapping;
   accept native subtree nodes with per-root coordinate and per-group slot bounds.

2. Add compact forest/`NodeId` nodes with descriptor lookups through `SmallVec`s
   with inline capacity one for trees and regions.

3. Add forest presence caches with selective region coverage, a shared region
   predicate for packing/building, and a default packing threshold of
   64 or more groups. Add point data with independent per-tree sources.

4. Add checked serialization, retained grammar bindings, and storage-independent
   read paths for allocated and retained storage. Serialize only region
   descriptors and reconstruct runtime tree metadata from root spans.

5. Extend queries to tree/region scopes, initially matching one tree at a time.
   Use cursor setters for query restrictions and region ordering for byte-range
   tree selection. Bounded region queries require caller-established source context.

## Verification

Keep the existing tests and add three focused tests, reusing fixtures and comparison
helpers where practical:

1. **Forest packing and round trip.** Pack A/B/A grammar regions, with several
   trees in one region. Check input mapping, distinct node identities, and
   navigation staying within each tree. Serialize and reload the forest, then
   compare its trees with the original.

2. **Region queries.** Compare a region query with querying its trees individually,
   using a text provider that selects sources by tree ID. Check rejection of a
   query compiled for a different language.

3. **Bounded queries and ordering.** Use one example for each ordering class and
   compare region results with per-tree queries using the same bounds. Include an
   early root that extends into the viewport so start-based pruning cannot skip it.

Broader combinations, malformed inputs, boundary cases, and invariant checking
belong to the planned property and fuzz tests that exercise the full system.
Those gaps are intentional; implementing this design does not require expanding
the focused tests into an exhaustive suite.
