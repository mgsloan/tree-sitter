# Side data: symbol presence and points

Step 1 of 3: side data → [forests](forests-design.md) →
[injections](injections-design.md). This step works on standalone trees and does
not require either later feature.

Implementation design for the Rust core in `crates/squatter-rust`.
Code shows additions and changed signatures; omitted fields/methods remain as
before. The C-backed reference stays separate, as in the
[Rust core design](rust-core-design.md).

No data has been persisted for ongoing use. Temporary databases create no
compatibility obligation. Prototype format, schema, and profile versions remain
at 0; change them directly and regenerate caches without migrations. Tree-sitter's
upstream ABI versions are independent. These rules apply to all three steps.

## Two kinds of side data

Keep compressed byte coordinates and authoritative interpretation data in the
immutable slab. Remove symbol-presence bitmaps, per-node point columns, and their
per-group point bases. Grammar-symbol overrides and supertype encodings remain
authoritative data.

| Side data | What it adds | When absent |
| --- | --- | --- |
| `PresenceCache` | faster symbol scanning | same results through ordinary scanning |
| `PointData` | materialized row/column coordinates | point APIs return `(0, byte_offset)` |

Both are derived from a richer input, but only presence is a transparent cache
for the tree API. Points are reproducible from source plus tree. A tree alone
cannot recover them, and their presence changes its observable behavior.

Use **point data** for the attached allocation and **point sidecar** for its
serialized form. Persistence can cache data derived from the source and tree;
that does not make it a transparent runtime optimization.
Attaching/removing point data changes coordinates and can change point-bounded
query results.

```rust
pub struct PackOptions {
    pub initial_group_capacity: u32,
    pub repack: bool,
    pub symbol_presence: bool,
    pub points: bool,
}

pub struct Tree {
    core: CoreSlab,
    grammar: Grammar,
    presence_cache: Option<PresenceCache>,
    point_data: Option<PointData>,
}

pub struct PresenceCache { /* independently owned symbol-presence data */ }
pub struct PointData { /* independently owned point data */ }
```

Keep `symbol_presence` and `points` as conversion/parse options, with their
existing defaults of true. They select which sidecars are created and set before
the operation returns. A true flag promises completed side data on success;
allocation or construction failure must not silently return a tree without it.
A false flag leaves that sidecar absent. Construction may fill sidecar storage
during packing/parsing, but always uses separate allocations.

```rust
let options = PackOptions {
    symbol_presence: false,
    points: false,
    ..PackOptions::default()
};
let mut tree = packer.pack_with_options(&grammar, &native, options)?;
assert!(!tree.has_points());
// callers can build/set sidecars later, or drop those requested at creation
```

Keep `Tree::has_points()` as the explicit test for current point availability:
creation options describe the initial state, while later set/drop operations can
change it. Side-data flags control construction, not the core slab format. A loader must honor its requested side-data policy on both
hits and misses; a core-tree hit alone does not fulfill a request for points.

## Independent allocations

The owner descriptor above is outside the serialized core slab. Its optional
sidecar handles point to independently owned storage:

```text
Tree
  core             → immutable core slab allocation
  presence_cache   → optional presence allocation
  point_data       → optional point allocation
```

Each sidecar is buildable, loadable, and settable independently after the core
exists. `set_*` changes only the owner's sidecar handle; it must not resize,
move, repack, or rewrite the core slab. `drop_*` has the same core-stability
requirement. Core addresses, column offsets, groups, node IDs, serialized bytes,
and serialized core contents remain unchanged.

Sidecars index core groups/slots; they are not appended columns addressed by
offsets in the core header. Their own encoding and allocation sizes do not affect
the core layout.

Built or copied core, presence, and point data must use separate allocations,
including when created together during conversion/parsing. Do not coallocate
sidecars with the core or with each other. Mapped sidecars occupy separate LMDB
values and retain their backing through an owner. `drop_*` immediately frees an
owned sidecar allocation or releases its mapped backing handle, independently of
the core. Releasing a mapped handle does not unmap the database or reclaim pages
still held by other readers.

## Point access without source

Keep accessors source-free, infallible, and constant-time. They read attached
point data or use the existing synthetic coordinate frame:

```rust
impl Tree {
    pub fn has_points(&self) -> bool;
}

impl<'tree> Node<'tree> {
    pub fn has_points(self) -> bool;
    pub fn byte_range(self) -> Range<usize>;
    pub fn start_position(self) -> Point;
    pub fn end_position(self) -> Point;
    pub fn point_range(self) -> Range<Point>;

    pub fn descendant_for_point_range(self, start: Point, end: Point) -> Option<Self>;
}

impl Cursor<'_> {
    pub fn goto_first_child_for_point(&mut self, point: Point) -> Option<usize>;
}

// accessor behavior when has_points() is false
fn fallback_start(node: Node<'_>) -> Point {
    Point::new(0, node.start_byte())
}

fn fallback_end(node: Node<'_>) -> Point {
    Point::new(0, node.end_byte())
}
```

