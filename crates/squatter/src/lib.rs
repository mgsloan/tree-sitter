//! Immutable, contiguous Tree-sitter trees and streaming queries.
//!
//! ```no_run
//! # fn example(tree: &tree_sitter::Tree) -> Result<(), tree_sitter_squatter::Error> {
//! let packed = tree_sitter_squatter::Tree::pack(tree)?;
//! for node in packed.root_node().preorder() {
//!     println!("{}: {:?}", node.kind(), node.byte_range());
//! }
//! let compact = packed.repack()?;
//! let bytes = compact.as_bytes();
//! # Ok(()) }
//! ```
//! Queries compile once and stream captures borrowed from their cursor:
//!
//! ```no_run
//! # fn query_example(language: &tree_sitter::Language, tree: &tree_sitter::Tree,
//! # source: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
//! use tree_sitter_squatter::{Tree, Query, QueryCursor};
//! let packed = Tree::pack(tree)?;
//! let query = Query::new(language, "(_) @node")?;
//! let mut cursor = QueryCursor::new();
//! let mut execution = cursor.execute(&query, packed.root_node(), source);
//! while let Some((result, index)) = execution.next_capture() {
//!     println!("{:?}", result.captures[index].node.byte_range());
//! }
//! if let Some(error) = execution.error() { return Err(error.into()); }
//! # Ok(()) }
//! ```
use std::{
    ffi::{CStr, c_char, c_void},
    marker::PhantomData,
    ops::Range,
    ptr::NonNull,
};
use tree_sitter::{Language, Point};

