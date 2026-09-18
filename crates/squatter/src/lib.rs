//! Immutable, contiguous Tree-sitter trees and streaming queries.
//!
//! ```no_run
//! # fn example(tree: &tree_sitter::Tree) -> Result<(), tree_squatter::Error> {
//! let grammar = tree_squatter::Grammar::new(&tree.language().to_owned())?;
//! let packed = tree_squatter::Tree::pack(&grammar, tree)?;
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
//! use tree_squatter::{Tree, Query, QueryCursor};
//! let grammar = tree_squatter::Grammar::new(&tree.language().to_owned())?;
//! let packed = Tree::pack(&grammar, tree)?;
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
    ops::{Deref, Range},
    ptr::NonNull,
};
use tree_sitter::Language;
use tree_sitter::Point;

/// Slab version and actual C build configuration (not a grammar/runtime identity).
pub fn representation_id() -> u64 {
    unsafe extern "C" {
        fn sq_representation_id() -> u64;
    }
    unsafe { sq_representation_id() }
}

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
    /// Store source row/column positions. Without them, point APIs return byte
    /// offsets as columns on row zero.
    pub points: bool,
}
impl Default for PackOptions {
    fn default() -> Self {
        Self {
            initial_group_capacity: 0,
            repack: false,
            symbol_presence: true,
            points: true,
        }
    }
}

/// Owns a slab and retains its prepared grammar; independent of the original tree.
pub struct Tree(NonNull<c_void>);

/// Prepared immutable grammar tables. Clones share metadata; contexts and trees
/// retain it independently. Native grammar libraries must outlive every handle.
pub struct Grammar(NonNull<c_void>);
// Metadata is immutable after construction; native ownership uses atomic references.
unsafe impl Send for Grammar {}
unsafe impl Sync for Grammar {}
impl Grammar {
    pub fn new(language: &Language) -> Result<Self, Error> {
        let raw_language = language.clone().into_raw();
        let mut status = 0;
        let raw = unsafe { ffi::sq_grammar_new(raw_language.cast(), &mut status) };
        drop(unsafe { Language::from_raw(raw_language) });
        NonNull::new(raw).map(Self).ok_or_else(|| error(status))
    }

    /// Restore a dictionary produced by [`Self::cache`] for the exact same
    /// grammar/runtime, copying directly from the supplied bytes.
    /// Other lookup tables are derived from the language. Invalid bytes fail;
    /// callers may fall back to [`Self::new`].
    pub fn from_cache(language: &Language, bytes: &[u8]) -> Result<Self, Error> {
        let raw_language = language.clone().into_raw();
        let mut status = 0;
        let raw = unsafe {
            ffi::sq_grammar_new_with_cache(
                raw_language.cast(),
                bytes.as_ptr().cast(),
                bytes.len(),
                &mut status,
            )
        };
        drop(unsafe { Language::from_raw(raw_language) });
        NonNull::new(raw).map(Self).ok_or_else(|| error(status))
    }

    pub fn language(&self) -> Language {
        unsafe {
            let language = std::mem::ManuallyDrop::new(Language::from_raw(
                ffi::sq_grammar_language(self.0.as_ptr()).cast(),
            ));
            Language::clone(&language)
        }
    }

    /// Serialize the costly dictionary. Grammars with at most eight supertypes
    /// return an empty vector.
    pub fn cache(&self) -> Result<Vec<u8>, Error> {
        let size = unsafe { ffi::sq_grammar_cache_size(self.0.as_ptr()) as usize };
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut bytes = vec![0; size];
        let mut status = 0;
        let ok = unsafe {
            ffi::sq_grammar_copy_cache(
                self.0.as_ptr(),
                bytes.as_mut_ptr().cast(),
                size,
                &mut status,
            )
        };
        ok.then_some(bytes).ok_or_else(|| error(status))
    }
}
impl Clone for Grammar {
    fn clone(&self) -> Self {
        unsafe {
            ffi::sq_grammar_copy(self.0.as_ptr());
        }
        Self(self.0)
    }
}
impl Drop for Grammar {
    fn drop(&mut self) {
        unsafe { ffi::sq_grammar_delete(self.0.as_ptr()) }
    }
}

