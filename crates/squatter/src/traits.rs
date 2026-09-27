//! Shared, statically dispatched navigation for mainline and packed trees.
//!
//! Request a whole scan so each backend can choose its traversal:
//! ```
//! use tree_squatter::{KindSet, traits::NodeLike};
//!
//! fn matching_bytes<'tree, N: NodeLike<'tree>>(root: N, kinds: &KindSet) -> usize {
//!     root.descendants_matching_kinds(kinds)
//!         .map(|node| node.byte_range().len())
//!         .sum()
//! }
//! ```
use crate::{
    ChildIx, FieldId, GrammarKindId, KindId, NamedChildIx, Node, SlotIx, Tree, TreeCursor,
    scan::IdSelection,
};
use std::ops::Range;
use tree_sitter::Point;

/// Constant-time attributes supported by both representations on freshly parsed trees.
/// Child and descendant counts are separate node operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attributes<'tree> {
    pub kind: &'tree str,
    pub grammar_name: &'tree str,
    pub kind_id: KindId,
    pub grammar_id: GrammarKindId,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_position: Point,
    pub end_position: Point,
    pub has_points: bool,
    pub is_named: bool,
    pub is_extra: bool,
    pub is_missing: bool,
    pub is_error: bool,
    pub has_error: bool,
}

pub trait TreeLike {
    type Node<'tree>: NodeLike<'tree>
    where
        Self: 'tree;
    fn root_node(&self) -> Self::Node<'_>;
}

pub trait NodeLike<'tree>: Copy + Eq {
    type Cursor: CursorLike<'tree, Node = Self>;
    /// Stable within this tree; not comparable across representations.
    type Id: Copy + Eq + std::hash::Hash;
    fn id(&self) -> Self::Id;
    /// Read constant-time attributes; counts are separate operations below.
    fn attributes(self) -> Attributes<'tree>;
    fn kind_id(&self) -> KindId;
    fn grammar_id(&self) -> GrammarKindId;
    fn kind(&self) -> &'tree str;
    fn grammar_name(&self) -> &'tree str;
    fn byte_range(&self) -> Range<usize>;
    fn start_byte(&self) -> usize;
    fn end_byte(&self) -> usize;
    fn start_position(&self) -> Point;
    fn end_position(&self) -> Point;
    fn has_points(self) -> bool;
    fn is_named(&self) -> bool;
    fn is_extra(&self) -> bool;
    fn is_missing(&self) -> bool;
    fn is_error(&self) -> bool;
    fn has_error(&self) -> bool;
    /// Preorder including this node, using the backend's native traversal.
    fn preorder(self) -> impl Iterator<Item = Self>;
    /// Public kind IDs, in preorder including this node. Never leaves its subtree.
    fn descendants_matching_kinds<K: IdSelection>(self, kinds: K) -> impl Iterator<Item = Self>;
    fn children<'cursor>(
        &self,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor;
    fn named_children<'cursor>(
        &self,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor;
    fn children_by_field_id<'cursor>(
        &self,
        field: FieldId,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor;
    fn children_by_field_name<'cursor>(
        &self,
        name: &str,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor;
    fn field_name_for_child(&self, index: ChildIx) -> Option<&'tree str>;
    fn field_name_for_named_child(&self, index: NamedChildIx) -> Option<&'tree str>;
    fn has_children(self) -> bool;
    /// May scan unnamed children; stops at the first named child.
    fn has_named_children(self) -> bool;
    /// Count visible children; this can scan children in packed trees.
    fn child_count(&self) -> ChildIx;
    /// Count named children; this can scan children in packed trees.
    fn named_child_count(&self) -> NamedChildIx;
    /// Count visible descendants including this node; this can scan packed groups.
    fn descendant_count(&self) -> usize;
    fn walk(&self) -> Self::Cursor;
    /// Can scan packed nodes; a cursor retains ancestry during traversal.
    fn parent(&self) -> Option<Self>;
    /// Can scan preceding children. Prefer iteration when visiting all children.
    fn child(&self, index: ChildIx) -> Option<Self>;
    fn named_child(&self, index: NamedChildIx) -> Option<Self>;
    fn next_sibling(&self) -> Option<Self>;
    fn prev_sibling(&self) -> Option<Self>;
    fn next_named_sibling(&self) -> Option<Self>;
    fn prev_named_sibling(&self) -> Option<Self>;
    fn child_by_field_id(&self, field: FieldId) -> Option<Self>;
    fn child_by_field_name(&self, name: impl AsRef<[u8]>) -> Option<Self>;
    fn descendant_for_byte_range(&self, start: usize, end: usize) -> Option<Self>;
    fn descendant_for_point_range(&self, start: Point, end: Point) -> Option<Self>;
}

