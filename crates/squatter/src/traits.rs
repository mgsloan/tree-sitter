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
//! For stateful attribute reads, use `node_iterator` and `NodeIteratorLike`.
use crate::{Cursor, Error, KindSet, Node, NodeIterator, Tree};
use std::ops::Range;
use tree_sitter::Point;

/// Constant-time attributes supported by both representations on freshly parsed trees.
/// Child and descendant counts are separate node operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attributes<'tree> {
    pub kind: &'tree str,
    pub grammar_name: &'tree str,
    pub kind_id: u16,
    pub grammar_id: u16,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_position: Point,
    pub end_position: Point,
    pub is_named: bool,
    pub is_extra: bool,
    pub is_missing: bool,
    pub is_error: bool,
    /// Squatter reports a conservative predicate shared by all nodes in a physical block.
    pub has_error: bool,
    pub has_changes: bool,
}

pub trait TreeLike {
    type Node<'tree>: NodeLike<'tree>
    where
        Self: 'tree;
    fn root(&self) -> Self::Node<'_>;
}

pub trait NodeLike<'tree>: Copy + Eq {
    type Cursor: CursorLike<'tree, Node = Self>;
    /// Stable within this tree; not comparable across representations.
    fn identity(self) -> usize;
    /// Read constant-time attributes; counts are separate operations below.
    fn attributes(self) -> Attributes<'tree>;
    fn kind_id(self) -> u16;
    fn grammar_id(self) -> u16;
    fn kind(self) -> &'tree str;
    fn grammar_name(self) -> &'tree str;
    fn byte_range(self) -> Range<usize>;
    fn start_byte(self) -> usize;
    fn end_byte(self) -> usize;
    fn start_position(self) -> Point;
    fn end_position(self) -> Point;
    fn is_named(self) -> bool;
    fn is_extra(self) -> bool;
    fn is_missing(self) -> bool;
    fn is_error(self) -> bool;
    fn has_error(self) -> bool;
    fn has_changes(self) -> bool;
    /// Preorder including this node, using the backend's native traversal.
    fn preorder(self) -> impl Iterator<Item = Self>;
    /// Stateful preorder reads.
    fn node_iterator(self) -> Result<impl NodeIteratorLike<'tree, Node = Self>, Error>;
    /// Public kind IDs, in preorder including this node. Never leaves its subtree.
    fn descendants_matching_kinds(self, kinds: &KindSet) -> impl Iterator<Item = Self>;
    /// Structural children, including empty nodes, without requiring a count.
    fn children(self) -> impl Iterator<Item = Self>;
    fn named_children(self) -> impl Iterator<Item = Self> {
        self.children().filter(|node| node.is_named())
    }
    /// All children with this field, including inherited fields. Zero yields none.
    fn children_by_field_id(self, field: u16) -> impl Iterator<Item = Self>;
    fn has_children(self) -> bool;
    /// May scan unnamed children; stops at the first named child.
    fn has_named_children(self) -> bool;
    /// Count visible children; this can scan children in packed trees.
    fn child_count(self) -> usize;
    /// Count named children; this can scan children in packed trees.
    fn named_child_count(self) -> usize;
    /// Count visible descendants including this node; this can scan packed groups.
    fn descendant_count(self) -> usize;
    fn cursor(self) -> Result<Self::Cursor, Error>;
    /// Can scan packed nodes; a cursor retains ancestry during traversal.
    fn parent(self) -> Option<Self>;
    /// Can scan preceding children. Prefer iteration when visiting all children.
    fn child(self, index: usize) -> Option<Self>;
    fn named_child(self, index: usize) -> Option<Self>;
    fn next_sibling(self) -> Option<Self>;
    fn prev_sibling(self) -> Option<Self>;
    fn next_named_sibling(self) -> Option<Self>;
    fn prev_named_sibling(self) -> Option<Self>;
    fn child_by_field_id(self, field: u16) -> Option<Self>;
    fn descendant_for_byte_range(self, start: usize, end: usize) -> Option<Self>;
    fn descendant_for_point_range(self, start: Point, end: Point) -> Option<Self>;
}