Named point-range navigation follows the same contract. No accessor reads source,
builds an index, allocates, or searches line starts. Attachment/removal switches
the coordinate frame for all subsequent borrowers; it never changes byte offsets.
A real point can also have row zero, so the returned value is not an availability
test. Callers needing document coordinates check `has_points()` first.

### Attributes and shared traits

Keep the existing point-bearing attributes and source-free signatures. Add an
availability flag so callers can distinguish stored points from synthetic ones
without guessing from row/column values.

```rust
pub struct Attributes<'tree> {
    // all existing fields, including start_position and end_position
    pub has_points: bool,
}

pub trait NodeLike<'tree>: Copy + Eq {
    fn has_points(self) -> bool;
    fn attributes(self) -> Attributes<'tree>;
    fn start_position(self) -> Point;
    fn end_position(self) -> Point;
    fn descendant_for_point_range(self, start: Point, end: Point) -> Option<Self>;
    // other methods unchanged
}

pub trait CursorLike<'tree> {
    fn attributes(&mut self) -> Attributes<'tree>;
    fn goto_first_child_for_point(&mut self, point: Point) -> Option<usize>;
    // other methods unchanged
}
```

Concrete nodes/cursors use these same attribute signatures. Packed `has_points`
reports attachment; the native Tree-sitter implementation reports true because
native trees retain point coordinates. This flag describes availability, not
proof that custom native coordinates match a particular document source.

### Queries

Keep query execution's byte-slice input for text predicates. Supplying bytes does
not materialize points or change the meaning of point bounds.

```rust
impl QueryCursor {
    pub fn set_point_range(&mut self, range: Range<Point>) -> bool;

    pub fn execute<'cursor, 'query, 'tree, 'text>(
        &'cursor mut self,
        query: &'query Query,
        root: Node<'tree>,
        source: &'text [u8],
    ) -> QueryExecution<'cursor, 'query, 'tree, 'text>;
}

// a caller requiring document-coordinate bounds checks the capability first
assert!(root.has_points());
cursor.set_point_range(document_range);
let execution = cursor.execute(&query, root, bytes);
```

Point bounds and point-based navigation use the same coordinates as accessors,
including the row-zero frame when point data is absent. Preserve existing range
semantics, sentinels, and boundaries. A query with document point bounds can
therefore produce different matches without point data. Byte-bounded matching
and text predicates remain independent of point availability; point fields on
returned nodes/attributes still reflect it.

## Explicit point derivation

Source is an input to point-data construction, not to node access. Keep line-index
construction and lookup explicit so their cost cannot hide in a cheap accessor.

```rust
pub struct LineIndex { /* owned line starts */ }

impl LineIndex {
    pub fn new(bytes: &[u8]) -> Result<Self, Error>;
    pub fn point(&self, byte: usize) -> Point;
}
```

The index owns only line starts and does not retain the source. Building it reads
the input; individual `point` calls search line starts. Offsets past EOF extend
the final row's byte column. An application can use this explicitly for occasional
conversion, or build complete `PointData` before publishing a tree to consumers.
Neither operation happens automatically during tree access.

Derived document points use zero-based rows and byte columns. Row is the number
of LF bytes strictly before the offset; column is the offset minus that row's
start. Zero and EOF are valid, including EOF after a final newline. Do not decode
or normalize CRLF, BOMs, or encoding. Other coordinate units require another
explicit profile. One source index can serve multiple trees using the same bytes
and coordinate frame, and can be dropped after building their point data.

A generic tree does not prove which source produced it. The builder's caller
supplies matching bytes. Native parsers may accept custom point coordinates;
this materializer produces source-derived document coordinates instead. Parser-input points can
affect parsing, separately from derived output data.

## Side-data construction and ownership

```rust
pub enum SideDataError {
    Cancelled,
    InvalidTarget,
    Core(Error),
}

impl PresenceCache {
    pub fn build(tree: &Tree, cancel: Option<&AtomicBool>) -> Result<Self, SideDataError>;
}

impl PointData {
    pub fn build(
        tree: &Tree,
        source: &LineIndex,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, SideDataError>;
}

impl Tree {
    pub fn set_presence_cache(&mut self, cache: PresenceCache) -> Result<(), SideDataError>;
    pub fn set_point_data(&mut self, points: PointData) -> Result<(), SideDataError>;
    pub fn drop_presence_cache(&mut self);
    pub fn drop_point_data(&mut self);
}
```

`PresenceCache` and `PointData` remain public owned types. Builders borrow the
tree immutably, so workers can build side data while other readers use the tree.
Completed values are `Send` and retain no tree/source borrow.

