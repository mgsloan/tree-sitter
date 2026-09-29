use crate::{
    ChildIx, FieldId, GrammarId, KindId, NamedChildIx, SlotIx, Tree,
    scan::{self, Postorder, Preorder, Scan},
    storage::*,
    traits,
    types::{GroupIx, PackedPoint},
};
use std::{marker::PhantomData, ops::Range, ptr::NonNull};
use tree_sitter::Point;

use fearless_simd::{dispatch, prelude::*, u8x32};

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct RawNode {
    // Every node borrows a live descriptor. Encoding that invariant also lets
    // Option<Node> use null for None without a separate discriminant.
    pub tree: NonNull<TreeData>,
    pub slot: SlotIx,
}

/// A single node within a syntax [`Tree`].
///
/// **Different than Tree-sitter:** Shared methods borrow the handle. Equality and hashing
/// include both tree descriptor and slot, so nodes from simultaneously live trees can be
/// used together as keys. An ID alone does not retain the tree.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Node<'tree> {
    pub(crate) raw: RawNode,
    pub(crate) lifetime: PhantomData<&'tree Tree>,
}

unsafe impl Send for Node<'_> {}
unsafe impl Sync for Node<'_> {}

impl PartialEq for Node<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.raw.tree == other.raw.tree && self.raw.slot == other.raw.slot
    }
}

impl Eq for Node<'_> {}

impl std::hash::Hash for Node<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.tree.hash(state);
        self.raw.slot.hash(state);
    }
}

impl std::fmt::Debug for Node<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Node")
            .field("slot", &self.slot().raw())
            .field("kind", &self.kind())
            .field("bytes", &self.byte_range())
            .finish()
    }
}

impl Tree {
    /// Create a new [`TreeCursor`] starting from the root of the tree.
    pub fn walk(&self) -> TreeCursor<'_> {
        self.root_node().walk()
    }

    /// Get the language that was used to parse the syntax tree.
    ///
    /// **Different than Tree-sitter:** Borrows the prepared grammar wrapper.
    pub fn language(&self) -> &crate::Language {
        &self.data().language
    }

    /// Get the root node of the syntax tree.
    pub fn root_node(&self) -> Node<'_> {
        Node {
            raw: RawNode {
                tree: self.0,
                slot: SlotIx::from_raw(self.data().group_end(self.group_count() - 1) - 1),
            },
            lifetime: PhantomData,
        }
    }

    /// Returns a node at a live physical slot; returns `None` for
    /// waste or out-of-range slots.
    ///
    /// **Not in Tree-sitter**
    pub fn node_at_slot(&self, slot: SlotIx) -> Option<Node<'_>> {
        (slot.raw() < self.slot_count() && slot.raw() < self.data().group_end(slot.group().raw()))
            .then(|| self.root_node().at(slot))
    }
}