pub trait CursorLike<'tree> {
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
    /// Failure (including a coordinate exceeding u32) leaves the cursor unchanged.
    fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<usize>;
    /// Point counterpart of goto_first_child_for_byte, with the same failure behavior.
    fn goto_first_child_for_point(&mut self, point: Point) -> Option<usize>;
    fn field_id(&self) -> Option<u16>;
    fn depth(&self) -> u32;
    fn goto_first_child(&mut self) -> bool;
    fn goto_last_child(&mut self) -> bool;
    fn goto_next_sibling(&mut self) -> bool;
    fn goto_parent(&mut self) -> bool;
}

/// Reads refer to the last yielded node, and return None before iteration and
/// after exhaustion.
pub trait NodeIteratorLike<'tree>: Iterator<Item = Self::Node> {
    type Node: NodeLike<'tree>;
    fn node(&self) -> Option<Self::Node>;
    fn attributes(&mut self) -> Option<Attributes<'tree>>;
    fn kind_id(&mut self) -> Option<u16>;
    fn byte_range(&mut self) -> Option<Range<usize>>;
}

impl TreeLike for Tree {
    type Node<'tree> = Node<'tree>;
    fn root(&self) -> Self::Node<'_> {
        self.root_node()
    }
}
impl TreeLike for tree_sitter::Tree {
    type Node<'tree> = tree_sitter::Node<'tree>;
    fn root(&self) -> Self::Node<'_> {
        self.root_node()
    }
}