pub trait CursorLike<'tree>: Clone {
    type Node: NodeLike<'tree>;
    fn node(&self) -> Self::Node;
    /// Current node constant-time attributes.
    fn attributes(&mut self) -> Attributes<'tree> {
        self.node().attributes()
    }
    /// Change the traversal root, retaining allocated cursor storage.
    fn reset(&mut self, node: Self::Node);
    /// Can scan siblings. Failure leaves the cursor unchanged.
    fn goto_previous_sibling(&mut self) -> bool;
    /// Move to the first child ending after the byte and return its index. Can scan children.
    /// Failure leaves the cursor unchanged.
    fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<ChildIx>;
    /// Point counterpart of goto_first_child_for_byte, with the same failure behavior.
    fn goto_first_child_for_point(&mut self, point: Point) -> Option<ChildIx>;
    fn field_id(&self) -> Option<FieldId>;
    fn field_name(&self) -> Option<&'tree str>;
    fn reset_to(&mut self, cursor: &Self);
    fn depth(&self) -> u32;
    fn goto_first_child(&mut self) -> bool;
    fn goto_last_child(&mut self) -> bool;
    fn goto_next_sibling(&mut self) -> bool;
    fn goto_parent(&mut self) -> bool;
}

impl TreeLike for Tree {
    type Node<'tree> = Node<'tree>;
    fn root_node(&self) -> Self::Node<'_> {
        self.root_node()
    }
}
impl TreeLike for tree_sitter::Tree {
    type Node<'tree> = tree_sitter::Node<'tree>;
    fn root_node(&self) -> Self::Node<'_> {
        self.root_node()
    }
}

