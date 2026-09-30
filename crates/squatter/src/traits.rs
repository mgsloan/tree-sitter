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
    ChildIx, FieldId, Forest, GrammarId, KindId, NamedChildIx, Node, NodeId, Tree, TreeCursor,
    scan::IdSelection,
};
use std::ops::Range;
use tree_sitter::Point;

/// Parses a fresh UTF-8 document without incremental reuse.
/// Tree-sitter's inherent methods shadow these methods; use qualified calls there.
pub trait Parse {
    type Tree: TreeLike;
    type Error;
    type Options<'a>: Default + From<crate::ParseOptions<'a>>;

    /// Parses chunks starting at the requested byte offset and point.
    /// An empty chunk ends input; reads may seek backward.
    fn parse_with_options<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self,
        callback: &mut F,
        options: Self::Options<'_>,
    ) -> Result<Self::Tree, Self::Error>;

    fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Self::Tree, Self::Error> {
        let source = source.as_ref();
        self.parse_with_options(
            &mut |byte, _| source.get(byte..).unwrap_or_default(),
            Default::default(),
        )
    }
}

/// Parsing progress, valid only during the callback.
pub trait ParseStateLike {
    fn current_byte_offset(&self) -> usize;
    fn has_error(&self) -> bool;
    /// Always false; packing does not report progress.
    fn is_converting(&self) -> bool;
    /// Always false; parsing traverses input forward.
    fn current_byte_offset_descends(&self) -> bool;
}

/// Constant-time attributes supported by both representations on freshly parsed trees.
/// Child and descendant counts are separate node operations.
///
/// **Not in Tree-sitter**. Bundled inspection of a fresh snapshot. Edit registration and
/// change tracking are excluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attributes<'tree> {
    pub kind: &'tree str,
    pub grammar_name: &'tree str,
    pub kind_id: KindId,
    pub grammar_id: GrammarId,
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

/// Shared navigation over packed and tree-sitter trees.
///
/// **Not in Tree-sitter**
pub trait TreeLike {
    type Node<'tree>: NodeLike<'tree>
    where
        Self: 'tree;
    /// Get the root node of the syntax tree.
    fn root_node(&self) -> Self::Node<'_>;
    /// Create a new [`TreeCursor`] starting from the root of the tree.
    fn walk(&self) -> <Self::Node<'_> as NodeLike<'_>>::Cursor {
        self.root_node().walk()
    }
}