impl<'tree> Node<'tree> {
    #[inline]
    pub(crate) fn data(self) -> &'tree TreeData {
        // Node construction is restricted to live slots in a retained tree.
        unsafe { self.raw.tree.as_ref() }
    }

    #[inline]
    pub(crate) fn at(self, slot: SlotIx) -> Self {
        Self {
            raw: RawNode { slot, ..self.raw },
            lifetime: PhantomData,
        }
    }

    #[inline]
    pub(crate) fn first_slot(self) -> u32 {
        self.data().first_slot(self.slot().raw())
    }

    /// Physical slot in reverse preorder; decreasing slots advance preorder.
    ///
    /// **Not in Tree-sitter**. Returns a physical slot scoped to this immutable tree
    /// snapshot. Slots from different trees are unrelated; repacking may change slots. Use
    /// `NodeLike::id` through [`crate::traits::NodeLike`] for generic identity.
    #[inline]
    pub fn slot(self) -> SlotIx {
        self.raw.slot
    }

    /// Get the [`crate::Language`] that was used to parse this node's syntax tree.
    ///
    /// **Different than Tree-sitter:** Borrows the prepared grammar wrapper.
    pub fn language(&self) -> &'tree crate::Language {
        &self.data().language
    }

    /// Get the range of source code that this node represents, both in terms of
    /// raw bytes and of row/column coordinates.
    ///
    /// **Different behavior than Tree-sitter:** Without point data, positions use row zero and
    /// the byte offset as column. Check `has_points()` before relying on line/column
    /// coordinates.
    pub fn range(&self) -> tree_sitter::Range {
        tree_sitter::Range {
            start_byte: self.start_byte(),
            end_byte: self.end_byte(),
            start_point: self.start_position(),
            end_point: self.end_position(),
        }
    }

    /// Returns the source slice indexed by this node’s byte offsets divided by two. Supply
    /// the UTF-16 input used to parse the tree; out-of-bounds offsets panic.
    pub fn utf16_text<'source>(&self, source: &'source [u16]) -> &'source [u16] {
        &source[self.start_byte() / 2..self.end_byte() / 2]
    }

    /// Get the byte range of source code that this node represents.
    pub fn byte_range(&self) -> Range<usize> {
        self.start_byte()..self.end_byte()
    }

    /// Returns the UTF-8 source slice for this node. Invalid UTF-8 returns an error;
    /// out-of-bounds byte offsets panic.
    pub fn utf8_text<'source>(
        &self,
        source: &'source [u8],
    ) -> Result<&'source str, std::str::Utf8Error> {
        std::str::from_utf8(&source[self.byte_range()])
    }

    /// This node and its descendants, parents before children, left to right.
    ///
    /// **Not in Tree-sitter**. Builds a subtree scan; filters and group traversal remain
    /// available.
    pub fn preorder(self) -> Scan<'tree, Preorder<'tree>> {
        Preorder::scan(self)
    }

    /// This node and its descendants, children left to right before their parent.
    ///
    /// **Not in Tree-sitter**. Builds a subtree scan in postorder.
    pub fn postorder(self) -> Scan<'tree, Postorder<'tree>> {
        Postorder::scan(self)
    }

    /// Choose the cheaper traversal. Order is unspecified across representations.
    ///
    /// **Not in Tree-sitter**. Alias for [`Self::preorder`].
    pub fn all(self) -> Scan<'tree, Preorder<'tree>> {
        self.preorder()
    }

    /// Scan this subtree in preorder, matching public kind IDs.
    ///
    /// **Not in Tree-sitter**. Scans this subtree in preorder for the selected public kind
    /// IDs. Missing presence caches affect cost, not results.
    pub fn descendants_matching_kinds<K: scan::IdSelection>(
        self,
        kinds: K,
    ) -> scan::Nodes<'tree, scan::Filtered<Preorder<'tree>, K::KindPredicate>> {
        self.preorder().filter_kind_ids(kinds).nodes()
    }

    /// This node's displayed kind ID, including aliases, compatible with Tree-sitter.
    ///
    /// Use this to compare kinds with Tree-sitter nodes or language APIs.
    /// [`Self::squatter_kind_id`] provides cheaper access for use within Squatter.
    pub fn kind_id(&self) -> KindId {
        self.data()
            .tables()
            .decode_kind(self.data().symbol_index(self.slot().raw()))
    }

    /// This node's displayed kind ID, including aliases, for use within Squatter.
    ///
    /// Cheaper to access than [`Self::kind_id`]. Use IDs from the same language
    /// version; use [`Self::kind_id`] when comparing with Tree-sitter IDs.
    pub fn squatter_kind_id(&self) -> crate::SquatterKindId {
        self.data().symbol_index(self.slot().raw())
    }

    /// This node's original grammar ID, ignoring aliases, for use within Squatter.
    ///
    /// Cheaper to access than [`Self::grammar_id`]. Use IDs from the same language
    /// version; use [`Self::grammar_id`] when comparing with Tree-sitter IDs.
    pub fn squatter_grammar_id(&self) -> crate::SquatterGrammarId {
        self.data().grammar_index(self.slot().raw())
    }

    /// This node's original grammar ID, ignoring aliases, compatible with Tree-sitter.
    ///
    /// Use this to compare original symbols with Tree-sitter nodes or grammar tables.
    /// [`Self::squatter_grammar_id`] provides cheaper access for use within Squatter.
    pub fn grammar_id(&self) -> GrammarId {
        self.data()
            .tables()
            .decode_grammar_kind(self.data().grammar_index(self.slot().raw()))
    }

    /// Get this node's type as a string.
    pub fn kind(&self) -> &'tree str {
        self.data().tables().symbol_name(self.kind_id().raw())
    }

    /// Get this node's symbol name as it appears in the grammar ignoring
    /// aliases as a string.
    pub fn grammar_name(&self) -> &'tree str {
        self.data().tables().symbol_name(self.grammar_id().raw())
    }

    /// Get the byte offset where this node starts.
    #[inline]
    pub fn start_byte(&self) -> usize {
        let data = self.data();
        (data.word(data.layout.start_byte_base, self.slot().group().raw())
            + data.byte(data.layout.start_byte_delta, self.slot().raw()) as u32) as usize
    }

    /// Get the byte offset where this node ends.
    #[inline]
    pub fn end_byte(&self) -> usize {
        let data = self.data();
        (data.word(data.layout.end_byte_base, self.slot().group().raw())
            - data.short(data.layout.end_byte_delta, self.slot().raw()) as u32) as usize
    }

    /// Get this node's start position in terms of rows and columns.
    ///
    /// **Different behavior than Tree-sitter:** Without point data, positions use row zero and
    /// the byte offset as column. Check `has_points()` before relying on line/column
    /// coordinates.
    pub fn start_position(&self) -> Point {
        self.packed_start_point().point()
    }

    /// Reports whether point data is attached. Without it, point
    /// accessors, ranges, point lookups, and point scans use `(0, byte_offset)`.
    ///
    /// **Not in Tree-sitter**
    pub fn has_points(self) -> bool {
        self.data().has_points()
    }

    /// Get this node's end position in terms of rows and columns.
    ///
    /// **Different behavior than Tree-sitter:** Without point data, positions use row zero and
    /// the byte offset as column. Check `has_points()` before relying on line/column
    /// coordinates.
    pub fn end_position(&self) -> Point {
        self.packed_end_point().point()
    }

    #[inline]
    pub(crate) fn packed_start_point(self) -> PackedPoint {
        let data = self.data();
        data.point_data.as_ref().map_or_else(
            || PackedPoint(self.start_byte() as u64),
            |points| points.start(self.slot().raw()),
        )
    }

    #[inline]
    pub(crate) fn packed_end_point(self) -> PackedPoint {
        let data = self.data();
        data.point_data.as_ref().map_or_else(
            || PackedPoint(self.end_byte() as u64),
            |points| points.end(self.slot().raw()),
        )
    }

    /// Check if this node is *named*.
    ///
    /// Named nodes correspond to named rules in the grammar, whereas
    /// *anonymous* nodes correspond to string literals in the grammar.
    #[inline]
    pub fn is_named(&self) -> bool {
        self.data()
            .tables()
            .named_index(self.data().symbol_index(self.slot().raw()))
    }

    /// Check if this node is *extra*.
    ///
    /// Extra nodes represent things like comments, which are not required by the
    /// grammar, but can appear anywhere.
    pub fn is_extra(&self) -> bool {
        self.data().flags() & EXTRAS != 0
            && self.data().bit(self.data().layout.extra, self.slot().raw())
    }

    /// Check if this node is *missing*.
    ///
    /// Missing nodes are inserted by the parser in order to recover from
    /// certain kinds of syntax errors.
    pub fn is_missing(&self) -> bool {
        self.data().flags() & MISSING != 0
            && self
                .data()
                .bit(self.data().layout.missing, self.slot().raw())
    }

    /// Check if this node represents a syntax error.
    ///
    /// Syntax errors represent parts of the code that could not be incorporated
    /// into a valid syntax tree.
    pub fn is_error(&self) -> bool {
        self.data().symbol_index(self.slot().raw()).raw() as u32 == self.data().tables().kind_count
    }

    /// Check if this node represents a syntax error or contains any syntax
    /// errors anywhere within it.
    pub fn has_error(&self) -> bool {
        self.data().flags() & ERRORS != 0
            && self.data().bit(self.data().layout.error, self.slot().raw())
    }

    /// Reads the stored field of this node in constant time. Unlike
    /// cursor field access, this is independent of the traversal root.
    ///
    /// **Not in Tree-sitter**
    pub fn field_id(self) -> Option<FieldId> {
        FieldId::from_raw(
            self.data()
                .short(self.data().layout.field, self.slot().raw()),
        )
    }

    /// Resolves the stored field through the grammar. It scans and
    /// validates the field-name string.
    ///
    /// **Not in Tree-sitter**
    pub fn field_name(self) -> Option<&'tree str> {
        self.data().tables().field_name(self.field_id()?.raw())
    }

    /// Tests membership using an original grammar symbol ID.
    ///
    /// **Not in Tree-sitter**
    pub fn has_supertype(self, symbol: GrammarId) -> bool {
        let data = self.data();
        let tables = data.tables();
        if tables.supertype_count == 0 {
            return false;
        }

        let symbols = tables.supertypes();
        let Ok(index) = symbols.binary_search(&symbol.raw()) else {
            return false;
        };
        let value = data.short(data.layout.supertype, self.slot().raw()) as usize;
        if tables.supertype_count <= 8 {
            value & (1 << index) != 0
        } else {
            let offset = value * tables.dictionary_words as usize + index / 64;
            unsafe { *tables.supertype_masks.add(offset) & (1 << (index % 64)) != 0 }
        }
    }

    /// Get the node's number of descendants, including one for the node itself.
    ///
    /// **Different performance than Tree-sitter:** Scans packed groups in this subtree.
    pub fn descendant_count(&self) -> usize {
        let first = self.first_slot();
        let waste: u32 = (first / GROUP_SIZE..self.slot().group().raw())
            .map(|group| self.data().waste(group))
            .sum();
        (self.slot().raw() - first + 1 - waste) as usize
    }

    /// Returns the next node in tree-wide preorder, which may leave
    /// this subtree.
    ///
    /// **Not in Tree-sitter**
    #[inline]
    pub fn next_preorder(self) -> Option<Self> {
        self.data()
            .previous_slot(self.slot().raw())
            .map(|slot| self.at(SlotIx::from_raw(slot)))
    }

    /// Returns the previous node in tree-wide preorder, which may
    /// leave this subtree.
    ///
    /// **Not in Tree-sitter**
    pub fn prev_preorder(self) -> Option<Self> {
        let slot = self.previous_preorder_slot();
        (slot < self.data().groups() * GROUP_SIZE).then(|| self.at(SlotIx::from_raw(slot)))
    }

    fn previous_preorder_slot(self) -> u32 {
        let slot = self.slot().raw() + 1;
        if slot < self.data().group_end(self.slot().group().raw()) {
            slot
        } else {
            (self.slot().group().raw() + 1) * GROUP_SIZE
        }
    }

    fn first_child(self) -> Option<Self> {
        self.next_preorder()
            .filter(|node| node.slot().raw() >= self.first_slot())
    }

    pub(crate) fn next_sibling_including_empty(self) -> Option<Self> {
        if self.data().bit(self.data().layout.last, self.slot().raw()) {
            None
        } else {
            // Subtree boundaries include leading waste, so the predecessor is live.
            Some(self.at(SlotIx::from_raw(self.first_slot() - 1)))
        }
    }

    /// Get this node's immediate parent.
    /// Prefer [`child_with_descendant`](Node::child_with_descendant)
    /// for iterating over this node's ancestors.
    ///
    /// **Different performance than Tree-sitter:** Can scan subsequent packed groups. A
    /// cursor retains ancestry for repeated navigation.
    pub fn parent(&self) -> Option<Self> {
        let data = self.data();
        let mut slot = self.slot().raw() + 1;
        while slot < data.groups() * GROUP_SIZE {
            let group = slot / GROUP_SIZE;
            let end = data.group_end(group);
            let maximum = data.word(data.layout.span_max, group) as u64;

            // Reject a whole group if even its largest possible span cannot
            // reach this node. The first enclosing span is the nearest parent.
            if slot as u64 <= self.slot().raw() as u64 + maximum {
                while slot < end {
                    if slot as u64
                        <= self.slot().raw() as u64 + maximum - data.span_delta(slot) as u64
                    {
                        return Some(self.at(SlotIx::from_raw(slot)));
                    }
                    slot += 1;
                }
            }
            slot = (group + 1) * GROUP_SIZE;
        }
        None
    }

    fn structural_children(self) -> Children<'tree> {
        Children {
            next: self.first_child(),
        }
    }

    /// Iterate over this node's children.
    ///
    /// A [`TreeCursor`] is used to retrieve the children efficiently. Obtain
    /// a [`TreeCursor`] by calling [`Tree::walk`] or [`Node::walk`]. To avoid
    /// unnecessary allocations, you should reuse the same cursor for
    /// subsequent calls to this method.
    ///
    /// If you're walking the tree recursively, you may want to use the
    /// [`TreeCursor`] APIs directly instead.
    ///
    /// **Different than Tree-sitter:** Returns a plain iterator with `size_hint() == (0,
    /// None)`. No initial counting pass is needed. Iteration resets and moves the supplied
    /// cursor; dropping the iterator leaves the cursor at its current position.
    pub fn children<'cursor>(
        &self,
        cursor: &'cursor mut TreeCursor<'tree>,
    ) -> impl Iterator<Item = Self> + 'cursor {
        cursor.reset(*self);
        let mut ready = cursor.goto_first_child();
        std::iter::from_fn(move || {
            if !ready {
                return None;
            }
            let node = cursor.node();
            ready = cursor.goto_next_sibling();
            Some(node)
        })
    }

    /// Iterate over this node's named children.
    ///
    /// See also [`Node::children`].
    ///
    /// **Different than Tree-sitter:** Returns a plain iterator with `size_hint() == (0,
    /// None)`. No initial counting pass is needed. Iteration resets and moves the supplied
    /// cursor; dropping the iterator leaves the cursor at its current position.
    pub fn named_children<'cursor>(
        &self,
        cursor: &'cursor mut TreeCursor<'tree>,
    ) -> impl Iterator<Item = Self> + 'cursor {
        cursor.reset(*self);
        let mut ready = cursor.goto_first_child();
        std::iter::from_fn(move || {
            if !ready {
                return None;
            }
            let original = cursor.node;
            while !cursor.node().is_named() {
                if !cursor.goto_next_sibling() {
                    // Exhausting named children leaves the cursor after the last
                    // yielded child, even when unnamed children follow it.
                    cursor.node = original;
                    ready = false;
                    return None;
                }
            }
            let node = cursor.node();
            ready = cursor.goto_next_sibling();
            Some(node)
        })
    }

    /// Iterate over this node's children with a given field id.
    ///
    /// See also [`Node::children_by_field_name`].
    ///
    /// **Different than Tree-sitter:** Returns a plain iterator with `size_hint() == (0,
    /// None)`. No initial counting pass is needed. Iteration resets and moves the supplied
    /// cursor; dropping the iterator leaves the cursor at its current position.
    pub fn children_by_field_id<'cursor>(
        &self,
        field: FieldId,
        cursor: &'cursor mut TreeCursor<'tree>,
    ) -> impl Iterator<Item = Self> + 'cursor {
        self.children(cursor)
            .filter(move |node| node.field_id() == Some(field))
    }

    /// Iterate over this node's children with a given field name.
    ///
    /// See also [`Node::children`].
    ///
    /// **Different than Tree-sitter:** Returns a plain iterator with `size_hint() == (0,
    /// None)`. No initial counting pass is needed. Iteration resets and moves the supplied
    /// cursor; dropping the iterator leaves the cursor at its current position.
    /// An unknown field name yields no children and leaves the cursor unchanged.
    pub fn children_by_field_name<'cursor>(
        &self,
        name: &str,
        cursor: &'cursor mut TreeCursor<'tree>,
    ) -> impl Iterator<Item = Self> + 'cursor {
        let field = self.data().language.field_id_for_name(name);
        let mut ready = false;
        if field.is_some() {
            cursor.reset(*self);
            ready = cursor.goto_first_child();
        }
        std::iter::from_fn(move || {
            while ready {
                let node = cursor.node();
                let matches = cursor.field_id() == field;
                ready = cursor.goto_next_sibling();
                if matches {
                    return Some(node);
                }
            }
            None
        })
    }

    /// Tests for a structural child without counting children.
    ///
    /// **Not in Tree-sitter**
    pub fn has_children(self) -> bool {
        self.first_child().is_some()
    }

    /// Scans children until finding a named child.
    ///
    /// **Not in Tree-sitter**
    pub fn has_named_children(self) -> bool {
        self.structural_children()
            .filter(|node| node.is_named())
            .next()
            .is_some()
    }

    /// Get this node's number of children.
    ///
    /// **Different performance than Tree-sitter:** Scans children.
    pub fn child_count(&self) -> ChildIx {
        ChildIx::new(self.structural_children().count() as u32)
    }

    /// Get this node's number of *named* children.
    ///
    /// See also [`Node::is_named`].
    ///
    /// **Different performance than Tree-sitter:** Scans all children.
    pub fn named_child_count(&self) -> NamedChildIx {
        NamedChildIx::new(
            self.structural_children()
                .filter(|node| node.is_named())
                .count() as u32,
        )
    }

    /// Get the node's child at the given index, where zero represents the first
    /// child.
    ///
    /// This method scans preceding children, so if
    /// you might be iterating over a long list of children, you should use
    /// [`Node::children`] instead.
    ///
    /// **Different performance than Tree-sitter:** Visits up to index + 1 children. Repeated
    /// indexed lookup across a wide node can be quadratic; prefer one traversal.
    pub fn child(&self, index: ChildIx) -> Option<Self> {
        self.structural_children().nth(index.raw() as usize)
    }

    /// Get this node's *named* child at the given index.
    ///
    /// See also [`Node::is_named`].
    /// This method scans preceding children, so if
    /// you might be iterating over a long list of children, you should use
    /// [`Node::named_children`] instead.
    ///
    /// **Different performance than Tree-sitter:** Scans preceding children, including
    /// unnamed children. Prefer one traversal for several children.
    pub fn named_child(&self, index: NamedChildIx) -> Option<Self> {
        self.structural_children()
            .filter(|node| node.is_named())
            .nth(index.raw() as usize)
    }

    /// Get this node's child with the given numerical field id.
    ///
    /// See also [`child_by_field_name`](Node::child_by_field_name). You can
    /// convert a field name to an id using [`crate::Language::field_id_for_name`].
    ///
    /// **Different performance than Tree-sitter:** Scans children.
    ///
    /// **Different behavior than Tree-sitter:** Fields stored on visible children can differ
    /// from tree-sitter lookup across alias-visible boundaries. Child enumeration uses
    /// stored fields.
    pub fn child_by_field_id(&self, field: FieldId) -> Option<Self> {
        // ERROR productions have no field map, even if descendants contributed
        // inherited fields to enumeration.
        if self.is_error() {
            None
        } else {
            self.structural_children()
                .find(|node| node.field_id() == Some(field))
        }
    }

    /// Get the first child with the given field name.
    ///
    /// If multiple children may have the same field name, access them using
    /// [`children_by_field_name`](Node::children_by_field_name)
    pub fn child_by_field_name(&self, field: impl AsRef<[u8]>) -> Option<Self> {
        let tables = self.data().tables();
        let field = (1..=tables.field_count as u16)
            .find(|index| tables.field_name(*index).map(str::as_bytes) == Some(field.as_ref()))?;
        self.child_by_field_id(FieldId::from_raw(field)?)
    }

    /// Get the node that contains `descendant`.
    ///
    /// Note that this can return `descendant` itself.
    pub fn child_with_descendant(&self, descendant: Self) -> Option<Self> {
        if self.raw.tree != descendant.raw.tree
            || descendant.slot() >= self.slot()
            || descendant.slot().raw() < self.first_slot()
        {
            return None;
        }
        self.structural_children()
            .find(|child| child.first_slot() <= descendant.slot().raw())
    }

    /// Get this node's next sibling.
    pub fn next_sibling(&self) -> Option<Self> {
        let end = self.end_byte();
        let mut next = self.next_sibling_including_empty();
        while next.is_some_and(|node| node.end_byte() <= end) {
            next = next.unwrap().next_sibling_including_empty();
        }
        next
    }

    /// Get this node's next named sibling.
    pub fn next_named_sibling(&self) -> Option<Self> {
        let end = self.end_byte();
        let mut next = self.next_sibling_including_empty();
        while next.is_some_and(|node| node.end_byte() <= end || !node.is_named()) {
            next = next.unwrap().next_sibling_including_empty();
        }
        next
    }

    /// Get this node's previous sibling.
    pub fn prev_sibling(&self) -> Option<Self> {
        self.parent()?
            .structural_children()
            .take_while(|node| *node != *self)
            .last()
    }

    /// Get this node's previous named sibling.
    pub fn prev_named_sibling(&self) -> Option<Self> {
        self.parent()?
            .structural_children()
            .take_while(|node| *node != *self)
            .filter(|node| node.is_named())
            .last()
    }

    /// Get this node's first child that contains or starts after the given byte offset.
    pub fn first_child_for_byte(&self, byte: usize) -> Option<Self> {
        let byte = byte as u32 as usize;
        self.structural_children()
            .find(|node| node.end_byte() > byte)
    }

    /// Get this node's first named child that contains or starts after the given byte offset.
    pub fn first_named_child_for_byte(&self, byte: usize) -> Option<Self> {
        let byte = byte as u32 as usize;
        self.structural_children()
            .filter(|node| node.is_named())
            .find(|node| node.end_byte() > byte)
    }

    /// Read the constant-time attributes. Counts are separate operations.
    ///
    /// **Not in Tree-sitter**. Bundles constant-time node attributes; child and descendant
    /// counts are separate.
    pub fn attributes(self) -> traits::Attributes<'tree> {
        traits::Attributes {
            kind: self.kind(),
            grammar_name: self.grammar_name(),
            kind_id: self.kind_id(),
            grammar_id: self.grammar_id(),
            start_byte: self.start_byte(),
            end_byte: self.end_byte(),
            start_position: self.start_position(),
            end_position: self.end_position(),
            has_points: self.has_points(),
            is_named: self.is_named(),
            is_extra: self.is_extra(),
            is_missing: self.is_missing(),
            is_error: self.is_error(),
            has_error: self.has_error(),
        }
    }

    /// Create a new [`TreeCursor`] starting from this node.
    ///
    /// Note that the given node is considered the root of the cursor,
    /// and the cursor cannot walk outside this node.
    ///
    /// **Different performance than Tree-sitter:** Creates an empty ancestor stack. Reuse the
    /// cursor to retain its allocation.
    pub fn walk(&self) -> TreeCursor<'tree> {
        TreeCursor {
            node: *self,
            parents: Vec::new(),
        }
    }

    /// Get the smallest node within this node that spans the given byte range.
    pub fn descendant_for_byte_range(&self, start: usize, end: usize) -> Option<Self> {
        self.seek::<false>(start as u32 as u64, end as u32 as u64, false)
    }

    /// Get the smallest named node within this node that spans the given byte range.
    pub fn named_descendant_for_byte_range(&self, start: usize, end: usize) -> Option<Self> {
        self.seek::<false>(start as u32 as u64, end as u32 as u64, true)
    }

    /// Get the smallest node within this node that spans the given point range.
    ///
    /// **Different behavior than Tree-sitter:** Without point data, positions use row zero and
    /// the byte offset as column. Check `has_points()` before relying on line/column
    /// coordinates.
    pub fn descendant_for_point_range(&self, start: Point, end: Point) -> Option<Self> {
        self.seek::<true>(
            PackedPoint::from_point_cast(start).raw(),
            PackedPoint::from_point_cast(end).raw(),
            false,
        )
    }

    /// Get the smallest named node within this node that spans the given point range.
    ///
    /// **Different behavior than Tree-sitter:** Without point data, positions use row zero and
    /// the byte offset as column. Check `has_points()` before relying on line/column
    /// coordinates.
    pub fn named_descendant_for_point_range(&self, start: Point, end: Point) -> Option<Self> {
        self.seek::<true>(
            PackedPoint::from_point_cast(start).raw(),
            PackedPoint::from_point_cast(end).raw(),
            true,
        )
    }

    #[inline]
    fn start_key<const POINTS: bool>(self) -> u64 {
        if POINTS {
            self.packed_start_point().raw()
        } else {
            self.start_byte() as u64
        }
    }

    #[inline]
    fn end_key<const POINTS: bool>(self) -> u64 {
        if POINTS {
            self.packed_end_point().raw()
        } else {
            self.end_byte() as u64
        }
    }

    // Shared empty boundaries require sibling order; the indexed search finds
    // the last qualifying start and cannot distinguish that ordering alone.
    #[inline(never)]
    fn seek_descent<const POINTS: bool>(mut self, start: u64, end: u64, named: bool) -> Self {
        let mut result = self;
        loop {
            let found = self.structural_children().find(|child| {
                let child_start = child.start_key::<POINTS>();
                let child_end = child.end_key::<POINTS>();
                child_start <= start
                    && child_end >= end
                    && if child_start == child_end {
                        child_end >= start
                    } else {
                        child_end > start
                    }
            });
            let Some(child) = found else {
                return result;
            };
            self = child;
            if !named || self.is_named() {
                result = self;
            }
        }
    }

    fn seek<const POINTS: bool>(self, start: u64, end: u64, named: bool) -> Option<Self> {
        if start > end {
            return None;
        }
        let data = self.data();
        if POINTS && !data.has_points() {
            return if start >> 32 == 0 && end >> 32 == 0 {
                self.seek::<false>(start, end, named)
            } else {
                Some(self.seek_descent::<true>(start, end, named))
            };
        }
        if start < self.start_key::<POINTS>() || end > self.end_key::<POINTS>() {
            return Some(self);
        }

        // Starts decrease in physical order. The last live slot has the
        // group's earliest start, including for independently stored points.
        let first = self.first_slot();
        let mut low = first / GROUP_SIZE;
        let mut high = self.slot().group().raw();
        while low < high {
            let middle = low + (high - low) / 2;
            let after = if POINTS {
                self.at(SlotIx::from_raw(data.group_end(middle) - 1))
                    .start_key::<true>()
                    > start
            } else {
                data.word(data.layout.start_byte_base, middle) as u64 > start
            };
            if after {
                low = middle + 1;
            } else {
                high = middle;
            }
        }

        let mut slot = (low * GROUP_SIZE).max(first);
        let limit = data.group_end(low).min(self.slot().raw() + 1);
        if POINTS {
            let group = scan::GroupRef::new(self).at_group(GroupIx(low));
            slot = (low * GROUP_SIZE + group.first_point_start_before(start, slot % GROUP_SIZE))
                .min(limit);
        } else {
            let base = data.word(data.layout.start_byte_base, low) as u64;
            let mask = start_mask(data, low, (start - base).min(255) as u8) >> (slot % GROUP_SIZE);
            slot = if mask == 0 {
                limit
            } else {
                (slot + mask.trailing_zeros()).min(limit)
            };
        }
        if slot == limit {
            slot = (low + 1) * GROUP_SIZE;
            if slot > self.slot().raw() {
                return Some(self);
            }
        }

        let mut candidate = self.at(SlotIx::from_raw(slot));
        if start == end {
            let mut previous = candidate;
            while previous.slot() <= self.slot() && previous.start_key::<POINTS>() == start {
                if previous.end_key::<POINTS>() == start {
                    return Some(self.seek_descent::<POINTS>(start, end, named));
                }
                previous = previous.at(SlotIx::from_raw(previous.previous_preorder_slot()));
            }
        }

        // Long point end scans can skip whole intervening subtrees through
        // their parent spans. Byte end scans use the compact delta columns.
        if POINTS && self.slot().raw() - candidate.slot().raw() > 512 * GROUP_SIZE {
            while candidate.slot() < self.slot() {
                let candidate_end = candidate.end_key::<true>();
                if candidate_end >= end && candidate_end > start && (!named || candidate.is_named())
                {
                    return Some(candidate);
                }
                candidate = candidate.parent().unwrap_or(self);
            }
            return Some(self);
        }

        if candidate.slot() < self.slot() {
            let candidate_end = candidate.end_key::<POINTS>();
            if candidate_end >= end && candidate_end > start && (!named || candidate.is_named()) {
                return Some(candidate);
            }
            candidate.raw.slot = SlotIx::from_raw(candidate.raw.slot.raw() + 1);
        }

        // Earlier preorder siblings end before the range. The first qualifying
        // end after the selected start is an enclosing ancestor.
        while candidate.slot() < self.slot() {
            let group = candidate.slot().group().raw();
            let limit = data.group_end(group).min(self.slot().raw());
            let view = scan::GroupRef::new(self).at_group(GroupIx(group));
            let mut first = candidate.slot().in_group().raw();
            while let Some(offset) = view.first_end_after::<POINTS>(start, end, first) {
                let slot = group * GROUP_SIZE + offset;
                if slot >= limit {
                    break;
                }
                let node = self.at(SlotIx::from_raw(slot));
                if !named || node.is_named() {
                    return Some(node);
                }
                first = offset + 1;
            }
            candidate.raw.slot = SlotIx::from_raw((group + 1) * GROUP_SIZE);
        }
        Some(self)
    }
}