pub mod query;
pub use query::{
    Query, QueryCapture, QueryCursor, QueryError, QueryExecution, QueryExecutionError, QueryMatch,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum Error {
    InvalidArgument = 1,
    Allocation = 2,
    Overflow = 3,
    DictionaryFull = 4,
    InvalidSlab = 5,
    Language = 6,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = unsafe { CStr::from_ptr(ffi::sq_error_string(*self as i32)) };
        f.write_str(message.to_str().unwrap())
    }
}
impl std::error::Error for Error {}
fn error(code: i32) -> Error {
    match code {
        2 => Error::Allocation,
        3 => Error::Overflow,
        4 => Error::DictionaryFull,
        5 => Error::InvalidSlab,
        6 => Error::Language,
        _ => Error::InvalidArgument,
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PackOptions {
    pub initial_group_capacity: u32,
    pub repack: bool,
    pub symbol_presence: bool,
}
impl Default for PackOptions {
    fn default() -> Self {
        Self {
            initial_group_capacity: 0,
            repack: false,
            symbol_presence: true,
        }
    }
}

/// Owns a slab and retains its language; independent of the original tree.
pub struct Tree(NonNull<c_void>);
// The C slab is immutable and retains a thread-safe Tree-sitter language.
unsafe impl Send for Tree {}
unsafe impl Sync for Tree {}
impl Tree {
    pub fn pack(tree: &tree_sitter::Tree) -> Result<Self, Error> {
        Self::pack_with_options(tree, PackOptions::default())
    }
    pub fn pack_with_options(
        tree: &tree_sitter::Tree,
        options: PackOptions,
    ) -> Result<Self, Error> {
        let mut status = 0;
        let raw = unsafe {
            ffi::sq_tree_pack(
                tree.root_node().into_raw().tree.cast(),
                options,
                &mut status,
            )
        };
        NonNull::new(raw).map(Self).ok_or_else(|| error(status))
    }
    /// Fresh parse followed by conversion; the parser's existing language/options apply.
    pub fn parse(
        parser: &mut tree_sitter::Parser,
        source: impl AsRef<[u8]>,
    ) -> Result<Self, Error> {
        let source = source.as_ref();
        if source.len() > u32::MAX as usize {
            return Err(Error::Overflow);
        }
        let tree = parser.parse(source, None).ok_or(Error::InvalidArgument)?;
        Self::pack(&tree)
    }
    /// Loads a native-endian slab using the exact matching grammar.
    ///
    /// Structural validation rejects malformed data. Grammar identity is the
    /// caller's responsibility; the slab does not contain a grammar fingerprint.
    pub fn from_bytes(language: &Language, bytes: &[u8]) -> Result<Self, Error> {
        let raw_language = language.clone().into_raw();
        let mut status = 0;
        let raw = unsafe {
            ffi::sq_tree_from_bytes(
                raw_language.cast(),
                bytes.as_ptr().cast(),
                bytes.len(),
                &mut status,
            )
        };
        drop(unsafe { Language::from_raw(raw_language) });
        NonNull::new(raw).map(Self).ok_or_else(|| error(status))
    }
    pub fn repack(&self) -> Result<Self, Error> {
        let mut status = 0;
        let raw = unsafe { ffi::sq_tree_repack(self.0.as_ptr(), &mut status) };
        NonNull::new(raw).map(Self).ok_or_else(|| error(status))
    }
    pub fn as_bytes(&self) -> &[u8] {
        let mut length = 0;
        let data = unsafe { ffi::sq_tree_data(self.0.as_ptr(), &mut length) };
        unsafe { std::slice::from_raw_parts(data.cast(), length as usize) }
    }
    pub fn root_node(&self) -> Node<'_> {
        Node::from_raw(unsafe { ffi::sq_tree_root_node(self.0.as_ptr()) }).unwrap()
    }
    pub fn node_at_slot(&self, slot: u32) -> Option<Node<'_>> {
        Node::from_raw(unsafe { ffi::sq_tree_node_at_slot(self.0.as_ptr(), slot) })
    }
    pub fn group_count(&self) -> u32 {
        unsafe { ffi::sq_tree_group_count(self.0.as_ptr()) }
    }
    pub fn group_capacity(&self) -> u32 {
        unsafe { ffi::sq_tree_group_capacity(self.0.as_ptr()) }
    }
    pub fn slot_count(&self) -> u32 {
        unsafe { ffi::sq_tree_slot_count(self.0.as_ptr()) }
    }
    pub fn group_has_symbol(&self, group: u32, symbol: u16) -> bool {
        unsafe { ffi::sq_tree_group_has_symbol(self.0.as_ptr(), group, symbol) }
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        unsafe { ffi::sq_tree_delete(self.0.as_ptr()) };
    }
}
impl std::fmt::Debug for Tree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tree")
            .field("bytes", &self.as_bytes().len())
            .field("groups", &self.group_count())
            .finish()
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct RawNode {
    tree: *const c_void,
    slot: u32,
}
#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct RawPoint {
    row: u32,
    column: u32,
}
impl From<RawPoint> for Point {
    fn from(p: RawPoint) -> Self {
        Point::new(p.row as usize, p.column as usize)
    }
}
impl TryFrom<Point> for RawPoint {
    type Error = std::num::TryFromIntError;
    fn try_from(p: Point) -> Result<Self, Self::Error> {
        Ok(Self {
            row: p.row.try_into()?,
            column: p.column.try_into()?,
        })
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Node<'tree> {
    raw: RawNode,
    lifetime: PhantomData<&'tree Tree>,
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("slot", &self.slot())
            .field("kind", &self.kind())
            .field("bytes", &self.byte_range())
            .finish()
    }
}
impl<'tree> Node<'tree> {
    fn from_raw(raw: RawNode) -> Option<Self> {
        (!raw.tree.is_null()).then_some(Self {
            raw,
            lifetime: PhantomData,
        })
    }
    pub fn slot(self) -> u32 {
        self.raw.slot
    }
    pub fn byte_range(self) -> Range<usize> {
        self.start_byte()..self.end_byte()
    }
    pub fn utf8_text(self, source: &[u8]) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&source[self.byte_range()])
    }
    pub fn preorder(self) -> Preorder<'tree> {
        Preorder {
            next: Some(self),
            end_slot: unsafe { ffi::sq_node_end_slot(self.raw) },
        }
    }

    pub fn children(self) -> Children<'tree> {
        Children {
            next: self.child(0),
        }
    }
    pub fn named_children(self) -> impl Iterator<Item = Self> {
        self.children().filter(|n| n.is_named())
    }
    pub fn walk(self) -> Result<Cursor<'tree>, Error> {
        let raw = unsafe { ffi::sq_cursor_new(self.raw) };
        NonNull::new(raw)
            .map(|raw| Cursor {
                raw,
                lifetime: PhantomData,
            })
            .ok_or(Error::Allocation)
    }

    pub fn kind_id(self) -> u16 {
        unsafe { ffi::sq_node_symbol(self.raw) }
    }
    pub fn grammar_id(self) -> u16 {
        unsafe { ffi::sq_node_grammar_symbol(self.raw) }
    }
    pub fn kind(self) -> &'tree str {
        unsafe {
            CStr::from_ptr(ffi::sq_node_type(self.raw))
                .to_str()
                .unwrap()
        }
    }
    pub fn grammar_name(self) -> &'tree str {
        unsafe {
            CStr::from_ptr(ffi::sq_node_grammar_type(self.raw))
                .to_str()
                .unwrap()
        }
    }
    pub fn start_byte(self) -> usize {
        (unsafe { ffi::sq_node_start_byte(self.raw) }) as usize
    }
    pub fn end_byte(self) -> usize {
        (unsafe { ffi::sq_node_end_byte(self.raw) }) as usize
    }
    pub fn start_position(self) -> Point {
        unsafe { ffi::sq_node_start_point(self.raw) }.into()
    }
    pub fn end_position(self) -> Point {
        unsafe { ffi::sq_node_end_point(self.raw) }.into()
    }
    pub fn is_named(self) -> bool {
        unsafe { ffi::sq_node_is_named(self.raw) }
    }
    pub fn is_extra(self) -> bool {
        unsafe { ffi::sq_node_is_extra(self.raw) }
    }
    pub fn is_missing(self) -> bool {
        unsafe { ffi::sq_node_is_missing(self.raw) }
    }
    pub fn is_error(self) -> bool {
        unsafe { ffi::sq_node_is_error(self.raw) }
    }
    pub fn has_error(self) -> bool {
        unsafe { ffi::sq_node_has_error(self.raw) }
    }
    pub fn has_changes(self) -> bool {
        unsafe { ffi::sq_node_has_changes(self.raw) }
    }
    pub fn descendant_count(self) -> usize {
        (unsafe { ffi::sq_node_descendant_count(self.raw) }) as usize
    }
    pub fn child_count(self) -> usize {
        (unsafe { ffi::sq_node_child_count(self.raw) }) as usize
    }
    pub fn named_child_count(self) -> usize {
        (unsafe { ffi::sq_node_named_child_count(self.raw) }) as usize
    }
    pub fn field_id(self) -> u16 {
        unsafe { ffi::sq_node_field_id(self.raw) }
    }
    pub fn field_name(self) -> Option<&'tree str> {
        {
            let p = unsafe { ffi::sq_node_field_name(self.raw) };
            if p.is_null() {
                None
            } else {
                Some(unsafe { CStr::from_ptr(p).to_str().unwrap() })
            }
        }
    }
    pub fn parent(self) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_parent(self.raw) })
    }
    pub fn next_sibling(self) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_next_sibling(self.raw) })
    }
    pub fn prev_sibling(self) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_prev_sibling(self.raw) })
    }
    pub fn next_named_sibling(self) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_next_named_sibling(self.raw) })
    }
    pub fn prev_named_sibling(self) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_prev_named_sibling(self.raw) })
    }
    pub fn next_preorder(self) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_next_preorder(self.raw) })
    }
    pub fn prev_preorder(self) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_prev_preorder(self.raw) })
    }
    pub fn child(self, index: usize) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_child(self.raw, index.try_into().ok()?) })
    }
    pub fn named_child(self, index: usize) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_named_child(self.raw, index.try_into().ok()?) })
    }
    pub fn first_child_for_byte(self, index: usize) -> Option<Self> {
        Self::from_raw(unsafe {
            ffi::sq_node_first_child_for_byte(self.raw, index.try_into().ok()?)
        })
    }
    pub fn first_named_child_for_byte(self, index: usize) -> Option<Self> {
        Self::from_raw(unsafe {
            ffi::sq_node_first_named_child_for_byte(self.raw, index.try_into().ok()?)
        })
    }
    pub fn descendant_for_byte_range(self, start: usize, end: usize) -> Option<Self> {
        Self::from_raw(unsafe {
            ffi::sq_node_descendant_for_byte_range(
                self.raw,
                start.try_into().ok()?,
                end.try_into().ok()?,
            )
        })
    }
    pub fn named_descendant_for_byte_range(self, start: usize, end: usize) -> Option<Self> {
        Self::from_raw(unsafe {
            ffi::sq_node_named_descendant_for_byte_range(
                self.raw,
                start.try_into().ok()?,
                end.try_into().ok()?,
            )
        })
    }
    pub fn descendant_for_point_range(self, start: Point, end: Point) -> Option<Self> {
        Self::from_raw(unsafe {
            ffi::sq_node_descendant_for_point_range(
                self.raw,
                start.try_into().ok()?,
                end.try_into().ok()?,
            )
        })
    }
    pub fn named_descendant_for_point_range(self, start: Point, end: Point) -> Option<Self> {
        Self::from_raw(unsafe {
            ffi::sq_node_named_descendant_for_point_range(
                self.raw,
                start.try_into().ok()?,
                end.try_into().ok()?,
            )
        })
    }
    pub fn child_by_field_id(self, field: u16) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_child_by_field_id(self.raw, field) })
    }
    pub fn child_by_field_name(self, field: &str) -> Option<Self> {
        Self::from_raw(unsafe {
            ffi::sq_node_child_by_field_name(
                self.raw,
                field.as_ptr().cast(),
                field.len().try_into().ok()?,
            )
        })
    }
    pub fn child_with_descendant(self, descendant: Self) -> Option<Self> {
        Self::from_raw(unsafe { ffi::sq_node_child_with_descendant(self.raw, descendant.raw) })
    }
    pub fn has_supertype(self, symbol: u16) -> bool {
        unsafe { ffi::sq_node_has_supertype(self.raw, symbol) }
    }
}

