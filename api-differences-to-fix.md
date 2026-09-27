# API differences to fix

Tree-squatter should follow tree-sitter's Rust API wherever it implements the
same operation. Differences should be limited to:

- Additional capabilities, such as packing, persistence, scans, and side data.
- Behavior needed for those capabilities, such as operating without a point cache.
- Necessary representation changes, such as a wrapper around `Language`.
- Newtype wrappers around primitive values, including the planned `ChildIx(u32)`,
  `NamedChildIx(u32)` and `DescendantIx(u32)`.

This proposal follows [the API comparison](api-comparison.md) and the current
Rust implementations. Each group shows current tree-sitter, current tree-squatter,
and proposed tree-squatter APIs, followed by the changes. Declaration blocks are
selected outlines, not compilable definitions; bodies, unrelated methods, and
some generic/lifetime detail are omitted. Proposed names describe a target, not
implemented functionality.

Except for explicitly excluded capabilities such as edit registration and change
tracking, missing features remain gaps even when they require substantial work. Different
implementation details or naming preferences do not justify changing a shared
contract. Additions should extend the API without displacing shared operations.

Parser lifecycle, options, backend selection, the shared parser trait, and excluded
change tracking are covered in [parser API design](parser-api-design.md). Query
APIs, result types, behavior, and scan selection are covered in
[query revamp](query-revamp.md).

Review the cost of missing APIs before committing to add them. The proposed
conveniences below are candidates, not a requirement to reproduce every method
regardless of cost.

## Cost review for scan-based APIs

- Tree-sitter does not promise O(1) indexed child lookup. Its Rust documentation
  describes `child(i)` and `named_child(i)` as logarithmic; the implementation
  traverses internal children and uses stored visible/named counts to skip hidden
  subtrees. This is not direct indexing into an array of visible children.
- Tree-squatter's `child(i)` walks preceding children, taking O(i + 1) for an
  in-range index. Named lookup also visits intervening unnamed children.
- Tree-sitter's child counts are stored and O(1). Tree-squatter's counts scan
  children and take O(number of children).
- Tree-sitter's `field_name_for_child` also traverses to the child and resolves
  field mappings; it is not an O(1) lookup. A straightforward packed equivalent
  would scan to the child, then use its stored field ID. Reading `field_id()` on
  an already available packed child is O(1); reading its name additionally scans
  and validates the grammar's field-name string.
- Repeated indexed lookup across every packed child can take quadratic time.
  Prefer one children traversal and direct field access. Offer
  `field_name_for_child` and `field_name_for_named_child` only through `NodeLike`,
  documenting their scanning cost; do not add inherent packed-node methods.
- Return plain `Iterator`s from child enumeration. Do not count children up
  front to implement `ExactSizeIterator`. Use `size_hint() == (0, None)`;
  callers that need a count can explicitly consume the iterator with `count()`.