/// Statically dispatched navigation over both representations.
/// Typed indices distinguish all-child and named-child domains.
///
/// **Not in Tree-sitter**
pub trait NodeLike<'tree>: Copy + Eq {
    type Cursor: CursorLike<'tree, Node = Self>;
    /// Stable within this tree; not comparable across representations.
    type Id: Copy + Eq + std::hash::Hash;
    /// Get a numeric id for this node that is unique.
    ///
    /// Within a given syntax tree, no two nodes have the same id.
    ///
    /// Identity is scoped to one immutable snapshot; it is not guaranteed across packing,
    /// reloads, or edits. An ID does not keep its tree alive. Packed nodes also expose
    /// inherent `slot()`; `id()` requires this trait in scope.
    fn id(&self) -> Self::Id;
    /// Read constant-time attributes; counts are separate operations below.
    fn attributes(self) -> Attributes<'tree>;
    /// This node's displayed kind ID, including aliases, comparable across
    /// Squatter and Tree-sitter nodes using the same language version.
    fn kind_id(&self) -> KindId;
    /// This node's original grammar ID, ignoring aliases, comparable across
    /// Squatter and Tree-sitter nodes using the same language version.
    fn grammar_id(&self) -> GrammarId;
    /// Get this node's type as a string.
    fn kind(&self) -> &'tree str;
    /// Get this node's symbol name as it appears in the grammar ignoring
    /// aliases as a string.
    fn grammar_name(&self) -> &'tree str;
    /// Get the range of source code that this node represents, both in terms of
    /// raw bytes and of row/column coordinates.
    ///
    /// **Different behavior than Tree-sitter:** For packed trees without point data, positions
    /// use row zero and the byte offset as column. Check `has_points()` before relying on
    /// line/column coordinates.
    fn range(&self) -> tree_sitter::Range;
    /// Returns the UTF-8 source slice for this node. Invalid UTF-8 returns an error;
    /// out-of-bounds byte offsets panic.
    fn utf8_text<'source>(
        &self,
        source: &'source [u8],
    ) -> Result<&'source str, std::str::Utf8Error>;
    /// Returns the source slice indexed by this node’s byte offsets divided by two. Supply
    /// the UTF-16 input used to parse the tree; out-of-bounds offsets panic.
    fn utf16_text<'source>(&self, source: &'source [u16]) -> &'source [u16];
    /// Get the byte range of source code that this node represents.
    fn byte_range(&self) -> Range<usize>;
    /// Get the byte offset where this node starts.
    fn start_byte(&self) -> usize;
    /// Get the byte offset where this node ends.
    fn end_byte(&self) -> usize;
    /// Get this node's start position in terms of rows and columns.
    ///
    /// **Different behavior than Tree-sitter:** For packed trees without point data, positions
    /// use row zero and the byte offset as column. Check `has_points()` before relying on
    /// line/column coordinates.
    fn start_position(&self) -> Point;
    /// Get this node's end position in terms of rows and columns.
    ///
    /// **Different behavior than Tree-sitter:** For packed trees without point data, positions
    /// use row zero and the byte offset as column. Check `has_points()` before relying on
    /// line/column coordinates.
    fn end_position(&self) -> Point;
    /// Reports whether point data is attached. Without it, point
    /// accessors, ranges, point lookups, and point scans use `(0, byte_offset)`.
    fn has_points(self) -> bool;
    /// Check if this node is *named*.
    ///
    /// Named nodes correspond to named rules in the grammar, whereas
    /// *anonymous* nodes correspond to string literals in the grammar.
    fn is_named(&self) -> bool;
    /// Check if this node is *extra*.
    ///
    /// Extra nodes represent things like comments, which are not required by the
    /// grammar, but can appear anywhere.
    fn is_extra(&self) -> bool;
    /// Check if this node is *missing*.
    ///
    /// Missing nodes are inserted by the parser in order to recover from
    /// certain kinds of syntax errors.
    fn is_missing(&self) -> bool;
    /// Check if this node represents a syntax error.
    ///
    /// Syntax errors represent parts of the code that could not be incorporated
    /// into a valid syntax tree.
    fn is_error(&self) -> bool;
    /// Check if this node represents a syntax error or contains any syntax
    /// errors anywhere within it.
    fn has_error(&self) -> bool;
    /// Iterates over this node and its descendants in preorder, using the backend's
    /// native traversal.
    ///
    /// Returns a plain iterator. Packed scan filters and group traversal are available
    /// through the inherent [`Node::preorder`] method.
    fn preorder(self) -> impl Iterator<Item = Self>;
    /// Public kind IDs, in preorder including this node. Never leaves its subtree.
    ///
    /// Missing presence caches affect cost, not results.
    fn descendants_matching_kinds<K: IdSelection>(self, kinds: K) -> impl Iterator<Item = Self>;
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
    /// None)`. Packed iteration needs no initial counting pass. Iteration resets and moves
    /// the supplied cursor; dropping the iterator leaves the cursor at its current
    /// position.
    fn children<'cursor>(
        &self,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor;
    /// Iterate over this node's named children.
    ///
    /// See also [`Node::children`].
    ///
    /// **Different than Tree-sitter:** Returns a plain iterator with `size_hint() == (0,
    /// None)`. Packed iteration needs no initial counting pass. Iteration resets and moves
    /// the supplied cursor; dropping the iterator leaves the cursor at its current
    /// position.
    fn named_children<'cursor>(
        &self,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor;
    /// Iterate over this node's children with a given field id.
    ///
    /// See also [`Node::children_by_field_name`].
    ///
    /// **Different than Tree-sitter:** Returns a plain iterator with `size_hint() == (0,
    /// None)`. Packed iteration needs no initial counting pass. Iteration resets and moves
    /// the supplied cursor; dropping the iterator leaves the cursor at its current
    /// position.
    fn children_by_field_id<'cursor>(
        &self,
        field: FieldId,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor;
    /// Iterate over this node's children with a given field name.
    ///
    /// See also [`Node::children`].
    ///
    /// **Different than Tree-sitter:** Returns a plain iterator with `size_hint() == (0,
    /// None)`. Packed iteration needs no initial counting pass. Iteration resets and moves
    /// the supplied cursor; dropping the iterator leaves the cursor at its current
    /// position.
    /// An unknown field name yields no children and leaves the cursor unchanged.
    fn children_by_field_name<'cursor>(
        &self,
        name: &str,
        cursor: &'cursor mut Self::Cursor,
    ) -> impl Iterator<Item = Self> + 'cursor
    where
        Self: 'cursor;
    /// Get the field name of this node's child at the given index.
    ///
    /// **Different performance than Tree-sitter:** Packed nodes scan preceding children, then
    /// resolve the stored field. Prefer a single child traversal and direct field access.
    fn field_name_for_child(&self, index: ChildIx) -> Option<&'tree str>;
    /// Get the field name of this node's named child at the given index.
    ///
    /// **Different performance than Tree-sitter:** Packed nodes scan children, including
    /// intervening unnamed children, then resolve the stored field.
    fn field_name_for_named_child(&self, index: NamedChildIx) -> Option<&'tree str>;
    /// Tests for a structural child without counting children.
    fn has_children(self) -> bool;
    /// May scan unnamed children; stops at the first named child.
    fn has_named_children(self) -> bool;
    /// Get this node's number of children.
    ///
    /// **Different performance than Tree-sitter:** For packed nodes, scans children.
    fn child_count(&self) -> ChildIx;
    /// Get this node's number of *named* children.
    ///
    /// See also [`Node::is_named`].
    ///
    /// **Different performance than Tree-sitter:** For packed nodes, scans all children.
    fn named_child_count(&self) -> NamedChildIx;
    /// Get the node's number of descendants, including one for the node itself.
    ///
    /// **Different performance than Tree-sitter:** For packed nodes, scans packed groups in
    /// this subtree.
    fn descendant_count(&self) -> usize;
    /// Create a new [`TreeCursor`] starting from this node.
    ///
    /// Note that the given node is considered the root of the cursor,
    /// and the cursor cannot walk outside this node.
    ///
    /// **Different performance than Tree-sitter:** For packed nodes, creates an empty
    /// ancestor stack. Reuse the cursor to retain its allocation.
    fn walk(&self) -> Self::Cursor;
    /// Get this node's immediate parent.
    /// Prefer [`child_with_descendant`](Node::child_with_descendant)
    /// for iterating over this node's ancestors.
    ///
    /// **Different performance than Tree-sitter:** For packed nodes, can scan subsequent
    /// packed groups. A cursor retains ancestry for repeated navigation.
    fn parent(&self) -> Option<Self>;
    /// Get the node's child at the given index, where zero represents the first
    /// child.
    ///
    /// This method scans preceding children, so if
    /// you might be iterating over a long list of children, you should use
    /// [`Node::children`] instead.
    ///
    /// **Different performance than Tree-sitter:** For packed nodes, visits up to index + 1
    /// children. Repeated indexed lookup across a wide node can be quadratic; prefer one
    /// traversal.
    fn child(&self, index: ChildIx) -> Option<Self>;
    /// Get this node's *named* child at the given index.
    ///
    /// See also [`Node::is_named`].
    /// This method scans preceding children, so if
    /// you might be iterating over a long list of children, you should use
    /// [`Node::named_children`] instead.
    ///
    /// **Different performance than Tree-sitter:** For packed nodes, scans preceding
    /// children, including unnamed children. Prefer one traversal for several children.
    fn named_child(&self, index: NamedChildIx) -> Option<Self>;
    /// Get this node's next sibling.
    fn next_sibling(&self) -> Option<Self>;
    /// Get this node's previous sibling.
    fn prev_sibling(&self) -> Option<Self>;
    /// Get this node's next named sibling.
    fn next_named_sibling(&self) -> Option<Self>;
    /// Get this node's previous named sibling.
    fn prev_named_sibling(&self) -> Option<Self>;
    /// Get this node's child with the given numerical field id.
    ///
    /// See also [`child_by_field_name`](Node::child_by_field_name). You can
    /// convert a field name to an id using [`crate::Language::field_id_for_name`].
    fn child_by_field_id(&self, field: FieldId) -> Option<Self>;
    /// Get the first child with the given field name.
    ///
    /// If multiple children may have the same field name, access them using
    /// [`children_by_field_name`](Node::children_by_field_name)
    fn child_by_field_name(&self, name: impl AsRef<[u8]>) -> Option<Self>;
    /// Get the smallest node within this node that spans the given byte range.
    fn descendant_for_byte_range(&self, start: usize, end: usize) -> Option<Self>;
    /// Get the smallest node within this node that spans the given point range.
    ///
    /// **Different behavior than Tree-sitter:** For packed trees without point data, positions
    /// use row zero and the byte offset as column. Check `has_points()` before relying on
    /// line/column coordinates.
    fn descendant_for_point_range(&self, start: Point, end: Point) -> Option<Self>;
}