pub struct Preorder<'tree> {
    next: Option<Node<'tree>>,
    end_slot: u32,
}
impl<'tree> Iterator for Preorder<'tree> {
    type Item = Node<'tree>;
    fn next(&mut self) -> Option<Self::Item> {
        let node = self.next?;
        self.next = node
            .next_preorder()
            .filter(|next| next.slot() < self.end_slot);
        Some(node)
    }
}
impl std::iter::FusedIterator for Preorder<'_> {}
pub struct Children<'tree> {
    next: Option<Node<'tree>>,
}
impl<'tree> Iterator for Children<'tree> {
    type Item = Node<'tree>;
    fn next(&mut self) -> Option<Self::Item> {
        let node = self.next?;
        self.next = Node::from_raw(unsafe { ffi::sq_node_next_sibling_including_empty(node.raw) });
        Some(node)
    }
}
impl std::iter::FusedIterator for Children<'_> {}

pub struct Cursor<'tree> {
    raw: NonNull<c_void>,
    lifetime: PhantomData<&'tree Tree>,
}
impl<'tree> Cursor<'tree> {
    /// Read a snapshot of the current node's attributes in one native call.
    pub fn attributes(&mut self) -> traits::Attributes<'tree> {
        let mut raw = std::mem::MaybeUninit::uninit();
        unsafe {
            ffi::sq_cursor_attributes(self.raw.as_ptr(), raw.as_mut_ptr());
            raw.assume_init().into_attributes()
        }
    }

    pub fn node(&self) -> Node<'tree> {
        Node::from_raw(unsafe { ffi::sq_cursor_node(self.raw.as_ptr()) }).unwrap()
    }
    pub fn depth(&self) -> u32 {
        unsafe { ffi::sq_cursor_depth(self.raw.as_ptr()) }
    }
    pub fn goto_first_child(&mut self) -> bool {
        unsafe { ffi::sq_cursor_goto_first_child(self.raw.as_ptr()) }
    }
    pub fn goto_last_child(&mut self) -> bool {
        unsafe { ffi::sq_cursor_goto_last_child(self.raw.as_ptr()) }
    }
    pub fn goto_next_sibling(&mut self) -> bool {
        unsafe { ffi::sq_cursor_goto_next_sibling(self.raw.as_ptr()) }
    }
    pub fn goto_parent(&mut self) -> bool {
        unsafe { ffi::sq_cursor_goto_parent(self.raw.as_ptr()) }
    }
}
impl Drop for Cursor<'_> {
    fn drop(&mut self) {
        unsafe { ffi::sq_cursor_delete(self.raw.as_ptr()) };
    }
}
#[repr(C)]
struct RawCursorAttributes {
    kind: *const std::ffi::c_char,
    grammar_name: *const std::ffi::c_char,
    start_byte: u32,
    end_byte: u32,
    start_point: RawPoint,
    end_point: RawPoint,
    child_count: u32,
    named_child_count: u32,
    descendant_count: u32,
    symbol: u16,
    grammar_symbol: u16,
    field_id: u16,
    is_named: bool,
    is_extra: bool,
    is_missing: bool,
    is_error: bool,
    has_error: bool,
}
impl RawCursorAttributes {
    // Only called with an initialized snapshot from a live cursor. Its language
    // strings are retained by the tree, so they may outlive the cursor itself.
    unsafe fn into_attributes<'tree>(self) -> traits::Attributes<'tree> {
        traits::Attributes {
            kind: unsafe { CStr::from_ptr(self.kind) }.to_str().unwrap(),
            grammar_name: unsafe { CStr::from_ptr(self.grammar_name) }
                .to_str()
                .unwrap(),
            kind_id: self.symbol,
            grammar_id: self.grammar_symbol,
            start_byte: self.start_byte as usize,
            end_byte: self.end_byte as usize,
            start_position: self.start_point.into(),
            end_position: self.end_point.into(),
            is_named: self.is_named,
            is_extra: self.is_extra,
            is_missing: self.is_missing,
            is_error: self.is_error,
            has_error: self.has_error,
            has_changes: false,
            child_count: self.child_count as usize,
            named_child_count: self.named_child_count as usize,
            descendant_count: self.descendant_count as usize,
        }
    }
}