/// Reusable worker-local packing scratch shared across grammars.
/// Output trees do not borrow the context. Separate contexts can share a grammar.
pub struct PackContext(NonNull<c_void>);
// Moving exclusively owned scratch is safe; shared concurrent use is not.
unsafe impl Send for PackContext {}
impl PackContext {
    pub fn new() -> Result<Self, Error> {
        let mut status = 0;
        let raw = unsafe { ffi::sq_pack_context_new(&mut status) };
        NonNull::new(raw).map(Self).ok_or_else(|| error(status))
    }

    pub fn pack(&mut self, grammar: &Grammar, tree: &tree_sitter::Tree) -> Result<Tree, Error> {
        self.pack_with_options(grammar, tree, PackOptions::default())
    }

    pub fn pack_with_options(
        &mut self,
        grammar: &Grammar,
        tree: &tree_sitter::Tree,
        options: PackOptions,
    ) -> Result<Tree, Error> {
        let mut status = 0;
        let raw = unsafe {
            ffi::sq_pack_context_pack(
                self.0.as_ptr(),
                grammar.0.as_ptr(),
                tree.root_node().into_raw().tree.cast(),
                options,
                &mut status,
            )
        };
        NonNull::new(raw).map(Tree).ok_or_else(|| error(status))
    }

    /// Release high-water scratch.
    pub fn trim(&mut self) {
        unsafe { ffi::sq_pack_context_trim(self.0.as_ptr()) }
    }
}
impl Drop for PackContext {
    fn drop(&mut self) {
        unsafe { ffi::sq_pack_context_delete(self.0.as_ptr()) }
    }
}
/// Validated view of externally owned immutable slab bytes.
///
/// Dereferencing exposes the read-only tree APIs. Nodes and query executions
/// borrow this descriptor, which in turn cannot outlive the supplied bytes.
///
/// ```compile_fail
/// use tree_squatter::{BorrowedTree, Tree};
/// fn dangling(grammar: &tree_squatter::Grammar) -> BorrowedTree<'static> {
///     let bytes = vec![0u8; 128];
///     Tree::from_bytes_borrowed(grammar, &bytes).unwrap()
/// }
/// ```
pub struct BorrowedTree<'a> {
    tree: Tree,
    bytes: PhantomData<&'a [u8]>,
}
impl Deref for BorrowedTree<'_> {
    type Target = Tree;
    fn deref(&self) -> &Tree {
        &self.tree
    }
}

/// Ownership of immutable storage whose address survives moves of its owner.
///
/// # Safety
/// Every call must return the same slice (address and length). Its allocation
/// must remain alive and immutable until the owner is dropped, even when the
/// owner is moved or accessed from another thread. No external party may resize,
/// unmap, or mutate it. Inline arrays and mutable mappings do not meet this
/// contract. Alignment is checked by the loader, not required by this trait.
pub unsafe trait StableSlab: Send + Sync + 'static {
    fn bytes(&self) -> &[u8];
}

/// A validated descriptor retaining the owner of its immutable slab storage.
/// Nodes borrow this wrapper; the native descriptor is destroyed before storage.
///
/// ```compile_fail
/// use tree_squatter::{Node, StableSlab, Tree};
/// fn dangling(grammar: &tree_squatter::Grammar, owner: impl StableSlab) -> Node<'static> {
///     Tree::from_owned_slab(grammar, owner).unwrap().root_node()
/// }
/// ```
pub struct BackedTree {
    // Declaration order is important: fields drop in this order.
    tree: Tree,
    _owner: Box<dyn StableSlab>,
}
impl BackedTree {
    /// Copy into owned aligned storage without checking auxiliary semantics.
    pub fn detach(&self) -> Result<Tree, Error> {
        let bytes = self.as_bytes();
        let mut status = 0;
        // The new descriptor independently retains this tree's prepared grammar.
        let raw = unsafe {
            ffi::sq_tree_from_bytes_safety_checked(
                ffi::sq_tree_grammar(self.tree.0.as_ptr()),
                bytes.as_ptr().cast(),
                bytes.len(),
                &mut status,
            )
        };
        NonNull::new(raw).map(Tree).ok_or_else(|| error(status))
    }
}
impl Deref for BackedTree {
    type Target = Tree;
    fn deref(&self) -> &Tree {
        &self.tree
    }
}