// Both node APIs intentionally share names and signatures. Keep the forwarding
// list in one place so extending the comparison contract extends both backends.
macro_rules! node_navigation {
    ($node:ty) => {
        fn parent(&self) -> Option<Self> {
            <$node>::parent(self)
        }

        fn next_sibling(&self) -> Option<Self> {
            <$node>::next_sibling(self)
        }
        fn prev_sibling(&self) -> Option<Self> {
            <$node>::prev_sibling(self)
        }
        fn next_named_sibling(&self) -> Option<Self> {
            <$node>::next_named_sibling(self)
        }
        fn prev_named_sibling(&self) -> Option<Self> {
            <$node>::prev_named_sibling(self)
        }
        fn child_by_field_name(&self, name: impl AsRef<[u8]>) -> Option<Self> {
            <$node>::child_by_field_name(self, name)
        }
        fn child_by_field_id(&self, field: FieldId) -> Option<Self> {
            <$node>::child_by_field_id(self, field.into())
        }
        fn descendant_for_byte_range(&self, start: usize, end: usize) -> Option<Self> {
            <$node>::descendant_for_byte_range(self, start, end)
        }
        fn descendant_for_point_range(&self, start: Point, end: Point) -> Option<Self> {
            <$node>::descendant_for_point_range(self, start, end)
        }
    };
}
macro_rules! node_attributes {
    ($node:ty) => {
        fn kind_id(&self) -> KindId {
            <$node>::kind_id(self).into()
        }
        fn grammar_id(&self) -> GrammarKindId {
            <$node>::grammar_id(self).into()
        }
        fn kind(&self) -> &'tree str {
            <$node>::kind(self)
        }
        fn grammar_name(&self) -> &'tree str {
            <$node>::grammar_name(self)
        }
        fn byte_range(&self) -> Range<usize> {
            <$node>::byte_range(self)
        }
        fn start_byte(&self) -> usize {
            <$node>::start_byte(self)
        }
        fn end_byte(&self) -> usize {
            <$node>::end_byte(self)
        }
        fn start_position(&self) -> Point {
            <$node>::start_position(self)
        }
        fn end_position(&self) -> Point {
            <$node>::end_position(self)
        }
        fn is_named(&self) -> bool {
            <$node>::is_named(self)
        }
        fn is_extra(&self) -> bool {
            <$node>::is_extra(self)
        }
        fn is_missing(&self) -> bool {
            <$node>::is_missing(self)
        }
        fn is_error(&self) -> bool {
            <$node>::is_error(self)
        }
        fn has_error(&self) -> bool {
            <$node>::has_error(self)
        }
    };
}
macro_rules! attributes {
    ($node:expr) => {
        Attributes {
            kind: $node.kind(),
            grammar_name: $node.grammar_name(),
            kind_id: $node.kind_id().into(),
            grammar_id: $node.grammar_id().into(),
            start_byte: $node.start_byte(),
            end_byte: $node.end_byte(),
            start_position: $node.start_position(),
            end_position: $node.end_position(),
            has_points: $node.has_points(),
            is_named: $node.is_named(),
            is_extra: $node.is_extra(),
            is_missing: $node.is_missing(),
            is_error: $node.is_error(),
            has_error: $node.has_error(),
        }
    };
}
impl<'tree> NodeLike<'tree> for tree_sitter::Node<'tree> {
    type Cursor = tree_sitter::TreeCursor<'tree>;
    node_attributes!(tree_sitter::Node<'tree>);
    fn has_points(self) -> bool {
        true
    }
    fn preorder(self) -> impl Iterator<Item = Self> {
        NativePreorder::new(self)
    }
    fn descendants_matching_kinds<K: IdSelection>(self, kinds: K) -> impl Iterator<Item = Self> {
        let empty = kinds.is_empty();
        NativePreorder::new(self)
            .take_while(move |_| !empty)
            .filter(move |node| kinds.contains_id(node.kind_id().into()))
    }
    fn children<'cursor>(
        &self,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor,
    {
        let mut children = tree_sitter::Node::children(self, cursor);
        std::iter::from_fn(move || children.next())
    }
    fn named_children<'cursor>(
        &self,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor,
    {
        let mut children = tree_sitter::Node::named_children(self, cursor);
        std::iter::from_fn(move || children.next())
    }
    fn children_by_field_id<'cursor>(
        &self,
        field: FieldId,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor,
    {
        let mut children = tree_sitter::Node::children_by_field_id(
            self,
            std::num::NonZeroU16::new(field.get()).unwrap(),
            cursor,
        );
        std::iter::from_fn(move || children.next())
    }
    fn children_by_field_name<'cursor>(
        &self,
        name: &str,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor,
    {
        let mut children = tree_sitter::Node::children_by_field_name(self, name, cursor);
        std::iter::from_fn(move || children.next())
    }
    fn field_name_for_child(&self, index: ChildIx) -> Option<&'tree str> {
        tree_sitter::Node::field_name_for_child(self, index.get())
    }
    fn field_name_for_named_child(&self, index: NamedChildIx) -> Option<&'tree str> {
        tree_sitter::Node::field_name_for_named_child(self, index.get())
    }
    fn has_children(self) -> bool {
        tree_sitter::Node::child_count(&self) != 0
    }
    fn has_named_children(self) -> bool {
        tree_sitter::Node::named_child_count(&self) != 0
    }
    fn child_count(&self) -> ChildIx {
        ChildIx::new(tree_sitter::Node::child_count(self) as u32)
    }
    fn named_child_count(&self) -> NamedChildIx {
        NamedChildIx::new(tree_sitter::Node::named_child_count(self) as u32)
    }
    fn descendant_count(&self) -> usize {
        tree_sitter::Node::descendant_count(self)
    }
    type Id = usize;
    fn id(&self) -> Self::Id {
        tree_sitter::Node::id(self)
    }
    fn attributes(self) -> Attributes<'tree> {
        attributes!(self)
    }
    fn walk(&self) -> Self::Cursor {
        self.walk()
    }
    fn child(&self, index: ChildIx) -> Option<Self> {
        tree_sitter::Node::child(self, index.get())
    }
    fn named_child(&self, index: NamedChildIx) -> Option<Self> {
        tree_sitter::Node::named_child(self, index.get())
    }
    node_navigation!(tree_sitter::Node<'tree>);
}
impl<'tree> NodeLike<'tree> for Node<'tree> {
    type Cursor = TreeCursor<'tree>;
    node_attributes!(Node<'tree>);
    fn has_points(self) -> bool {
        Node::has_points(self)
    }
    fn preorder(self) -> impl Iterator<Item = Self> {
        Node::preorder(self).nodes()
    }
    fn descendants_matching_kinds<K: IdSelection>(self, kinds: K) -> impl Iterator<Item = Self> {
        Node::descendants_matching_kinds(self, kinds)
    }
    fn children<'cursor>(
        &self,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor,
    {
        Node::children(self, cursor)
    }
    fn named_children<'cursor>(
        &self,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor,
    {
        Node::named_children(self, cursor)
    }
    fn children_by_field_id<'cursor>(
        &self,
        field: FieldId,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor,
    {
        Node::children_by_field_id(self, field, cursor)
    }
    fn children_by_field_name<'cursor>(
        &self,
        name: &str,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor,
    {
        Node::children_by_field_name(self, name, cursor)
    }
    fn field_name_for_child(&self, index: ChildIx) -> Option<&'tree str> {
        self.child(index)?.field_name()
    }
    fn field_name_for_named_child(&self, index: NamedChildIx) -> Option<&'tree str> {
        self.named_child(index)?.field_name()
    }
    fn has_children(self) -> bool {
        Node::has_children(self)
    }
    fn has_named_children(self) -> bool {
        Node::has_named_children(self)
    }
    fn child_count(&self) -> ChildIx {
        Node::child_count(self)
    }
    fn named_child_count(&self) -> NamedChildIx {
        Node::named_child_count(self)
    }
    fn descendant_count(&self) -> usize {
        Node::descendant_count(self)
    }
    type Id = SlotIx;
    fn id(&self) -> Self::Id {
        self.slot()
    }
    fn attributes(self) -> Attributes<'tree> {
        Node::attributes(self)
    }
    fn walk(&self) -> Self::Cursor {
        self.walk()
    }
    fn child(&self, index: ChildIx) -> Option<Self> {
        Node::child(self, index)
    }
    fn named_child(&self, index: NamedChildIx) -> Option<Self> {
        Node::named_child(self, index)
    }
    node_navigation!(Node<'tree>);
}
macro_rules! cursor_navigation {
    ($cursor:ty) => {
        fn field_name(&self) -> Option<&'tree str> {
            <$cursor>::field_name(self)
        }
        fn reset_to(&mut self, cursor: &Self) {
            <$cursor>::reset_to(self, cursor)
        }
        fn node(&self) -> Self::Node {
            <$cursor>::node(self)
        }
        fn reset(&mut self, node: Self::Node) {
            <$cursor>::reset(self, node)
        }
        fn goto_previous_sibling(&mut self) -> bool {
            <$cursor>::goto_previous_sibling(self)
        }

        fn depth(&self) -> u32 {
            <$cursor>::depth(self)
        }
        fn goto_first_child(&mut self) -> bool {
            <$cursor>::goto_first_child(self)
        }
        fn goto_last_child(&mut self) -> bool {
            <$cursor>::goto_last_child(self)
        }
        fn goto_next_sibling(&mut self) -> bool {
            <$cursor>::goto_next_sibling(self)
        }
        fn goto_parent(&mut self) -> bool {
            <$cursor>::goto_parent(self)
        }
    };
}
impl<'tree> CursorLike<'tree> for tree_sitter::TreeCursor<'tree> {
    type Node = tree_sitter::Node<'tree>;
    fn field_id(&self) -> Option<FieldId> {
        self.field_id().map(Into::into)
    }
    fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<ChildIx> {
        tree_sitter::TreeCursor::goto_first_child_for_byte(self, byte)
            .map(|index| ChildIx::new(index as u32))
    }
    fn goto_first_child_for_point(&mut self, point: Point) -> Option<ChildIx> {
        tree_sitter::TreeCursor::goto_first_child_for_point(self, point)
            .map(|index| ChildIx::new(index as u32))
    }
    cursor_navigation!(tree_sitter::TreeCursor<'tree>);
}
impl<'tree> CursorLike<'tree> for TreeCursor<'tree> {
    type Node = Node<'tree>;
    fn attributes(&mut self) -> Attributes<'tree> {
        TreeCursor::attributes(self)
    }
    fn field_id(&self) -> Option<FieldId> {
        TreeCursor::field_id(self)
    }
    fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<ChildIx> {
        TreeCursor::goto_first_child_for_byte(self, byte)
    }
    fn goto_first_child_for_point(&mut self, point: Point) -> Option<ChildIx> {
        TreeCursor::goto_first_child_for_point(self, point)
    }
    cursor_navigation!(TreeCursor<'tree>);
}

struct NativePreorder<'tree> {
    cursor: tree_sitter::TreeCursor<'tree>,
    started: bool,
    finished: bool,
}
impl<'tree> NativePreorder<'tree> {
    fn new(node: tree_sitter::Node<'tree>) -> Self {
        Self {
            cursor: node.walk(),
            started: false,
            finished: false,
        }
    }
}
impl<'tree> Iterator for NativePreorder<'tree> {
    type Item = tree_sitter::Node<'tree>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        if self.started && !self.cursor.goto_first_child() {
            while !self.cursor.goto_next_sibling() {
                if !self.cursor.goto_parent() {
                    self.finished = true;
                    return None;
                }
            }
        }
        self.started = true;
        Some(self.cursor.node())
    }
}
impl std::iter::FusedIterator for NativePreorder<'_> {}
