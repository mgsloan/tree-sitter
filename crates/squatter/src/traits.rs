//! Shared, statically dispatched navigation for mainline and packed trees.
use crate::{CachedCursor, Cursor, Error, Node, Tree};
use tree_sitter::Point;

/// Attributes supported by both representations on freshly parsed trees.
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
    pub has_error: bool,
    pub has_changes: bool,
    pub child_count: usize,
    pub named_child_count: usize,
    pub descendant_count: usize,
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
    fn attributes(self) -> Attributes<'tree>;
    fn cursor(self) -> Result<Self::Cursor, Error>;
    fn parent(self) -> Option<Self>;
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
    /// Current attributes; cached cursors override this to reuse decoded columns.
    fn attributes(&mut self) -> Attributes<'tree> {
        self.node().attributes()
    }
    fn field_id(&self) -> Option<u16>;
    fn depth(&self) -> u32;
    fn goto_first_child(&mut self) -> bool;
    fn goto_last_child(&mut self) -> bool;
    fn goto_next_sibling(&mut self) -> bool;
    fn goto_previous_sibling(&mut self) -> bool;
    fn goto_parent(&mut self) -> bool;
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
    ($node:ty) => {
        fn parent(self) -> Option<Self> {
            <$node>::parent(&self)
        }
        fn child(self, index: usize) -> Option<Self> {
            <$node>::child(&self, index.try_into().ok()?)
        }
        fn named_child(self, index: usize) -> Option<Self> {
            <$node>::named_child(&self, index.try_into().ok()?)
        }
        fn next_sibling(self) -> Option<Self> {
            <$node>::next_sibling(&self)
        }
        fn prev_sibling(self) -> Option<Self> {
            <$node>::prev_sibling(&self)
        }
        fn next_named_sibling(self) -> Option<Self> {
            <$node>::next_named_sibling(&self)
        }
        fn prev_named_sibling(self) -> Option<Self> {
            <$node>::prev_named_sibling(&self)
        }
        fn child_by_field_id(self, field: u16) -> Option<Self> {
            <$node>::child_by_field_id(&self, field)
        }
        fn descendant_for_byte_range(self, start: usize, end: usize) -> Option<Self> {
            <$node>::descendant_for_byte_range(&self, start, end)
        }
        fn descendant_for_point_range(self, start: Point, end: Point) -> Option<Self> {
            <$node>::descendant_for_point_range(&self, start, end)
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
            child_count: $node.child_count() as usize,
            named_child_count: $node.named_child_count(),
            descendant_count: $node.descendant_count(),
        }
    };
}
impl<'tree> NodeLike<'tree> for tree_sitter::Node<'tree> {
    type Cursor = tree_sitter::TreeCursor<'tree>;
    fn identity(self) -> usize {
        self.id()
    }
    fn attributes(self) -> Attributes<'tree> {
        attributes!(self)
    }
    fn cursor(self) -> Result<Self::Cursor, Error> {
        Ok(self.walk())
    }
    node_navigation!(tree_sitter::Node<'tree>);
}
impl<'tree> NodeLike<'tree> for Node<'tree> {
    type Cursor = Cursor<'tree>;
    fn identity(self) -> usize {
        self.slot() as usize
    }
    fn attributes(self) -> Attributes<'tree> {
        attributes!(self)
    }
    fn cursor(self) -> Result<Self::Cursor, Error> {
        self.walk()
    }
    fn parent(self) -> Option<Self> {
        Node::parent(self)
    }
    fn child(self, index: usize) -> Option<Self> {
        Node::child(self, index)
    }
    fn named_child(self, index: usize) -> Option<Self> {
        Node::named_child(self, index)
    }
    fn next_sibling(self) -> Option<Self> {
        Node::next_sibling(self)
    }
    fn prev_sibling(self) -> Option<Self> {
        Node::prev_sibling(self)
    }
    fn next_named_sibling(self) -> Option<Self> {
        Node::next_named_sibling(self)
    }
    fn prev_named_sibling(self) -> Option<Self> {
        Node::prev_named_sibling(self)
    }
    fn child_by_field_id(self, field: u16) -> Option<Self> {
        Node::child_by_field_id(self, field)
    }
    fn descendant_for_byte_range(self, start: usize, end: usize) -> Option<Self> {
        Node::descendant_for_byte_range(self, start, end)
    }
    fn descendant_for_point_range(self, start: Point, end: Point) -> Option<Self> {
        Node::descendant_for_point_range(self, start, end)
    }
}
macro_rules! cursor_navigation {
    ($cursor:ty) => {
        fn node(&self) -> Self::Node {
            <$cursor>::node(self)
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
        fn goto_previous_sibling(&mut self) -> bool {
            <$cursor>::goto_previous_sibling(self)
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
impl<'tree> CursorLike<'tree> for CachedCursor<'tree> {
    type Node = Node<'tree>;
    fn attributes(&mut self) -> Attributes<'tree> {
        CachedCursor::attributes(self)
    }
    fn field_id(&self) -> Option<u16> {
        let field = self.node().field_id();
        (field != 0).then_some(field)
    }
    cursor_navigation!(CachedCursor<'tree>);
}