// The C slab is immutable and retains a thread-safe Tree-sitter language.
unsafe impl Send for Tree {}
unsafe impl Sync for Tree {}
impl Tree {
    /// Safety-validates an aligned slab and retains its owner without copying.
    /// Misaligned input returns `Error::InvalidArgument`. On failure the owner
    /// is dropped. No lifetime extension or exposed raw descriptor is involved.
    pub fn from_owned_slab(grammar: &Grammar, owner: impl StableSlab) -> Result<BackedTree, Error> {
        let bytes = owner.bytes();
        let mut status = 0;
        let raw = unsafe {
            ffi::sq_tree_from_bytes_borrowed_safety_checked(
                grammar.0.as_ptr(),
                bytes.as_ptr().cast(),
                bytes.len(),
                &mut status,
            )
        };
        let tree = NonNull::new(raw).map(Self).ok_or_else(|| error(status))?;
        Ok(BackedTree {
            tree,
            _owner: Box::new(owner),
        })
    }
    pub fn pack(grammar: &Grammar, tree: &tree_sitter::Tree) -> Result<Self, Error> {
        Self::pack_with_options(grammar, tree, PackOptions::default())
    }
    pub fn pack_with_options(
        grammar: &Grammar,
        tree: &tree_sitter::Tree,
        options: PackOptions,
    ) -> Result<Self, Error> {
        let mut status = 0;
        let raw = unsafe {
            ffi::sq_tree_pack(
                grammar.0.as_ptr(),
                tree.root_node().into_raw().tree.cast(),
                options,
                &mut status,
            )
        };
        NonNull::new(raw).map(Self).ok_or_else(|| error(status))
    }
    /// Fresh parse followed by conversion; the parser's existing language/options apply.
    pub fn parse(
        grammar: &Grammar,
        parser: &mut tree_sitter::Parser,
        source: impl AsRef<[u8]>,
    ) -> Result<Self, Error> {
        Self::parse_with_options(grammar, parser, source, PackOptions::default())
    }
    /// Fresh parse followed by conversion with the requested slab configuration.
    pub fn parse_with_options(
        grammar: &Grammar,
        parser: &mut tree_sitter::Parser,
        source: impl AsRef<[u8]>,
        options: PackOptions,
    ) -> Result<Self, Error> {
        let source = source.as_ref();
        if source.len() > u32::MAX as usize {
            return Err(Error::Overflow);
        }
        let tree = parser.parse(source, None).ok_or(Error::InvalidArgument)?;
        Self::pack_with_options(grammar, &tree, options)
    }
    /// Loads a little-endian slab using the exact matching grammar.
    ///
    /// Structural validation rejects malformed data. Grammar identity is the
    /// caller's responsibility; the slab does not contain a grammar fingerprint.
    pub fn from_bytes(grammar: &Grammar, bytes: &[u8]) -> Result<Self, Error> {
        Self::load_bytes(grammar, bytes, false)
    }
    /// Loads a copied slab with structural safety validation, not an integrity check.
    ///
    /// Retains layout, topology, symbol/dictionary index, and coordinate checks.
    /// Does not reconstruct auxiliary symbol-presence membership or require its
    /// padding (or unused dictionary bits) to be canonical. Corrupt but bounded
    /// auxiliary data may therefore yield incorrect query results.
    ///
    /// The exact matching grammar is required, as with [`Self::from_bytes`].
    /// Neither loader verifies agreement with source text. This entry point is
    /// intended for caches whose policy deliberately omits semantic integrity
    /// validation; it is not an unchecked or zero-copy loader.
    pub fn from_bytes_safety_checked(grammar: &Grammar, bytes: &[u8]) -> Result<Self, Error> {
        Self::load_bytes(grammar, bytes, true)
    }