mod ffi {
    use super::*;
    unsafe extern "C" {
        pub fn sq_error_string(error: i32) -> *const c_char;
        pub fn sq_tree_pack(
            tree: *const c_void,
            options: PackOptions,
            error: *mut i32,
        ) -> *mut c_void;
        pub fn sq_tree_from_bytes(
            language: *const c_void,
            bytes: *const c_void,
            length: usize,
            error: *mut i32,
        ) -> *mut c_void;
        pub fn sq_tree_repack(tree: *const c_void, error: *mut i32) -> *mut c_void;
        pub fn sq_tree_data(tree: *const c_void, length: *mut u32) -> *const c_void;
        pub fn sq_tree_delete(tree: *mut c_void);
        pub fn sq_tree_root_node(tree: *const c_void) -> RawNode;
        pub fn sq_tree_node_at_slot(tree: *const c_void, slot: u32) -> RawNode;
        pub fn sq_tree_group_count(tree: *const c_void) -> u32;
        pub fn sq_tree_group_capacity(tree: *const c_void) -> u32;
        pub fn sq_tree_slot_count(tree: *const c_void) -> u32;
        pub fn sq_tree_group_has_symbol(tree: *const c_void, group: u32, symbol: u16) -> bool;
        pub fn sq_node_child_by_field_id(node: RawNode, field: u16) -> RawNode;
        pub fn sq_node_child_by_field_name(
            node: RawNode,
            field: *const c_char,
            length: u32,
        ) -> RawNode;
        pub fn sq_node_child_with_descendant(node: RawNode, descendant: RawNode) -> RawNode;
        pub fn sq_node_has_supertype(node: RawNode, symbol: u16) -> bool;
        pub fn sq_cursor_attributes(cursor: *mut c_void, out: *mut RawCursorAttributes);
        pub fn sq_cursor_new(node: RawNode) -> *mut c_void;
        pub fn sq_cursor_delete(cursor: *mut c_void);
        pub fn sq_cursor_node(cursor: *const c_void) -> RawNode;
        pub fn sq_cursor_depth(cursor: *const c_void) -> u32;
        pub fn sq_node_symbol(node: RawNode) -> u16;
        pub fn sq_node_grammar_symbol(node: RawNode) -> u16;
        pub fn sq_node_type(node: RawNode) -> *const c_char;
        pub fn sq_node_grammar_type(node: RawNode) -> *const c_char;
        pub fn sq_node_start_byte(node: RawNode) -> u32;
        pub fn sq_node_end_byte(node: RawNode) -> u32;
        pub fn sq_node_start_point(node: RawNode) -> RawPoint;
        pub fn sq_node_end_point(node: RawNode) -> RawPoint;
        pub fn sq_node_is_named(node: RawNode) -> bool;
        pub fn sq_node_is_extra(node: RawNode) -> bool;
        pub fn sq_node_is_missing(node: RawNode) -> bool;
        pub fn sq_node_is_error(node: RawNode) -> bool;
        pub fn sq_node_has_error(node: RawNode) -> bool;
        pub fn sq_node_has_changes(node: RawNode) -> bool;
        pub fn sq_node_end_slot(node: RawNode) -> u32;
        pub fn sq_node_descendant_count(node: RawNode) -> u32;
        pub fn sq_node_child_count(node: RawNode) -> u32;
        pub fn sq_node_named_child_count(node: RawNode) -> u32;
        pub fn sq_node_field_id(node: RawNode) -> u16;
        pub fn sq_node_field_name(node: RawNode) -> *const c_char;
        pub fn sq_node_parent(node: RawNode) -> RawNode;
        pub fn sq_node_next_sibling(node: RawNode) -> RawNode;
        pub fn sq_node_next_sibling_including_empty(node: RawNode) -> RawNode;
        pub fn sq_node_prev_sibling(node: RawNode) -> RawNode;
        pub fn sq_node_next_named_sibling(node: RawNode) -> RawNode;
        pub fn sq_node_prev_named_sibling(node: RawNode) -> RawNode;
        pub fn sq_node_next_preorder(node: RawNode) -> RawNode;
        pub fn sq_node_prev_preorder(node: RawNode) -> RawNode;
        pub fn sq_node_child(node: RawNode, index: u32) -> RawNode;
        pub fn sq_node_named_child(node: RawNode, index: u32) -> RawNode;
        pub fn sq_node_first_child_for_byte(node: RawNode, index: u32) -> RawNode;
        pub fn sq_node_first_named_child_for_byte(node: RawNode, index: u32) -> RawNode;
        pub fn sq_node_descendant_for_byte_range(node: RawNode, start: u32, end: u32) -> RawNode;
        pub fn sq_node_named_descendant_for_byte_range(
            node: RawNode,
            start: u32,
            end: u32,
        ) -> RawNode;
        pub fn sq_node_descendant_for_point_range(
            node: RawNode,
            start: RawPoint,
            end: RawPoint,
        ) -> RawNode;
        pub fn sq_node_named_descendant_for_point_range(
            node: RawNode,
            start: RawPoint,
            end: RawPoint,
        ) -> RawNode;
        pub fn sq_cursor_goto_first_child(cursor: *mut c_void) -> bool;
        pub fn sq_cursor_goto_last_child(cursor: *mut c_void) -> bool;
        pub fn sq_cursor_goto_next_sibling(cursor: *mut c_void) -> bool;
        pub fn sq_cursor_goto_parent(cursor: *mut c_void) -> bool;
    }
}

pub mod traits;
