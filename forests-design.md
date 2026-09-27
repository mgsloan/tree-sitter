# Generic forests

Step 2 of 3: [side data](side-data.md) → forests →
[injections](injections-design.md). Assume step 1 is implemented, including
optional materialized points and independently owned side data.

Decision draft for `crates/squatter-rust`, not implemented API. Rust excerpts
show additions; routine constructors, errors, and unchanged methods are omitted.
Prototype formats remain at version 0, with no migration support.

A forest owns multiple independent packed trees, grouped by exact grammar.
It has no language resolver, discovery policy, logical layer graph, host tree,
or application query configuration. Callers can use it without the third step.

## Packing and tree views

```rust
pub struct Forest {
    core: ForestSlab,
    grammars: Vec<Grammar>,
    presence_caches: Vec<Option<PresenceCache>>, // one independently owned entry per region
    point_data: Option<PointsData>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeId(u32);   // local to one forest
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionId(u32); // local to one forest
pub struct TreeView<'forest> { /* borrowed tree interval and owner */ }
pub struct GrammarRegion<'forest> { /* borrowed region and owner */ }

pub struct PackInput<'tree> {
    pub grammar: &'tree Grammar,
    pub tree: &'tree tree_sitter::Tree,
    pub byte_origin: usize,
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
    pub fn tree(&self, id: TreeId) -> Option<TreeView<'_>>;
    pub fn regions(&self) -> impl Iterator<Item = GrammarRegion<'_>>;
    pub fn has_points(&self) -> bool;
}

impl<'forest> GrammarRegion<'forest> {
    pub fn id(&self) -> RegionId;
    pub fn grammar(&self) -> &'forest Grammar;
    pub fn trees(&self) -> impl Iterator<Item = TreeView<'forest>>;
}

impl<'forest> TreeView<'forest> {
    pub fn id(&self) -> TreeId;
    pub fn root_node(&self) -> Node<'forest>;
    pub fn has_points(&self) -> bool;
}
```

The returned vector maps input order to physical tree IDs. Empty input is valid.
Use deterministic region/tree ordering for the same ordered inputs and bindings.
Each exact grammar gets one contiguous region; each tree gets a group-aligned
interval. Grouping trees never combines their native parser inputs.

`PackOptions::symbol_presence` requests a completed cache for every grammar
region; `points` requests completed point data for the forest. Both default to
true as in step 1. Requested sidecars use their own allocations, even when filled
during forest packing. Failure to construct requested side data fails the
operation. Points must be requested during packing: their delta limits affect
core grouping and physical IDs. Presence does not affect grouping.

Packing adds `byte_origin` to native byte coordinates with checked arithmetic.
The core stores those resulting bytes; there is no additional placement applied
by node access. This supports trees parsed from slices of a larger source without
requiring any knowledge of why they were parsed separately. Nodes can span gaps
in native included ranges; the forest does not retain parser requests.

Tree/region IDs identify descriptors only within their owner. Physical node IDs
identify slots, including group waste; wasted slots do not produce nodes. None of
these IDs is stable across rebuilding/reordering. The caller chooses a main tree
if its application has one; grammar order makes no tree the document root.

```rust
let inputs = [
    PackInput { grammar: &grammar_a, tree: &first, byte_origin: 0 },
    PackInput { grammar: &grammar_b, tree: &second, byte_origin: second_origin },
    PackInput { grammar: &grammar_a, tree: &third, byte_origin: third_origin },
];
let options = PackOptions {
    symbol_presence: false,
    points: false,
    ..PackOptions::default()
};
let (forest, input_trees) = packer.pack_forest(&inputs, options, None)?;
let second_root = forest.tree(input_trees[1]).unwrap().root_node();
assert_eq!(second_root.start_byte(), second_origin + second.root_node().start_byte());
```

Retain standalone `Tree` and its convenience packing API. Internally it follows
the same storage and borrowing rules as a one-tree forest; callers need not
construct a forest to use ordinary trees.

## Ownership, navigation, and queries

Forest owns all columns, retained grammar handles, and attached side data. Views,
nodes, cursors, and scans borrow the owner; no view frees or retains a slab on its
own. Sidecar handles live in the owner descriptor, outside the core slab.
Each presence region and the point data have independently loadable allocations.
Set/drop requires exclusive access to the forest and never shifts or rewrites
its core columns, descriptor tables, groups, or IDs. Built/copied sidecars never
share an allocation with the core or another sidecar. Mapped sidecars retain
backing owners as in step 1. Dropping a region cache frees its owned storage or
releases its backing handle independently of other regions and point data.