    fn load_bytes(grammar: &Grammar, bytes: &[u8], safety_only: bool) -> Result<Self, Error> {
        let mut status = 0;
        let load = if safety_only {
            ffi::sq_tree_from_bytes_safety_checked
        } else {
            ffi::sq_tree_from_bytes
        };
        let raw = unsafe {
            load(
                grammar.0.as_ptr(),
                bytes.as_ptr().cast(),
                bytes.len(),
                &mut status,
            )
        };
        NonNull::new(raw).map(Self).ok_or_else(|| error(status))
    }
    /// Validates without copying bytes, using the exact matching grammar.
    ///
    /// Input must be aligned to 8 bytes (64 in the experimental alignment build).
    /// Misaligned input returns `Error::InvalidArgument`; `from_bytes` accepts
    /// arbitrary alignment by copying. Only the runtime descriptor is owned.
    pub fn from_bytes_borrowed<'a>(
        grammar: &Grammar,
        bytes: &'a [u8],
    ) -> Result<BorrowedTree<'a>, Error> {
        let mut status = 0;
        let raw = unsafe {
            ffi::sq_tree_from_bytes_borrowed(
                grammar.0.as_ptr(),
                bytes.as_ptr().cast(),
                bytes.len(),
                &mut status,
            )
        };
        let tree = NonNull::new(raw).map(Self).ok_or_else(|| error(status))?;
        Ok(BorrowedTree {
            tree,
            bytes: PhantomData,
        })
    }
    pub fn repack(&self) -> Result<Self, Error> {
        let mut status = 0;
        let raw = unsafe { ffi::sq_tree_repack(self.0.as_ptr(), &mut status) };
        NonNull::new(raw).map(Self).ok_or_else(|| error(status))
    }
    /// Size of the compact serialized slab, excluding transient spare capacity.
    pub fn compact_size(&self) -> usize {
        unsafe { ffi::sq_tree_compact_size(self.0.as_ptr()) as usize }
    }

    /// Serialize the costly grammar-derived dictionary retained by this tree.
    pub fn grammar_cache(&self) -> Result<Vec<u8>, Error> {
        let size = unsafe { ffi::sq_tree_grammar_cache_size(self.0.as_ptr()) as usize };
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut result = vec![0; size];
        let mut status = 0;
        let ok = unsafe {
            ffi::sq_tree_copy_grammar_cache(
                self.0.as_ptr(),
                result.as_mut_ptr().cast(),
                size,
                &mut status,
            )
        };
        ok.then_some(result).ok_or_else(|| error(status))
    }

    /// Copy used columns directly into a compact destination without allocating
    /// an intermediate tree. Requires exactly `compact_size()` bytes; arbitrary
    /// destination alignment is supported. Success initializes every byte.
    pub fn copy_compact_into<'a>(
        &self,
        destination: &'a mut [std::mem::MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Error> {
        let mut status = 0;
        let ok = unsafe {
            ffi::sq_tree_copy_compact(
                self.0.as_ptr(),
                destination.as_mut_ptr().cast(),
                destination.len(),
                &mut status,
            )
        };
        if !ok {
            return Err(error(status));
        }
        // The native writer initializes header, columns, padding, and tail on
        // success. It accepts unaligned storage and never reads destination.
        Ok(unsafe {
            std::slice::from_raw_parts_mut(destination.as_mut_ptr().cast(), destination.len())
        })
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
    /// Whether this tree stores the source's row/column positions.
    pub fn has_points(&self) -> bool {
        unsafe { ffi::sq_tree_has_points(self.0.as_ptr()) }
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
    /// Physical slot in reverse preorder; decreasing slots advance preorder.
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
            first_slot: unsafe { ffi::sq_node_first_slot(self.raw) },
        }
    }

    /// Native preorder iterator.
    /// Returned nodes borrow the tree, independently of the iterator.
    pub fn node_iterator(self) -> Result<NodeIterator<'tree>, Error> {
        let raw = unsafe { ffi::sq_node_iterator_new(self.raw) };
        NonNull::new(raw)
            .map(|raw| NodeIterator {
                raw,
                current: None,
                lifetime: PhantomData,
            })
            .ok_or(Error::Allocation)
    }

    /// Scan this node and its descendants in preorder, matching public kind IDs.
    /// The set is reusable across trees of the same language. Duplicate IDs yield
    /// no duplicate nodes; an empty set yields no nodes.
    pub fn descendants_matching_kinds<'kinds>(
        self,
        kinds: &'kinds KindSet,
    ) -> KindMatches<'tree, 'kinds> {
        KindMatches {
            kinds,
            scan: self.preorder(),
        }
    }

    /// Test for a child without counting siblings.
    pub fn has_children(self) -> bool {
        self.child(0).is_some()
    }

    /// Stops at the first named child; may scan unnamed children.
    pub fn has_named_children(self) -> bool {
        self.named_child(0).is_some()
    }

    pub fn children_by_field_id(self, field: u16) -> impl Iterator<Item = Self> {
        self.children()
            .take_while(move |_| field != 0)
            .filter(move |node| node.field_id() == field)
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

    /// Read the constant-time attributes in one native call. Counts are separate.
    pub fn attributes(self) -> traits::Attributes<'tree> {
        let mut raw = std::mem::MaybeUninit::uninit();
        unsafe {
            ffi::sq_node_attributes(self.raw, raw.as_mut_ptr());
            raw.assume_init().into_attributes()
        }
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
    /// Whether any node in this physical block has an error in its subtree.
    /// May be true for an error-free node sharing a block with an erroneous node.
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

/// A reusable set of public kind IDs, interpreted in the scanned tree's language.
#[derive(Clone, Debug, Default)]
pub struct KindSet {
    ids: Vec<u16>,
    words: Vec<u64>,
}
impl KindSet {
    pub fn new(kinds: impl IntoIterator<Item = u16>) -> Self {
        kinds.into_iter().collect()
    }
    pub fn contains(&self, kind: u16) -> bool {
        self.words
            .get(kind as usize / 64)
            .is_some_and(|word| word & (1u64 << (kind % 64)) != 0)
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}
impl FromIterator<u16> for KindSet {
    fn from_iter<I: IntoIterator<Item = u16>>(kinds: I) -> Self {
        let mut ids: Vec<_> = kinds.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        let mut words = vec![0; ids.last().map_or(0, |&kind| kind as usize / 64 + 1)];
        for &kind in &ids {
            words[kind as usize / 64] |= 1u64 << (kind % 64);
        }
        Self { ids, words }
    }
}

/// Preorder traversal filtered by public kind IDs.
pub struct KindMatches<'tree, 'kinds> {
    kinds: &'kinds KindSet,
    scan: Preorder<'tree>,
}
impl<'tree> Iterator for KindMatches<'tree, '_> {
    type Item = Node<'tree>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.kinds.is_empty() {
            return None;
        }
        self.scan.find(|node| self.kinds.contains(node.kind_id()))
    }
}
impl std::iter::FusedIterator for KindMatches<'_, '_> {}