fn start_mask(data: &TreeData, group: u32, threshold: u8) -> u64 {
    let deltas = data.column_slice(
        data.layout.start_byte_delta,
        (group * GROUP_SIZE) as usize,
        GROUP_SIZE as usize,
    );
    dispatch!(crate::simd::level(), simd => start_delta_mask(simd, deltas, threshold))
}

#[inline(always)]
fn start_delta_mask<S: Simd>(simd: S, deltas: &[u8], threshold: u8) -> u64 {
    u8x32::from_slice(simd, deltas)
        .simd_le(threshold)
        .to_bitmask()
}

struct Children<'tree> {
    next: Option<Node<'tree>>,
}

impl<'tree> Iterator for Children<'tree> {
    type Item = Node<'tree>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.next?;
        self.next = node.next_sibling_including_empty();
        Some(node)
    }
}

impl std::iter::FusedIterator for Children<'_> {}

/// A stateful object for walking a syntax [`Tree`] efficiently.
///
/// **Different performance than Tree-sitter:** Cloning copies cursor state and the ancestor
/// stack without copying the tree.
#[derive(Clone)]
pub struct TreeCursor<'tree> {
    node: Node<'tree>,
    parents: Vec<SlotIx>,
}

impl<'tree> TreeCursor<'tree> {
    /// Get the numerical field id of this tree cursor's current node.
    ///
    /// See also [`field_name`](TreeCursor::field_name).
    ///
    /// The traversal root has no field.
    pub fn field_id(&self) -> Option<FieldId> {
        if self.parents.is_empty() {
            None
        } else {
            self.node.field_id()
        }
    }