These observations come from [tree-sitter's node implementation](lib/src/node.c),
[its Rust binding](lib/binding_rust/lib.rs), and
[tree-squatter's node implementation](crates/squatter/src/node.rs). They describe
implementation costs, not measured runtime differences. For each costly API,
decide whether to add it with explicit cost documentation, provide an index/cache,
or defer it and point callers to iteration. Compatibility alone does not settle
that decision.

## Language and typed identifiers

**Current tree-sitter**

```rust
impl Language {
    pub fn id_for_node_kind(&self, kind: &str, named: bool) -> u16;
    pub fn node_kind_for_id(&self, id: u16) -> Option<&str>;
    pub fn field_id_for_name(&self, name: impl AsRef<[u8]>) -> Option<NonZeroU16>;
    pub fn field_name_for_id(&self, id: u16) -> Option<&str>;
}
```

**Current tree-squatter**

```rust
impl Language {
    pub fn new(language: &tree_sitter::Language) -> Result<Self, Error>;
    pub fn tree_sitter_language(&self) -> tree_sitter::Language;
    pub fn kind_id_for_name(&self, name: &str, named: bool) -> Option<KindId>;
    pub fn grammar_kind_id_for_name(&self, name: &str, named: bool)
        -> Option<GrammarKindId>;
    pub fn field_id_for_name(&self, name: &str) -> Option<FieldId>;
}
```

**Proposed tree-squatter**

```rust
impl Language {
    pub fn new(language: &tree_sitter::Language) -> Result<Self, Error>;
    pub fn tree_sitter_language(&self) -> tree_sitter::Language;
    pub fn id_for_node_kind(&self, kind: &str, named: bool) -> KindId;
    pub fn node_kind_for_id(&self, id: KindId) -> Option<&str>;
    pub fn field_id_for_name(&self, name: impl AsRef<[u8]>) -> Option<FieldId>;
    pub fn field_name_for_id(&self, id: FieldId) -> Option<&str>;

    pub fn kind_id_for_name(&self, name: &str, named: bool) -> Option<KindId>;
    pub fn grammar_kind_id_for_name(&self, name: &str, named: bool)
        -> Option<GrammarKindId>;
}
```

- Keep the prepared `Language` wrapper and domain-specific newtypes.
- `tree_sitter_language()` returns the underlying tree-sitter language.
- Add the shared lookup names, preserving tree-sitter's zero sentinel as
  `KindId::new(0)` on unsuccessful `id_for_node_kind` lookup.
- Keep checked lookup and underlying grammar-kind lookup as additions.
- Accept byte-like field names, as tree-sitter does.
- Forward other language metadata through the wrapper; retain grammar caching
  and hashing as additions.

## Node receivers, identity, and child lookup

**Current tree-sitter**

```rust
impl<'tree> Node<'tree> {
    pub fn id(&self) -> usize;
    pub fn kind_id(&self) -> u16;
    pub fn grammar_id(&self) -> u16;
    pub fn child(&self, index: u32) -> Option<Self>;
    pub fn named_child(&self, index: u32) -> Option<Self>;
    pub fn child_count(&self) -> u32;
    pub fn named_child_count(&self) -> usize;
    pub fn child_by_field_name(&self, name: impl AsRef<[u8]>) -> Option<Self>;
}
```

**Current tree-squatter**

```rust
impl<'tree> Node<'tree> {
    pub fn slot(self) -> SlotIx;
    pub fn kind_id(self) -> KindId;
    pub fn grammar_id(self) -> GrammarKindId;
    pub fn child(self, index: usize) -> Option<Self>;
    pub fn named_child(self, index: usize) -> Option<Self>;
    pub fn child_count(self) -> usize;
    pub fn named_child_count(self) -> usize;
    pub fn child_by_field_name(self, name: &str) -> Option<Self>;
}
```

**Proposed tree-squatter**

```rust
pub struct ChildIx(u32);
pub struct NamedChildIx(u32);
pub struct DescendantIx(u32);
// each index type provides new(u32) and get() -> u32
impl ChildIx {
    pub const fn new(value: u32) -> Self;
    pub const fn get(self) -> u32;
}

impl<'tree> Node<'tree> {
    pub fn slot(self) -> SlotIx;
    pub fn kind_id(&self) -> KindId;
    pub fn grammar_id(&self) -> GrammarKindId;
    pub fn child(&self, index: ChildIx) -> Option<Self>;
    pub fn named_child(&self, index: NamedChildIx) -> Option<Self>;
    pub fn children<'cursor>(&self, cursor: &'cursor mut TreeCursor<'tree>)
        -> impl Iterator<Item = Self> + 'cursor where 'tree: 'cursor;
    pub fn named_children<'cursor>(&self, cursor: &'cursor mut TreeCursor<'tree>)
        -> impl Iterator<Item = Self> + 'cursor where 'tree: 'cursor;
    pub fn child_count(&self) -> ChildIx;
    pub fn named_child_count(&self) -> NamedChildIx;
    pub fn child_by_field_name(&self, name: impl AsRef<[u8]>) -> Option<Self>;
}
```

- Match `&self` receivers on shared node methods, including those omitted here.
  Copyable handles hide the difference at ordinary call sites, but not in method
  references or generic interfaces.
- Use `ChildIx(u32)` for indices among all children and `NamedChildIx(u32)`
  for indices among named children. The same integer can select different nodes
  in these two domains; do not implicitly convert between them.
- Use `DescendantIx(u32)` for preorder descendant indices relative to a traversal
  root, with zero identifying that root. Keep it distinct from physical slots.
- These newtypes are intentional differences from tree-sitter's primitive indices.
  Their widths follow enforced representation limits, not a bound inferred from
  source byte length.
- Return `ChildIx` from `child_count()` and `NamedChildIx` from
  `named_child_count()`. These values are exclusive upper bounds in their
  respective index domains, not valid child positions themselves. An empty
  sequence returns the corresponding wrapper around zero.
- Use `.get()` when a primitive count is needed. These wrappers do not imply that
  a position exists, and remain distinct from descendant indices and physical slots.
- Keep `slot()` as the inherent identity/addressing operation. Do not add an
  inherent `Node::id()` or a `NodeId` type; shared identity goes through
  `NodeLike::id()`.
- Broaden field-name input without changing lookup behavior.

## Node identity

Do not add an inherent `Node::id()` or a separate `NodeId` type. For tree-squatter,
`NodeLike::id()` returns `self.slot()` directly, preserving `SlotIx`. Use an
associated `Id` type so the tree-sitter implementation can return its native
`usize` identity without truncation:

```rust
impl<'tree> NodeLike<'tree> for tree_squatter::Node<'tree> {
    type Id = SlotIx;
    fn id(&self) -> Self::Id {
        self.slot()
    }
    // other associated types and methods omitted
}

impl<'tree> NodeLike<'tree> for tree_sitter::Node<'tree> {
    type Id = usize;
    fn id(&self) -> Self::Id {
        tree_sitter::Node::id(self)
    }
    // other associated types and methods omitted
}
```

- Slot identity is scoped to one immutable tree snapshot. Equal slots from
  different trees do not imply equal nodes, and repacking can change slots.
- Existing `Node` equality and hashing include the tree descriptor and slot;
  borrowed nodes can serve as keys spanning simultaneously live trees.
- The shared trait promises identity within a tree, not preservation across edits,
  reloads, or repacking. Tree-sitter's stronger guarantee for incrementally reused
  nodes is backend-specific.
- Copying an ID does not keep its tree alive. Calling `.id()` on a packed node
  requires the `NodeLike` trait to be in scope; its inherent accessor is `.slot()`.

## Children and cursor reuse

**Current tree-sitter**

```rust
let root = tree.root_node();
let mut cursor: TreeCursor<'_> = root.walk();
let children = root.children(&mut cursor);
let named = root.named_children(&mut cursor);
let by_id = root.children_by_field_id(field.get(), &mut cursor);
let by_name = root.children_by_field_name("body", &mut cursor);
let field_name = root.field_name_for_child(0);
let named_field_name = root.field_name_for_named_child(0);
```

**Current tree-squatter**

```rust
let root = tree.root_node();
let mut cursor: Cursor<'_> = root.walk()?;
let children = root.children();
let named = root.named_children();
let by_id = root.children_by_field_id(field);
// no children_by_field_name or field_name_for_[named_]child

let field_name = child.field_name();
```

**Proposed tree-squatter**

```rust
let root = tree.root_node();
let mut cursor: TreeCursor<'_> = root.walk();
let children = root.children(&mut cursor);
let named = root.named_children(&mut cursor);
let by_id = root.children_by_field_id(field, &mut cursor);
let by_name = root.children_by_field_name("body", &mut cursor);
let field_name = NodeLike::field_name_for_child(&root, ChildIx::new(0));
let named_field_name = NodeLike::field_name_for_named_child(&root, NamedChildIx::new(0));

let field_name = child.field_name();
```

- Rename the corresponding cursor type to `TreeCursor`.
- Make `walk()` infallible: it currently creates an empty ancestor vector and
  always returns `Ok`.
- Restore cursor arguments and cursor side effects for shared child methods.
  Return plain iterators with `size_hint() == (0, None)`, including named and
  field-filtered enumeration; do not promise `ExactSizeIterator`. Each iterator above is an independent example; consume
  or drop it before borrowing the cursor again.
- Add name-based enumeration. Add child-index field-name accessors only to
  `NodeLike`, taking `ChildIx` or `NamedChildIx` respectively. Keep them off the
  inherent packed-node API to encourage traversal and direct child field access.
- Keep direct node field inspection as an addition.
- Do not add `children_iter()` or cursor-free named/field variants. Use the shared
  cursor-taking child methods or manually walk the cursor. Both avoid an initial
  counting pass. Reusing a cursor also avoids repeated ancestor-vector allocation.

## Cursor inspection and movement

**Current tree-sitter**

```rust
impl<'tree> TreeCursor<'tree> {
    pub fn field_id(&self) -> Option<NonZeroU16>;
    pub fn field_name(&self) -> Option<&'tree str>;
    pub fn descendant_index(&self) -> usize;
    pub fn goto_descendant(&mut self, index: usize);
    pub fn reset_to(&mut self, cursor: &Self);
    pub fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<usize>;
    pub fn goto_first_child_for_point(&mut self, point: Point) -> Option<usize>;
}

impl Clone for TreeCursor<'_> { /* independent cursor state */ }
```

**Current tree-squatter**

```rust
impl<'tree> Cursor<'tree> {
    pub fn node(&self) -> Node<'tree>;
    pub fn depth(&self) -> u32;
    pub fn reset(&mut self, node: Node<'tree>);
    pub fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<usize>;
    pub fn goto_first_child_for_point(&mut self, point: Point) -> Option<usize>;
}

// field_id is available through CursorLike or cursor.node().field_id()
// no inherent field_name, descendant_index, goto_descendant, reset_to, or Clone
```

**Proposed tree-squatter**

```rust
impl<'tree> TreeCursor<'tree> {
    pub fn field_id(&self) -> Option<FieldId>;
    pub fn field_name(&self) -> Option<&'tree str>;
    pub fn descendant_index(&self) -> DescendantIx;
    pub fn goto_descendant(&mut self, index: DescendantIx);
    pub fn reset_to(&mut self, cursor: &Self);
    pub fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<ChildIx>;
    pub fn goto_first_child_for_point(&mut self, point: Point) -> Option<ChildIx>;
}

impl Clone for TreeCursor<'_> { /* independent cursor state */ }
```

- Expose shared field access as inherent methods, without requiring a trait import.
- Add descendant navigation using `DescendantIx`, relative to the cursor's
  traversal root. A physical slot is not a descendant index. Resetting the cursor
  to a different root changes the index's interpretation.
- Return `ChildIx` from child-positioning methods, consistently with child lookup.
- Include `Clone` and `reset_to` in this API pass. Copy the current node and
  ancestor stack so traversal state is independent; `reset_to` should reuse the
  destination stack allocation where possible. Neither operation copies the tree.
- Retain existing movement methods and additional bundled attribute access.

## Tree views, coordinates, and source text

**Current tree-sitter**

```rust
impl Tree {
    pub fn walk(&self) -> TreeCursor<'_>;
    pub fn language(&self) -> LanguageRef<'_>;
    pub fn root_node_with_offset(&self, bytes: usize, extent: Point) -> Node<'_>;
}
impl<'tree> Node<'tree> {
    pub fn language(&self) -> LanguageRef<'tree>;
    pub fn range(&self) -> Range;
    pub fn to_sexp(&self) -> String;
    pub fn utf16_text<'source>(&self, source: &'source [u16]) -> &'source [u16];
    pub fn has_error(&self) -> bool;
}
impl Clone for Tree { /* shared underlying tree storage */ }
```

**Current tree-squatter**

```rust
impl Tree {
    pub fn root_node(&self) -> Node<'_>;
    pub fn has_points(&self) -> bool;
}
impl<'tree> Node<'tree> {
    pub fn start_position(self) -> Point;
    pub fn end_position(self) -> Point;
    pub fn has_points(self) -> bool;
    pub fn has_error(self) -> bool;
}
// no Tree::walk, tree/node language access, offset view, range, to_sexp,
// utf16_text, or Tree::clone
```

**Proposed tree-squatter**

```rust
impl Tree {
    pub fn walk(&self) -> TreeCursor<'_>;
    pub fn language(&self) -> &Language;
    pub fn has_points(&self) -> bool;
}
impl<'tree> Node<'tree> {
    pub fn language(&self) -> &'tree Language;
    pub fn range(&self) -> tree_sitter::Range;
    pub fn to_sexp(&self) -> String;
    pub fn utf16_text<'source>(&self, source: &'source [u16]) -> &'source [u16];
    pub fn has_error(&self) -> bool;
    pub fn has_points(self) -> bool;
}
impl Clone for Tree { /* preserve tree contents and optional side data */ }
```

- Add navigation and formatting conveniences with tree-sitter semantics.
- Return the necessary grammar wrapper from language accessors. The proposed
  borrow exposes metadata without requiring an owned language clone.
- Exclude `root_node_with_offset` and offset views. Nodes report coordinates
  stored in the tree; no per-view coordinate translation is planned.
- Preserve optional point data. Without it, document the existing row-zero,
  byte-as-column behavior across all point-dependent APIs.
- Add cloning with compatible observable semantics. Copying cost may differ;
  preserve side data and keep fallible copying/detachment as additions.

## Shared navigation traits

**Current tree-sitter**

```rust
// concrete APIs; tree-sitter does not define these shared traits
let root = tree.root_node();
let identity = root.id();
let mut cursor = root.walk();
let children = root.children(&mut cursor);
```

**Current tree-squatter**

```rust
pub trait TreeLike {
    type Node<'tree>: NodeLike<'tree> where Self: 'tree;
    fn root(&self) -> Self::Node<'_>;
}
pub trait NodeLike<'tree>: Copy + Eq {
    type Cursor: CursorLike<'tree, Node = Self>;
    fn identity(self) -> usize;
    fn cursor(self) -> Result<Self::Cursor, Error>;
    fn children(self) -> impl Iterator<Item = Self>;
    fn child_count(self) -> usize;
}
```

**Proposed tree-squatter**

```rust
pub trait TreeLike {
    type Node<'tree>: NodeLike<'tree> where Self: 'tree;
    fn root_node(&self) -> Self::Node<'_>;
}
pub trait NodeLike<'tree>: Copy + Eq {
    type Cursor: CursorLike<'tree, Node = Self>;
    type Id: Copy + Eq + std::hash::Hash;
    fn id(&self) -> Self::Id;
    fn walk(&self) -> Self::Cursor;
    fn children<'cursor>(&self, cursor: &'cursor mut Self::Cursor)
        -> impl Iterator<Item = Self> + 'cursor where Self: 'cursor;
    fn child(&self, index: ChildIx) -> Option<Self>;
    fn named_child(&self, index: NamedChildIx) -> Option<Self>;
    fn child_count(&self) -> ChildIx;
    fn named_child_count(&self) -> NamedChildIx;
    fn field_name_for_child(&self, index: ChildIx) -> Option<&'tree str>;
    fn field_name_for_named_child(&self, index: NamedChildIx) -> Option<&'tree str>;
}
```

- Keep shared traits as an additional capability with implementations for both
  representations. `NodeLike::id()` returns `SlotIx` from `slot()` for packed nodes
  and the native `usize` ID for tree-sitter nodes.
- Align names, receivers, and contracts with the corresponding inherent methods,
  including distinct child/named-child indices and typed `CursorLike` positioning
  results.
- Update both implementations together. Retain subtree scans as additions, but
  do not add a separate cursor-free children interface to the trait.
- Expose indexed field-name lookup through the trait only for packed nodes;
  the tree-sitter implementation can forward to its inherent accessors.

## Packed storage, scans, and side data

**Current tree-sitter**

```rust
let tree = parser.parse(source, None).unwrap();
let start = tree.root_node().start_position();
// no packed slabs, attachable caches, or scan-builder API
```

**Current tree-squatter**

```rust
let mut tree = Tree::pack(&language, &native_tree)?;
let slab = tree.as_bytes();
let borrowed = Tree::from_bytes_borrowed(&language, slab)?;
let owned = Tree::from_owned_slab(&language, backing)?;
let count = tree.root_node().preorder().filter_kind_ids([kind]).count();

let lines = LineIndex::new(source)?;
let points = PointData::build(&tree, &lines, None)?;
tree.set_point_data(points)?;
tree.drop_presence_cache();
tree.drop_point_data();
```

**Proposed tree-squatter**

```rust
let mut tree = Tree::pack(&language, &native_tree)?;
let slab = tree.as_bytes();
let borrowed = Tree::from_bytes_borrowed(&language, slab)?;
let owned = Tree::from_owned_slab(&language, backing)?;
let count = tree.root_node().preorder().filter_kind_ids([kind]).count();

let lines = LineIndex::new(source)?;
let points = PointData::build(&tree, &lines, None)?;
tree.set_point_data(points)?;
tree.drop_presence_cache();
tree.drop_point_data();
```

- Retain these additions, including backing alignment, lifetime, and immutability
  requirements. Release borrowed views before mutating their owning tree.
- Retain packing options, reusable scratch, compaction, detachment, grammar caches,
  postorder/reverse/range scans, masks, ID sets, supertype tests, and attributes.
- Keep optional side data separate from slab bytes. Loading a slab does not
  implicitly restore separately persisted caches.
- Missing presence caches may change cost, never results. Missing point data is
  an explicit capability state with documented coordinate behavior.

## Behavior differences

These are current differences and proposed dispositions, not results of exhaustive
new differential testing.

- **Missing points:** tree-squatter returns row zero with byte offset as column.
  Keep optional points and document their effect on accessors, ranges, lookups,
  and scans.
- **Cache loading:** slab loading does not restore separate side data. Retain
  this behavior and expose cache availability.
- **Missing presence cache:** scanning still works. Preserve identical results
  with or without the cache.
- **Unknown kind:** checked tree-squatter lookup returns `None`; tree-sitter
  returns zero. Add the shared sentinel-based lookup while retaining the checked
  addition.
- **Syntax errors and grammars:** direct parsing rejects syntax errors and requires
  ABI 15 without external scanners or nonterminal extras. Preserve restrictions
  only on the explicit direct-parser extension; they remain gaps for `Parser`.
- **Change tracking:** remove the current always-false `has_changes()`.
  Tree-squatter intentionally omits edit registration, changed-range reporting,
  and incremental reuse; parsed trees are fresh snapshots.
- **Coordinate narrowing:** cast byte offsets and point components in shared
  node/cursor lookups with `as u32` wherever tree-sitter's Rust wrapper does.
  Do not check narrowing conversions. Retain existing wider-coordinate behavior
  in squatter-only scan APIs.
- **Identity and ownership:** numerical identities differ across representations,
  repacking can change slots, and borrowed trees depend on their backing storage.
  Retain those necessary differences with explicit scopes and lifetimes.

Allocation sizes, traversal cost, and copying cost can differ without changing
results.

## Commit plan

1. **API implementation commits.** Split changes into coherent commits: names,
   receivers, index newtypes, cursor construction, and setters; navigation conveniences
   approved by the cost review; behavior fixes. Query work is tracked separately
   in [query revamp](query-revamp.md).
   Update callers and relevant tests with each change. Remove `has_changes()`;
   exclude edit registration and incremental reuse. Resolve the open decisions in
   [parser API design](parser-api-design.md) before implementing those larger
   targets. Record deferred APIs explicitly.
2. **Documentation-copy commit.** After the selected API changes, copy the
   corresponding tree-sitter documentation onto shared tree-squatter types and
   methods. Use this checkout's Rust binding as the source and record its revision
   in the commit body. Keep this commit focused on copying documentation, with no
   implementation changes or editorial rewrites. Make only necessary accuracy
   corrections, including broken links and examples requiring the grammar wrapper
   or newtypes; identify those corrections in the commit body. Do not copy a claim
   that is false for the implemented API.
3. **Tree-squatter documentation commit.** Add clearly labeled notes for remaining
   differences and document additional APIs. Preserve the copied documentation
   untouched unless it is inaccurate. Append notes as separate paragraphs rather
   than weaving tree-squatter commentary into upstream prose. Keep this commit
   separate from both API implementation and the documentation copy.

Use these labels consistently in Rust documentation:

- **Tree-squatter behavior change:** for an observable result or contract
  difference, including the behavior when point data is absent.
- **Tree-squatter only:** for an additional API or capability with no tree-sitter
  counterpart.
- **Tree-squatter API difference:** for an intentional signature/type difference,
  such as `ChildIx` or the grammar wrapper.
- **Tree-squatter performance difference:** for a different complexity or cost,
  such as scanning to count children. Do not imply a measured slowdown without
  measurements.

For example, these are separate additions to the corresponding API docs:

```rust
/// **Tree-squatter API difference:** Takes `ChildIx` instead of `u32`.

/// **Tree-squatter performance difference:** Scans preceding children. Prefer
/// child iteration when visiting several children.

/// **Tree-squatter behavior change:** Without point data, returns row zero with
/// the byte offset as the column. Check `has_points()` before using line/column
/// coordinates.

/// **Tree-squatter only:** Returns this node's physical slot in the packed tree.
```

An appended note must not contradict the copied text. For example, replace an
inaccurate logarithmic-cost claim with the actual scanning cost; preserve the
rest of that method's documentation. Additional APIs need original documentation
and the `tree-squatter only` label, not an artificial upstream counterpart.

## Verification

Use existing navigation, binding, and boundary tests. Normalize newtypes and
representation-specific identities. Cover optional side data, malformed packed
input trees, empty and missing nodes, aliases, and range boundaries. Query
verification is tracked in [query revamp](query-revamp.md).

For the documentation commits, check rendered rustdoc, intra-doc links, and
affected doctests. Review the copy commit against its recorded source revision,
then verify that the annotation commit changes copied prose only where needed
for accuracy. Documentation must describe the implemented API, not unimplemented
targets from this proposal.

The target is shared call sites that need only necessary grammar-wrapper and
newtype adaptations. Additional capabilities should remain available without
forcing unrelated changes to shared calls or behavior.