pub struct Preorder<'tree> {
    next: Option<Node<'tree>>,
    first_slot: u32,
}
impl<'tree> Iterator for Preorder<'tree> {
    type Item = Node<'tree>;
    fn next(&mut self) -> Option<Self::Item> {
        let node = self.next?;
        self.next = node
            .next_preorder()
            .filter(|next| next.slot() >= self.first_slot);
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

/// Stackless native iteration over a node and its descendants.
/// Attribute access refers to the most recently yielded node; before the first
/// next() and after exhaustion it returns None.
pub struct NodeIterator<'tree> {
    raw: NonNull<c_void>,
    current: Option<Node<'tree>>,
    lifetime: PhantomData<&'tree Tree>,
}
impl<'tree> Iterator for NodeIterator<'tree> {
    type Item = Node<'tree>;
    fn next(&mut self) -> Option<Self::Item> {
        self.current = Node::from_raw(unsafe { ffi::sq_node_iterator_next(self.raw.as_ptr()) });
        self.current
    }
}
impl std::iter::FusedIterator for NodeIterator<'_> {}
impl<'tree> NodeIterator<'tree> {
    pub fn node(&self) -> Option<Node<'tree>> {
        self.current
    }
    /// Read the last yielded node's constant-time attributes.
    pub fn attributes(&mut self) -> Option<traits::Attributes<'tree>> {
        self.current?;
        let mut raw = std::mem::MaybeUninit::uninit();
        Some(unsafe {
            ffi::sq_node_iterator_attributes(self.raw.as_ptr(), raw.as_mut_ptr());
            raw.assume_init().into_attributes()
        })
    }
    /// Read the last yielded node's kind directly from fixed-width storage.
    pub fn kind_id(&mut self) -> Option<u16> {
        self.current?;
        Some(unsafe { ffi::sq_node_iterator_symbol(self.raw.as_ptr()) })
    }
    /// Read only byte coordinates, without decoding point coordinates or IDs.
    pub fn byte_range(&mut self) -> Option<Range<usize>> {
        self.current?;
        let (mut start, mut end) = (0, 0);
        unsafe { ffi::sq_node_iterator_byte_range(self.raw.as_ptr(), &mut start, &mut end) };
        Some(start as usize..end as usize)
    }
    pub fn field_id(&mut self) -> Option<u16> {
        self.current?;
        let field = unsafe { ffi::sq_node_iterator_field_id(self.raw.as_ptr()) };
        (field != 0).then_some(field)
    }
}
impl Drop for NodeIterator<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::sq_node_iterator_delete(self.raw.as_ptr());
        }
    }
}