/// Shared cursor operations. Clones and `reset_to` preserve
/// independent traversal state.
///
/// **Not in Tree-sitter**
pub trait CursorLike<'tree>: Clone {
    type Node: NodeLike<'tree>;
    /// Get the tree cursor's current [`Node`].
    fn node(&self) -> Self::Node;
    /// Current node constant-time attributes.
    fn attributes(&mut self) -> Attributes<'tree> {
        self.node().attributes()
    }
    /// Re-initialize this tree cursor to start at the given node.
    fn reset(&mut self, node: Self::Node);
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
    fn goto_previous_sibling(&mut self) -> bool;
    /// Move this cursor to the first child of its current node that contains or
    /// starts after the given byte offset.
    ///
    /// This returns the index of the child node if one was found, and returns
    /// `None` if no such child was found.
    ///
    /// Coordinates narrow with `as u32`.
    fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<ChildIx>;
    /// Move this cursor to the first child of its current node that contains or
    /// starts after the given point.
    ///
    /// This returns the index of the child node if one was found, and returns
    /// `None` if no such child was found.
    ///
    /// Point components narrow with `as u32`.
    ///
    /// **Different behavior than Tree-sitter:** For packed trees without point data, positions
    /// use row zero and the byte offset as column. Check `has_points()` before relying on
    /// line/column coordinates.
    fn goto_first_child_for_point(&mut self, point: Point) -> Option<ChildIx>;
    /// Get the numerical field id of this tree cursor's current node.
    ///
    /// See also [`field_name`](TreeCursor::field_name).
    fn field_id(&self) -> Option<FieldId>;
    /// Get the field name of this tree cursor's current node.
    fn field_name(&self) -> Option<&'tree str>;
    /// Re-initialize a tree cursor to the same position as another cursor.
    ///
    /// Unlike [`reset`](TreeCursor::reset), this will not lose parent
    /// information and allows reusing already created cursors.
    fn reset_to(&mut self, cursor: &Self);
    /// Get the depth of the cursor's current node relative to the original
    /// node that the cursor was constructed with.
    fn depth(&self) -> u32;
    /// Move this cursor to the first child of its current node.
    ///
    /// This returns `true` if the cursor successfully moved, and returns
    /// `false` if there were no children.
    fn goto_first_child(&mut self) -> bool;
    /// Move this cursor to the last child of its current node.
    ///
    /// This returns `true` if the cursor successfully moved, and returns
    /// `false` if there were no children.
    ///
    /// Note that this function may be slower than
    /// [`goto_first_child`](TreeCursor::goto_first_child) because it needs to
    /// iterate through all the children to compute the child's position.
    fn goto_last_child(&mut self) -> bool;
    /// Move this cursor to the next sibling of its current node.
    ///
    /// This returns `true` if the cursor successfully moved, and returns
    /// `false` if there was no next sibling node.
    ///
    /// Note that the node the cursor was constructed with is considered the root
    /// of the cursor, and the cursor cannot walk outside this node.
    fn goto_next_sibling(&mut self) -> bool;
    /// Move this cursor to the parent of its current node.
    ///
    /// This returns `true` if the cursor successfully moved, and returns
    /// `false` if there was no parent node (the cursor was already on the
    /// root node).
    ///
    /// Note that the node the cursor was constructed with is considered the root
    /// of the cursor, and the cursor cannot walk outside this node.
    fn goto_parent(&mut self) -> bool;
}