```rust
impl TreeLike for TreeView<'_> {
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
unrelated trees. Grammar iteration is not source order.

Forests do not prove source provenance or require that every tree came from one
source. Point data comes from parser coordinates during packing, not from a
later source lookup. Point accessors and point-bounded queries use attached
coordinates or the row-zero frame from step 1. Callers requiring document points check `has_points()`;
query source bytes do not supply missing points.

## Extend side data to forests

Keep the presence builder and side-data set/drop methods. Add region presence
builders and whole-forest point loading using the same owned side-data types:

```rust
impl PresenceCache {
    pub fn build_region(
        region: GrammarRegion<'_>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, SideDataError>;

    pub fn from_region_backing(
        region: GrammarRegion<'_>,
        backing: impl StableSlab,
    ) -> Result<Self, SideDataError>;

    pub fn copy_from_region_bytes(
        region: GrammarRegion<'_>,
        bytes: &[u8],
    ) -> Result<Self, SideDataError>;
}

impl PointsData {
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
    pub fn set_point_data(&mut self, points: PointsData) -> Result<(), SideDataError>;
    pub fn drop_presence_cache(&mut self, region: RegionId);
    pub fn drop_point_data(&mut self);
}
```

Presence caches describe one region of the forest. Bitmaps have one bit per
physical group in that region; tree views borrow them with a region-relative
group offset. An uncached region uses ordinary symbol scanning even if other
regions have caches.

Point data covers the entire forest. Create it during packing, when every tree
uses the same coordinate frame. Group boundaries must account for point deltas.
Attachment is all-or-nothing; `Forest::has_points()`, `TreeView::has_points()`,
and its nodes report the same availability. Without point data they all use
row-zero access, including forests containing trees from unrelated sources.

The caller supplies persisted point data for the exact matching forest. Point
access performs no source lookup and needs no retained source bytes. Forests with unrelated
sources can use explicit source conversion outside accessors, or separate owners when they need
attached points. Per-tree point attachments are outside this initial interface.

Side data uses the `as_bytes` representation from step 1. The backing constructors
read mapped payloads directly and retain their owners; the copy constructors
allocate aligned storage and memcpy the same layout. Neither path decodes fields
into another representation or reconstructs indexes. Release loading
and attachment check target kind, region counts, dimensions, alignment, payload
sizes, and point-delta overflow. Other content checks run only in debug builds.
The caller supplies data built for the matching forest/region; count checks alone do not prove that
pairing. Reordering trees/groups while rebuilding a core requires fresh side
data or a correct remapping. Loading or setting side data does not rebuild the
core. Failed attachment leaves current side data unchanged. Workers can build
presence caches through immutable region borrows; completed values retain no
borrow. Set/drop requires exclusive owner access after those borrows end. Set replaces existing data on success; drop frees owned storage or
releases the mapped backing handle, returns nothing, and is a no-op when absent.
Point attachment/removal preserves layout and IDs but switches the coordinate
frame; presence attachment/removal preserves query results.

## Representation and serialization

Symbol and grammar-symbol columns share one width across the slab, chosen from
all participating grammars before packing: one byte if every grammar has at most
254 symbols and aliases, otherwise two bytes. This includes both remapped error
IDs. The grammar-symbol column is first in the optional tail and is omitted only
when every emitted node has equal symbol and grammar-symbol IDs. Compressed
absolute bytes remain in shared columns; points and presence remain outside the
slab.

```text
forest
  grammar A region: [tree 0 groups][tree 1 groups][tree 2 groups]
  grammar B region: [tree 3 groups][tree 4 groups]
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
}

impl Forest {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error>;
    pub fn from_bytes(grammars: &[Grammar], bytes: &[u8]) -> Result<Self, Error>;
}
```

Region group bounds follow from its first and last trees. Tree node bounds follow
from group size; the final occupied slot is the root under reverse-preorder
encoding. Avoid redundant root/boundary tables unless measurements justify them.

Serialize fields explicitly in little-endian form. Release loading performs only
cheap header/count/size checks, including checked arithmetic for table and column
extents. Do not scan descriptors, nodes, indexes, or coordinates for validity.

Under `#[cfg(debug_assertions)]`, scan descriptors and contents: check alignment,
offsets, column/index/coordinate bounds, and grammar references. Regions partition
the tree table; trees partition used groups; topology stays inside each tree.
These remain representation invariants; release loading does not revalidate them
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
The core serializer requires no LMDB or application manifest. Initially load
forests into owned storage; compare a
compact-tree archive with direct shared-column persistence before introducing
transaction-backed forest ownership and its lifetime/validation machinery.

## Implementation and verification

1. Add forest descriptors, exact-grammar grouping, and input-to-tree mapping.
2. Adapt nodes, cursors, scans, and queries to tree-bounded borrowed storage.
3. Extend side data to regions and shared-source forests.
4. Add checked serialization and retained grammar bindings.

Verify empty, single-tree, mixed-grammar, and repeated-grammar inputs without any
discovery engine. Compare each packed tree with its native/standalone counterpart,
including overlapping bytes, nonzero origins, errors, predicates, point bounds,
and ties. Exercise every root/tree boundary, wasted slots, input mapping, changed
grammar bindings, serialization round trips, malformed descriptors, overflow,
cancelled packing, and existing-reader lifetimes. Verify region-specific missing
presence caches, invalid region/dimension rejection, and sidecar serialization
round trips. Presence changes must preserve query results. Points must
match explicit source conversion while attached and use row-zero coordinates
before attachment and after removal; test point-bounded queries in both states.
Byte-based matching and core contents must remain unchanged. Setting/replacing/
dropping independently built or loaded sidecars must preserve the core allocation
address, serialized bytes, descriptor offsets, groups, and IDs. Test creation
flags and immediate reclamation of each sidecar while the forest remains alive.
Check malformed counts/sizes in release and debug builds, and malformed contents
with matching counts in debug builds. Release load paths must contain no content
validation scan.

Later, compare per-tree/segmented queries with shared candidate scanning and
contiguous reassembly. Include viewport selection, predicates, merging, copying,
validation, and index construction in measurements. Candidate scanning may cross
tree boundaries; structural matching may not. Batching, result merging, and
placement/relocation APIs remain outside the initial forest interface.