pub struct Cursor<'tree> {
    raw: NonNull<c_void>,
    lifetime: PhantomData<&'tree Tree>,
}
impl<'tree> Cursor<'tree> {
    /// Read the current node's constant-time attributes in one native call.
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
    /// Start at another node, retaining allocated ancestor storage.
    pub fn reset(&mut self, node: Node<'tree>) {
        unsafe { ffi::sq_cursor_reset(self.raw.as_ptr(), node.raw) }
    }
    /// Can scan preceding siblings; does not reconstruct the parent.
    pub fn goto_previous_sibling(&mut self) -> bool {
        unsafe { ffi::sq_cursor_goto_previous_sibling(self.raw.as_ptr()) }
    }
    /// Seek the first child ending after the byte, returning its child index.
    /// Can scan children. Failure leaves the cursor unchanged.
    pub fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<usize> {
        let index = unsafe {
            ffi::sq_cursor_goto_first_child_for_byte(self.raw.as_ptr(), byte.try_into().ok()?)
        };
        index.try_into().ok()
    }
    /// Seek the first child ending after the point, returning its child index.
    /// Can scan children. Failure leaves the cursor unchanged.
    pub fn goto_first_child_for_point(&mut self, point: Point) -> Option<usize> {
        let index = unsafe {
            ffi::sq_cursor_goto_first_child_for_point(self.raw.as_ptr(), point.try_into().ok()?)
        };
        index.try_into().ok()
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
    // Only called with an initialized snapshot from a live tree. Its language
    // strings are retained by the tree and may outlive a cursor or iterator.
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
        }
    }
}