    /// Get the field name of this tree cursor's current node.
    ///
    /// The traversal root has no field, even when the underlying node has a field in the
    /// full tree.
    pub fn field_name(&self) -> Option<&'tree str> {
        self.field_id()
            .and_then(|field| self.node.data().tables().field_name(field.raw()))
    }

    /// Re-initialize a tree cursor to the same position as another cursor.
    ///
    /// Unlike [`reset`](TreeCursor::reset), this will not lose parent
    /// information and allows reusing already created cursors.
    ///
    /// **Different performance than Tree-sitter:** Copies the ancestor stack, reusing
    /// destination capacity where possible. The two cursors move independently and borrow
    /// their trees.
    pub fn reset_to(&mut self, cursor: &Self) {
        self.node = cursor.node;
        self.parents.clone_from(&cursor.parents);
    }

    /// Reads the current node’s bundled attributes.
    ///
    /// **Not in Tree-sitter**
    pub fn attributes(&mut self) -> traits::Attributes<'tree> {
        self.node.attributes()
    }

    /// Get the tree cursor's current [`Node`].
    pub fn node(&self) -> Node<'tree> {
        self.node
    }

    pub(crate) fn parent_node(&self) -> Option<Node<'tree>> {
        self.parents.last().map(|slot| self.node.at(*slot))
    }

    /// Re-initialize this tree cursor to start at the given node.
    pub fn reset(&mut self, node: Node<'tree>) {
        self.node = node;
        self.parents.clear();
    }

    /// Get the depth of the cursor's current node relative to the original
    /// node that the cursor was constructed with.
    pub fn depth(&self) -> u32 {
        self.parents.len() as u32
    }

    /// Move this cursor to the first child of its current node.
    ///
    /// This returns `true` if the cursor successfully moved, and returns
    /// `false` if there were no children.
    pub fn goto_first_child(&mut self) -> bool {
        let Some(child) = self.node.first_child() else {
            return false;
        };
        self.parents.push(self.node.slot());
        self.node = child;
        true
    }

    /// Move this cursor to the last child of its current node.
    ///
    /// This returns `true` if the cursor successfully moved, and returns
    /// `false` if there were no children.
    ///
    /// Note that this function may be slower than
    /// [`goto_first_child`](TreeCursor::goto_first_child) because it needs to
    /// iterate through all the children to compute the child's position.
    pub fn goto_last_child(&mut self) -> bool {
        if !self.goto_first_child() {
            return false;
        }
        while self.goto_next_sibling() {}
        true
    }

    /// Move this cursor to the next sibling of its current node.
    ///
    /// This returns `true` if the cursor successfully moved, and returns
    /// `false` if there was no next sibling node.
    ///
    /// Note that the node the cursor was constructed with is considered the root
    /// of the cursor, and the cursor cannot walk outside this node.
    pub fn goto_next_sibling(&mut self) -> bool {
        if self.parents.is_empty() {
            return false;
        }
        let Some(next) = self.node.next_sibling_including_empty() else {
            return false;
        };
        self.node = next;
        true
    }

    /// Move this cursor to the parent of its current node.
    ///
    /// This returns `true` if the cursor successfully moved, and returns
    /// `false` if there was no parent node (the cursor was already on the
    /// root node).
    ///
    /// Note that the node the cursor was constructed with is considered the root
    /// of the cursor, and the cursor cannot walk outside this node.
    pub fn goto_parent(&mut self) -> bool {
        let Some(slot) = self.parents.pop() else {
            return false;
        };
        self.node = self.node.at(slot);
        true
    }

    /// Move this cursor to the previous sibling of its current node.
    ///
    /// This returns `true` if the cursor successfully moved, and returns
    /// `false` if there was no previous sibling node.
    ///
    /// Note, that this function may be slower than
    /// [`goto_next_sibling`](TreeCursor::goto_next_sibling) due to how node
    /// positions are stored. In the worst case, this will need to iterate
    /// through all the children up to the previous sibling node to recalculate
    /// its position. Also note that the node the cursor was constructed with is
    /// considered the root of the cursor, and the cursor cannot walk outside this node.
    pub fn goto_previous_sibling(&mut self) -> bool {
        let previous = self.parent_node().and_then(|parent| {
            parent
                .structural_children()
                .take_while(|node| *node != self.node)
                .last()
        });
        let Some(previous) = previous else {
            return false;
        };
        self.node = previous;
        true
    }

    /// Move this cursor to the first child of its current node that contains or
    /// starts after the given byte offset.
    ///
    /// This returns the index of the child node if one was found, and returns
    /// `None` if no such child was found.
    ///
    /// **Different performance than Tree-sitter:** Scans children.
    pub fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<ChildIx> {
        let byte = byte as u32 as usize;
        self.goto_child_matching(|node| {
            node.end_byte() > byte && node.end_position() > Point::default()
        })
    }

    /// Move this cursor to the first child of its current node that contains or
    /// starts after the given point.
    ///
    /// This returns the index of the child node if one was found, and returns
    /// `None` if no such child was found.
    ///
    /// **Different behavior than Tree-sitter:** Without point data, positions use row zero and
    /// the byte offset as column. Check `has_points()` before relying on line/column
    /// coordinates.
    ///
    /// **Different performance than Tree-sitter:** Scans children.
    pub fn goto_first_child_for_point(&mut self, point: Point) -> Option<ChildIx> {
        let point = PackedPoint::from_point_cast(point).point();
        self.goto_child_matching(|node| node.end_byte() > 0 && node.end_position() > point)
    }

    fn goto_child_matching(
        &mut self,
        mut matches: impl FnMut(Node<'tree>) -> bool,
    ) -> Option<ChildIx> {
        let (index, child) = self
            .node
            .structural_children()
            .enumerate()
            .find(|(_, node)| matches(*node))?;
        self.parents.push(self.node.slot());
        self.node = child;
        Some(ChildIx::new(index as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_masks_match_scalar() {
        for level in crate::simd::test_levels() {
            dispatch!(level, simd => check_start_masks(simd));
        }
    }

    #[inline(always)]
    fn check_start_masks<S: Simd>(simd: S) {
        let mut storage = [0u8; 64];
        for offset in 0..32 {
            let bytes = &mut storage[offset..offset + 32];
            for (slot, byte) in bytes.iter_mut().enumerate() {
                *byte = (slot * 8 + offset) as u8;
            }
            for threshold in 0..=u8::MAX {
                let expected = bytes.iter().enumerate().fold(0, |mask, (slot, value)| {
                    mask | (u64::from(*value <= threshold) << slot)
                });
                assert_eq!(start_delta_mask(simd, bytes, threshold), expected);
            }
        }
    }
}