`set_*` consumes a completed value, performs the cheap checks described below, and
replaces existing side data only on success. The caller supplies data for the
matching tree and, for points, source. Count checks do not prove that pairing.
Cancellation or a failed build/set leaves the tree
unchanged. `drop_*` immediately frees owned storage or releases mapped backing and
returns nothing; dropping absent side data is a no-op. Neither operation changes the
core slab, other sidecars, or node IDs.
Dropping points restores the row-zero frame; dropping presence changes only
performance. Point eviction must respect the consumer's point requirements.

```rust
let presence = std::thread::scope(|scope| {
    let worker = scope.spawn(|| PresenceCache::build(&tree, None));
    // other readers can borrow tree while the worker builds
    worker.join().expect("presence worker panicked")
})?;

// the worker's immutable borrow has ended
tree.set_presence_cache(presence)?;
tree.drop_presence_cache();
```

Serialized and freshly built side data use the same owned types. Background
construction needs no mutable owner access; setting or dropping side data still
requires exclusive access.

Each public-symbol bitmap has one bit per physical group. A clear bit permits
skipping; a set bit only indicates a possible match. Use the query candidate
selection's interpretation of public symbols, aliases, and supertype requirements.
Build from symbol columns. An absent cache is distinct from an all-zero bitmap;
without a cache, use ordinary symbol scanning.

Point data addresses physical node slots, ignoring group waste. Absolute or
compressed values are a sidecar-format choice; they cannot change core packing
groups or IDs. Every valid node has materialized endpoints after attachment;
partial point data is never exposed as complete.

The tree owner owns its slab and side data. Nodes, cursors, scans, and executions
borrow them. `&mut Tree` excludes active readers during attachment/removal;
immutable readers need no locks, atomics, lazy writes, or per-view reference
counts. Attach after releasing views, then borrow new views. Cancellation uses
an external flag, not synchronization in cache lookup.

```rust
let source = LineIndex::new(bytes)?;
let mut tree = packer.pack_with_options(
    &grammar,
    &native,
    PackOptions { points: false, ..PackOptions::default() },
)?;
assert!(!tree.has_points());
let root_byte = tree.root_node().start_byte();
assert_eq!(tree.root_node().start_position(), Point::new(0, root_byte));

let expected = source.point(root_byte);
let points = PointData::build(&tree, &source, None)?;
tree.set_point_data(points)?;
drop(source); // point access no longer needs input bytes or a line index
assert!(tree.has_points());
assert_eq!(tree.root_node().start_position(), expected);

tree.drop_point_data();
assert!(!tree.has_points());
assert_eq!(tree.root_node().start_position(), Point::new(0, root_byte));
```

`LoadedFile` currently shares trees through `Arc`. Attach before sharing or
publish a new owner; do not expose mutation through shared ownership. Borrowed
and transaction-backed slabs remain usable without side data, with row-zero
points. Both `Tree` and `BackedTree` expose exclusive sidecar setters and droppers.
`BackedTree` must not expose `DerefMut<Target = Tree>`: replacing its inner tree
would separate the descriptor from the backing owner. `BorrowedTree` remains
read-only; repack it into an owned tree before attaching side data. Background
publication and a public C facade are outside this step. Any later C facade must
enforce the same exclusion on the caller side.

## Serialization and loading

The in-memory sidecar representation is also its persisted representation: a
small header and pointer-free payload, with offsets relative to the sidecar's
start. Builders produce this layout directly. There is no separate encoding,
decoding, pointer fixup, or reconstructed index when loading.

```rust
impl PresenceCache {
    pub fn as_bytes(&self) -> &[u8];
    pub fn from_backing(
        tree: &Tree,
        backing: impl StableSlab,
    ) -> Result<Self, SideDataError>;
    pub fn copy_from_bytes(tree: &Tree, bytes: &[u8]) -> Result<Self, SideDataError>;
}

impl PointData {
    pub fn as_bytes(&self) -> &[u8];
    pub fn from_backing(
        tree: &Tree,
        backing: impl StableSlab,
    ) -> Result<Self, SideDataError>;
    pub fn copy_from_bytes(tree: &Tree, bytes: &[u8]) -> Result<Self, SideDataError>;
}
```

`as_bytes` borrows the existing layout without allocation. `from_backing` wraps
stable bytes without copying the payload, retaining their owner for the sidecar's
lifetime. Reuse the existing `StableSlab` ownership contract: bytes remain valid
at a stable address and immutable while retained. The LMDB backing owner holds
the read transaction; node access reads the mmap directly.

`copy_from_bytes` allocates suitably aligned storage and copies the complete
layout with a memcpy. The result has no dependency on the input bytes or LMDB
transaction. Use it when independent ownership is preferable or mapped bytes
do not meet alignment requirements. `from_backing` rejects unsuitable alignment;
it does not silently allocate and copy. Both paths produce the same public
sidecar type and use the same set/drop methods.