mod ffi {
    use super::*;
    unsafe extern "C" {
        pub fn sq_error_string(error: i32) -> *const c_char;
        pub fn sq_tree_pack(
            grammar: *mut c_void,
            tree: *const c_void,
            options: PackOptions,
            error: *mut i32,
        ) -> *mut c_void;
        pub fn sq_pack_context_new(error: *mut i32) -> *mut c_void;
        pub fn sq_grammar_new(language: *const c_void, error: *mut i32) -> *mut c_void;
        pub fn sq_grammar_new_with_cache(
            language: *const c_void,
            bytes: *const c_void,
            length: usize,
            error: *mut i32,
        ) -> *mut c_void;
        pub fn sq_grammar_copy(grammar: *mut c_void) -> *mut c_void;
        pub fn sq_grammar_delete(grammar: *mut c_void);
        pub fn sq_grammar_language(grammar: *const c_void) -> *const c_void;
        pub fn sq_grammar_cache_size(grammar: *const c_void) -> u32;
        pub fn sq_grammar_copy_cache(
            grammar: *const c_void,
            bytes: *mut c_void,
            length: usize,
            error: *mut i32,
        ) -> bool;
        pub fn sq_tree_grammar(tree: *const c_void) -> *mut c_void;
        pub fn sq_pack_context_pack(
            context: *mut c_void,
            grammar: *mut c_void,
            tree: *const c_void,
            options: PackOptions,
            error: *mut i32,
        ) -> *mut c_void;
        pub fn sq_pack_context_trim(context: *mut c_void);
        pub fn sq_pack_context_delete(context: *mut c_void);
        pub fn sq_tree_from_bytes(
            grammar: *mut c_void,
            bytes: *const c_void,
            length: usize,
            error: *mut i32,
        ) -> *mut c_void;
        pub fn sq_tree_from_bytes_borrowed(
            grammar: *mut c_void,
            bytes: *const c_void,
            length: usize,
            error: *mut i32,
        ) -> *mut c_void;
        pub fn sq_tree_from_bytes_safety_checked(
            grammar: *mut c_void,
            bytes: *const c_void,
            length: usize,
            error: *mut i32,
        ) -> *mut c_void;
        pub fn sq_tree_from_bytes_borrowed_safety_checked(
            grammar: *mut c_void,
            bytes: *const c_void,
            length: usize,
            error: *mut i32,
        ) -> *mut c_void;
        pub fn sq_tree_repack(tree: *const c_void, error: *mut i32) -> *mut c_void;
        pub fn sq_tree_compact_size(tree: *const c_void) -> u32;
        pub fn sq_tree_copy_compact(
            tree: *const c_void,
            destination: *mut c_void,
            length: usize,
            error: *mut i32,
        ) -> bool;
        pub fn sq_tree_data(tree: *const c_void, length: *mut u32) -> *const c_void;
        pub fn sq_tree_grammar_cache_size(tree: *const c_void) -> u32;
        pub fn sq_tree_copy_grammar_cache(
            tree: *const c_void,
            destination: *mut c_void,
            length: usize,
            error: *mut i32,
        ) -> bool;
        pub fn sq_tree_delete(tree: *mut c_void);
        pub fn sq_tree_root_node(tree: *const c_void) -> RawNode;
        pub fn sq_tree_node_at_slot(tree: *const c_void, slot: u32) -> RawNode;
        pub fn sq_tree_group_count(tree: *const c_void) -> u32;
        pub fn sq_tree_group_capacity(tree: *const c_void) -> u32;
        pub fn sq_tree_slot_count(tree: *const c_void) -> u32;
        pub fn sq_tree_has_points(tree: *const c_void) -> bool;
        pub fn sq_tree_group_has_symbol(tree: *const c_void, group: u32, symbol: u16) -> bool;
        pub fn sq_node_child_by_field_id(node: RawNode, field: u16) -> RawNode;
        pub fn sq_node_child_by_field_name(
            node: RawNode,
            field: *const c_char,
            length: u32,
        ) -> RawNode;
        pub fn sq_node_child_with_descendant(node: RawNode, descendant: RawNode) -> RawNode;
        pub fn sq_node_has_supertype(node: RawNode, symbol: u16) -> bool;
        pub fn sq_node_iterator_new(node: RawNode) -> *mut c_void;
        pub fn sq_node_iterator_delete(iterator: *mut c_void);
        pub fn sq_node_iterator_next(iterator: *mut c_void) -> RawNode;
        pub fn sq_node_iterator_attributes(iterator: *mut c_void, out: *mut RawCursorAttributes);
        pub fn sq_node_iterator_field_id(iterator: *mut c_void) -> u16;
        pub fn sq_node_iterator_symbol(iterator: *mut c_void) -> u16;
        pub fn sq_node_iterator_byte_range(iterator: *mut c_void, start: *mut u32, end: *mut u32);
        pub fn sq_node_attributes(node: RawNode, out: *mut RawCursorAttributes);
        pub fn sq_cursor_attributes(cursor: *mut c_void, out: *mut RawCursorAttributes);
        pub fn sq_cursor_new(node: RawNode) -> *mut c_void;
        pub fn sq_cursor_delete(cursor: *mut c_void);
        pub fn sq_cursor_node(cursor: *const c_void) -> RawNode;
        pub fn sq_cursor_depth(cursor: *const c_void) -> u32;
        pub fn sq_cursor_reset(cursor: *mut c_void, node: RawNode);
        pub fn sq_cursor_goto_previous_sibling(cursor: *mut c_void) -> bool;
        pub fn sq_cursor_goto_first_child_for_byte(cursor: *mut c_void, byte: u32) -> i64;
        pub fn sq_cursor_goto_first_child_for_point(cursor: *mut c_void, point: RawPoint) -> i64;
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
        pub fn sq_node_first_slot(node: RawNode) -> u32;
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