// Both node APIs intentionally share names and signatures. Keep the forwarding
// list in one place so extending the comparison contract extends both backends.
macro_rules! node_navigation {
    ($node:ty $(, $borrow:tt)?) => {
        fn parent(self) -> Option<Self> {
            <$node>::parent($($borrow)? self)
        }
        fn child(self, index: usize) -> Option<Self> {
            <$node>::child($($borrow)? self, index.try_into().ok()?)
        }
        fn named_child(self, index: usize) -> Option<Self> {
            <$node>::named_child($($borrow)? self, index.try_into().ok()?)
        }
        fn next_sibling(self) -> Option<Self> {
            <$node>::next_sibling($($borrow)? self)
        }
        fn prev_sibling(self) -> Option<Self> {
            <$node>::prev_sibling($($borrow)? self)
        }
        fn next_named_sibling(self) -> Option<Self> {
            <$node>::next_named_sibling($($borrow)? self)
        }
        fn prev_named_sibling(self) -> Option<Self> {
            <$node>::prev_named_sibling($($borrow)? self)
        }
        fn child_by_field_id(self, field: u16) -> Option<Self> {
            <$node>::child_by_field_id($($borrow)? self, field)
        }
        fn descendant_for_byte_range(self, start: usize, end: usize) -> Option<Self> {
            <$node>::descendant_for_byte_range($($borrow)? self, start, end)
        }
        fn descendant_for_point_range(self, start: Point, end: Point) -> Option<Self> {
            <$node>::descendant_for_point_range($($borrow)? self, start, end)
        }
    };
}
macro_rules! node_attributes {
    ($node:ty $(, $borrow:tt)?) => {
        fn kind_id(self) -> u16 {
            <$node>::kind_id($($borrow)? self)
        }
        fn grammar_id(self) -> u16 {
            <$node>::grammar_id($($borrow)? self)
        }
        fn kind(self) -> &'tree str {
            <$node>::kind($($borrow)? self)
        }
        fn grammar_name(self) -> &'tree str {
            <$node>::grammar_name($($borrow)? self)
        }
        fn byte_range(self) -> Range<usize> {
            <$node>::byte_range($($borrow)? self)
        }
        fn start_byte(self) -> usize {
            <$node>::start_byte($($borrow)? self)
        }
        fn end_byte(self) -> usize {
            <$node>::end_byte($($borrow)? self)
        }
        fn start_position(self) -> Point {
            <$node>::start_position($($borrow)? self)
        }
        fn end_position(self) -> Point {
            <$node>::end_position($($borrow)? self)
        }
        fn is_named(self) -> bool {
            <$node>::is_named($($borrow)? self)
        }
        fn is_extra(self) -> bool {
            <$node>::is_extra($($borrow)? self)
        }
        fn is_missing(self) -> bool {
            <$node>::is_missing($($borrow)? self)
        }
        fn is_error(self) -> bool {
            <$node>::is_error($($borrow)? self)
        }
        fn has_error(self) -> bool {
            <$node>::has_error($($borrow)? self)
        }
        fn has_changes(self) -> bool {
            <$node>::has_changes($($borrow)? self)
        }
    };
}
macro_rules! attributes {
    ($node:expr) => {
        Attributes {
            kind: $node.kind(),
            grammar_name: $node.grammar_name(),
            kind_id: $node.kind_id(),
            grammar_id: $node.grammar_id(),
            start_byte: $node.start_byte(),
            end_byte: $node.end_byte(),
            start_position: $node.start_position(),
            end_position: $node.end_position(),
            is_named: $node.is_named(),
            is_extra: $node.is_extra(),
            is_missing: $node.is_missing(),
            is_error: $node.is_error(),
            has_error: $node.has_error(),
            has_changes: $node.has_changes(),
        }
    };
}
impl<'tree> NodeLike<'tree> for tree_sitter::Node<'tree> {
    type Cursor = tree_sitter::TreeCursor<'tree>;
    node_attributes!(tree_sitter::Node<'tree>, &);
    fn preorder(self) -> impl Iterator<Item = Self> {
        NativePreorder::new(self)
    }
    fn node_iterator(self) -> Result<impl NodeIteratorLike<'tree, Node = Self>, Error> {
        Ok(NativePreorder::new(self))
    }
    fn descendants_matching_kinds(self, kinds: &KindSet) -> impl Iterator<Item = Self> {
        NativePreorder::new(self)
            .take_while(move |_| !kinds.is_empty())
            .filter(move |node| kinds.contains(node.kind_id()))
    }
    fn children(self) -> impl Iterator<Item = Self> {
        NativeChildren::new(self, None)
    }
    fn children_by_field_id(self, field: u16) -> impl Iterator<Item = Self> {
        NativeChildren::new(self, Some(field))
    }
    fn has_children(self) -> bool {
        tree_sitter::Node::child_count(&self) != 0
    }
    fn has_named_children(self) -> bool {
        tree_sitter::Node::named_child_count(&self) != 0
    }
    fn child_count(self) -> usize {
        tree_sitter::Node::child_count(&self) as usize
    }
    fn named_child_count(self) -> usize {
        tree_sitter::Node::named_child_count(&self)
    }
    fn descendant_count(self) -> usize {
        tree_sitter::Node::descendant_count(&self)
    }
    fn identity(self) -> usize {
        self.id()
    }
    fn attributes(self) -> Attributes<'tree> {
        attributes!(self)
    }
    fn cursor(self) -> Result<Self::Cursor, Error> {
        Ok(self.walk())
    }
    node_navigation!(tree_sitter::Node<'tree>, &);
}
impl<'tree> NodeLike<'tree> for Node<'tree> {
    type Cursor = Cursor<'tree>;
    node_attributes!(Node<'tree>);
    fn preorder(self) -> impl Iterator<Item = Self> {
        Node::preorder(self)
    }
    fn node_iterator(self) -> Result<impl NodeIteratorLike<'tree, Node = Self>, Error> {
        Node::node_iterator(self)
    }
    fn descendants_matching_kinds(self, kinds: &KindSet) -> impl Iterator<Item = Self> {
        Node::descendants_matching_kinds(self, kinds)
    }
    fn children(self) -> impl Iterator<Item = Self> {
        Node::children(self)
    }
    fn children_by_field_id(self, field: u16) -> impl Iterator<Item = Self> {
        Node::children_by_field_id(self, field)
    }
    fn has_children(self) -> bool {
        Node::has_children(self)
    }
    fn has_named_children(self) -> bool {
        Node::has_named_children(self)
    }
    fn child_count(self) -> usize {
        Node::child_count(self)
    }
    fn named_child_count(self) -> usize {
        Node::named_child_count(self)
    }
    fn descendant_count(self) -> usize {
        Node::descendant_count(self)
    }
    fn identity(self) -> usize {
        self.slot() as usize
    }
    fn attributes(self) -> Attributes<'tree> {
        Node::attributes(self)
    }
    fn cursor(self) -> Result<Self::Cursor, Error> {
        self.walk()
    }
    node_navigation!(Node<'tree>);
}
macro_rules! cursor_navigation {
    ($cursor:ty) => {
        fn node(&self) -> Self::Node {
            <$cursor>::node(self)
        }
        fn reset(&mut self, node: Self::Node) {
            <$cursor>::reset(self, node)
        }
        fn goto_previous_sibling(&mut self) -> bool {
            <$cursor>::goto_previous_sibling(self)
        }
        fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<usize> {
            let byte = u32::try_from(byte).ok()? as usize;
            <$cursor>::goto_first_child_for_byte(self, byte).map(|index| index as usize)
        }
        fn goto_first_child_for_point(&mut self, point: Point) -> Option<usize> {
            u32::try_from(point.row).ok()?;
            u32::try_from(point.column).ok()?;
            <$cursor>::goto_first_child_for_point(self, point).map(|index| index as usize)
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
    fn field_id(&self) -> Option<u16> {
        self.field_id().map(Into::into)
    }
    cursor_navigation!(tree_sitter::TreeCursor<'tree>);
}
impl<'tree> CursorLike<'tree> for Cursor<'tree> {
    type Node = Node<'tree>;
    fn attributes(&mut self) -> Attributes<'tree> {
        Cursor::attributes(self)
    }
    fn field_id(&self) -> Option<u16> {
        let field = self.node().field_id();
        (field != 0).then_some(field)
    }
    cursor_navigation!(Cursor<'tree>);
}

struct NativePreorder<'tree> {
    cursor: tree_sitter::TreeCursor<'tree>,
    current: Option<tree_sitter::Node<'tree>>,
    finished: bool,
}
impl<'tree> NativePreorder<'tree> {
    fn new(node: tree_sitter::Node<'tree>) -> Self {
        Self {
            cursor: node.walk(),
            current: None,
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
        if self.current.is_some() && !self.cursor.goto_first_child() {
            while !self.cursor.goto_next_sibling() {
                if !self.cursor.goto_parent() {
                    self.finished = true;
                    self.current = None;
                    return None;
                }
            }
        }
        self.current = Some(self.cursor.node());
        self.current
    }
}
impl std::iter::FusedIterator for NativePreorder<'_> {}
impl<'tree> NodeIteratorLike<'tree> for NativePreorder<'tree> {
    type Node = tree_sitter::Node<'tree>;
    fn node(&self) -> Option<Self::Node> {
        self.current
    }
    fn attributes(&mut self) -> Option<Attributes<'tree>> {
        self.current.map(NodeLike::attributes)
    }
    fn kind_id(&mut self) -> Option<u16> {
        self.current.map(|node| node.kind_id())
    }
    fn byte_range(&mut self) -> Option<Range<usize>> {
        self.current.map(|node| node.byte_range())
    }
}
impl<'tree> NodeIteratorLike<'tree> for NodeIterator<'tree> {
    type Node = Node<'tree>;
    fn node(&self) -> Option<Self::Node> {
        NodeIterator::node(self)
    }
    fn attributes(&mut self) -> Option<Attributes<'tree>> {
        NodeIterator::attributes(self)
    }
    fn kind_id(&mut self) -> Option<u16> {
        NodeIterator::kind_id(self)
    }
    fn byte_range(&mut self) -> Option<Range<usize>> {
        NodeIterator::byte_range(self)
    }
}

struct NativeChildren<'tree> {
    cursor: tree_sitter::TreeCursor<'tree>,
    ready: bool,
    field: Option<u16>,
}
impl<'tree> NativeChildren<'tree> {
    fn new(node: tree_sitter::Node<'tree>, field: Option<u16>) -> Self {
        let mut cursor = node.walk();
        let ready = field != Some(0) && cursor.goto_first_child();
        Self {
            cursor,
            ready,
            field,
        }
    }
}
impl<'tree> Iterator for NativeChildren<'tree> {
    type Item = tree_sitter::Node<'tree>;
    fn next(&mut self) -> Option<Self::Item> {
        while self.ready {
            let node = self.cursor.node();
            let matches = self.field.is_none_or(|field| {
                self.cursor
                    .field_id()
                    .is_some_and(|actual| actual.get() == field)
            });
            self.ready = self.cursor.goto_next_sibling();
            if matches {
                return Some(node);
            }
        }
        None
    }
}
impl std::iter::FusedIterator for NativeChildren<'_> {}