The caller supplies sidecar bytes corresponding to the tree and intended source.
Release loading performs only very cheap checks: recognized format, matching
group/slot/symbol counts, alignment, and payload sizes consistent with those counts, using
checked arithmetic. Do not scan bitmap words, point values, or tree nodes for
validity; do not recompute contents or checksums. Setting loaded side data must
not hide such a scan either.

Debug builds additionally scan the contents and check their invariants against
the tree. Compile these scans out of release builds using `debug_assertions`:

```rust
impl PresenceCache {
    fn validate_loaded(&self, tree: &Tree) -> Result<(), SideDataError> {
        self.validate_counts(tree)?;

        #[cfg(debug_assertions)]
        self.validate_contents(tree)?;

        Ok(())
    }
}
// PointData follows the same count-check / debug-content-scan split
```

The cheap checks do not establish content validity. Matching persisted records
to their tree/source is separate work; no new fingerprint or cache-key scheme is
specified here.

Keep presence and points in separate LMDB databases, with independent read/build/
write operations. Loading or setting a sidecar never compacts or reorders the
core. If another operation rebuilds its groups/slots, rebuild or remap the side
data to match.

Expose the same creation policy on persistence loads, including cache hits:

```rust
pub struct LoadOptions<'a> {
    pub pack: PackOptions, // added; defaults to PackOptions::default()
    pub write: WritePolicy,
    pub cancel: Option<&'a AtomicBool>,
}
```

If a requested sidecar is missing, incompatible, or rejected by the applicable
checks, the loader builds it before returning
success; construction failure is an error. Unrequested sidecars remain absent,
using ordinary symbol scanning or row-zero point access. A core-tree hit alone
does not fulfill the side-data request. Materialization occurs during the
requested creation/load operation, never as a hidden accessor fallback.
Copied sidecars own their allocation; mapped sidecars retain the LMDB read
transaction through their backing owner. Release loads do not attempt to prove
bitmap or point contents semantically correct.

Publish sidecars independently, including after the tree transaction.
`LoadedFile::evict_sidecar` selects `SidecarKind::Presence` or `SidecarKind::Points`
and deletes that persisted sidecar without retiring the core or changing existing
readers. Later loads or publishers can recreate it. Cleanup must tolerate late
writers without resurrecting
retired source generations or authoritative artifacts. Publication and cleanup
never mutate allocations held by existing readers.

## Implementation and verification

1. Move derived columns into separate allocations; retain conversion/parse flags
   as sidecar creation controls.
2. Keep source-free point access and row-zero fallback; expose availability on
   tree/node/trait/attribute APIs.
3. Add explicit point derivation, complete owned side data, and exclusive attachment.
4. Add direct mapped sidecar access, allocation-and-copy loading, and independent
   persistence with explicit point availability requirements.

Verify materialized points against explicit source conversion at zero/EOF, final
newlines, CRLF, multibyte UTF-8 byte columns, and empty/missing nodes. Invalid byte
offsets fail construction. Verify row-zero access before attachment and after
removal, including point navigation, attributes, and point-bounded queries.
Availability must be distinguishable even when materialized points also have
row zero. Point access must work after dropping the source/index.

Presence attachment/removal must preserve all results. Point attachment/removal
must preserve core bytes, IDs, and byte-based matching, while changing the
coordinate frame used by point APIs. Test aliases/supertypes, missing versus
all-zero presence, malformed sidecars, cancelled builds, failed replacement,
eviction, worker construction alongside immutable readers, setting after workers
finish, no-op drops of absent data, and invalid dimensions on load. Record the core allocation
address, serialized bytes, offsets, and IDs before setting/replacing/dropping
separately built or loaded sidecars; all must remain unchanged. Exercise every
combination of creation flags, including defaults, on conversion and parse paths.
Verify requested sidecars are present on success, disabled ones are absent, and
dropping one frees owned storage or releases mapped backing while the core and
other sidecars remain alive.
Verify mapped sidecars read the supplied payload without copying and retain its
transaction for their lifetime. Copied sidecars must remain valid after the input
backing is dropped. Check alignment rejection, relocation by memcpy, and matching
results through both loading paths. Validation remains separate from decoding;
only debug builds scan payload contents.

Test count/size rejection in both build modes. Debug builds must reject invalid
contents even when counts match; release loading must not run a content scan.

Measure explicit point materialization independently of accessor cost. The
Rust `core-lifecycle-bench` workloads `source-points`, `point-build`, and
`point-access` separate line-index construction, sidecar construction, and
traversal reading materialized endpoints. See the
[measurement contract](tools/squatter/README.md). Do not add lazy source lookup
to preserve equivalence with absent point data.