impl TreeLike for Tree<'_> {
    type Node<'tree>
        = Node<'tree>
    where
        Self: 'tree;
    fn root_node(&self) -> Self::Node<'_> {
        self.root_node()
    }
}
impl TreeLike for Forest {
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
            KindId::from_raw(<$node>::kind_id(self).into())
        }
        fn grammar_id(&self) -> GrammarId {
            GrammarId::from_raw(<$node>::grammar_id(self).into())
        }
        fn kind(&self) -> &'tree str {
            <$node>::kind(self)
        }
        fn grammar_name(&self) -> &'tree str {
            <$node>::grammar_name(self)
        }
        fn range(&self) -> tree_sitter::Range {
            <$node>::range(self)
        }
        fn utf8_text<'source>(
            &self,
            source: &'source [u8],
        ) -> Result<&'source str, std::str::Utf8Error> {
            <$node>::utf8_text(self, source)
        }
        fn utf16_text<'source>(&self, source: &'source [u16]) -> &'source [u16] {
            <$node>::utf16_text(self, source)
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
            kind_id: KindId::from_raw($node.kind_id().into()),
            grammar_id: GrammarId::from_raw($node.grammar_id().into()),
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
            .filter(move |node| kinds.contains_id(KindId::from_raw(node.kind_id())))
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
            std::num::NonZeroU16::new(field.raw()).unwrap(),
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
        tree_sitter::Node::field_name_for_child(self, index.raw())
    }
    fn field_name_for_named_child(&self, index: NamedChildIx) -> Option<&'tree str> {
        tree_sitter::Node::field_name_for_named_child(self, index.raw())
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
        tree_sitter::Node::child(self, index.raw())
    }
    fn named_child(&self, index: NamedChildIx) -> Option<Self> {
        tree_sitter::Node::named_child(self, index.raw())
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
    type Id = NodeId;
    fn id(&self) -> Self::Id {
        self.id()
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
