use crate::{
    Error, Language, Node, NodeId, RegionIx, RepresentationId, SlotIx, TreeCursor, TreeIx,
    native::GrammarView,
    side_data::{PointsData, PresenceCache, PresenceView},
    types::{GroupIx, SlabOffset, SlotSpan, SquatterGrammarId, SquatterKindId},
};
use smallvec::SmallVec;
use std::{
    fmt,
    marker::PhantomData,
    mem::MaybeUninit,
    ops::{Deref, Range},
    ptr, slice,
};

// implied by forest storage version 0
pub(crate) const GROUP_SIZE: u32 = 32;
pub(crate) const SPAN_BITS: u32 = 16;
pub(crate) const ALIGNMENT: usize = 8;

// type: bits 31–24; version: bits 23–16; flags: bits 15–0
pub(crate) const fn slab_format(slab_type: u8, version: u8) -> u32 {
    ((slab_type as u32) << 24) | ((version as u32) << 16)
}

// header: format, used groups, capacity, region count; descriptors follow columns
pub(crate) const FOREST_FORMAT: u32 = slab_format(0xff, 0);
pub(crate) const EXTRAS: u32 = 1 << 3;
pub(crate) const ERRORS: u32 = 1 << 2;
pub(crate) const MISSING: u32 = 1 << 1;
pub(crate) const SEPARATE_GRAMMAR: u32 = 1;
pub(crate) const BYTE_IDS: u32 = 1 << 4;
pub(crate) const OPTIONAL: u32 = EXTRAS | ERRORS | MISSING | SEPARATE_GRAMMAR;

// Reserve room for both remapped error IDs in every grammar sharing the slab.
pub(crate) fn id_width_flags<'language>(
    languages: impl IntoIterator<Item = &'language Language>,
) -> u32 {
    let mut flags = BYTE_IDS;
    for language in languages {
        if language.tables().kind_count > 254 || language.tables().compact_grammar_count > 254 {
            flags = 0;
        }
    }
    flags
}

/// Identifies the packed slab format for persistence compatibility.
///
/// **Not in Tree-sitter**
pub fn representation_id() -> RepresentationId {
    RepresentationId(FOREST_FORMAT as u64)
}

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Layout<Column> {
    pub symbol_width: u32,
    pub waste: Column,
    pub start_byte_base: Column,
    pub start_byte_delta: Column,
    pub end_byte_base: Column,
    pub end_byte_delta: Column,
    pub span_max: Column,
    pub span_delta: Column,
    pub symbol: Column,
    pub field: Column,
    pub supertype: Column,
    pub last: Column,
    pub grammar: Column,
    pub extra: Column,
    pub error: Column,
    pub missing: Column,
    pub end: SlabOffset,
}

#[inline]
pub(crate) fn aligned_bytes(count: u32, bytes: u32) -> u64 {
    (count as u64 * bytes as u64 + 7) & !7
}

#[inline]
pub(crate) fn bit_bytes(count: u32) -> u64 {
    (count as u64).div_ceil(64) * 8
}

impl Layout<SlabOffset> {
    // The first column follows the fixed header, independent of capacity and flags.
    const WASTE: SlabOffset = SlabOffset(((16 + ALIGNMENT - 1) & !(ALIGNMENT - 1)) as u32);

    pub fn new(capacity: u32, flags: u32) -> Result<Self, Error> {
        let symbol_width = if flags & BYTE_IDS != 0 { 1 } else { 2 };
        let slots = capacity.checked_mul(GROUP_SIZE).ok_or(Error::Overflow)?;
        let mut next = Self::WASTE.raw() as u64;
        let mut column = |length: u64| {
            let offset = SlabOffset(next as u32);
            next = (next + length + ALIGNMENT as u64 - 1) & !(ALIGNMENT as u64 - 1);
            offset
        };
        let mut result = Self {
            symbol_width,
            waste: column(aligned_bytes(capacity, 2)),
            start_byte_base: column(aligned_bytes(capacity, 4)),
            start_byte_delta: column(aligned_bytes(slots, 1)),
            end_byte_base: column(aligned_bytes(capacity, 4)),
            end_byte_delta: column(aligned_bytes(slots, 2)),
            span_max: column(aligned_bytes(capacity, 4)),
            span_delta: column(aligned_bytes(slots, SPAN_BITS / 8)),
            symbol: column(aligned_bytes(slots, symbol_width)),
            field: column(aligned_bytes(slots, 2)),
            supertype: column(aligned_bytes(slots, 2)),
            last: column(bit_bytes(slots)),
            grammar: column(if flags & SEPARATE_GRAMMAR != 0 {
                aligned_bytes(slots, symbol_width)
            } else {
                0
            }),
            extra: column(if flags & EXTRAS != 0 {
                bit_bytes(slots)
            } else {
                0
            }),
            error: column(if flags & ERRORS != 0 {
                bit_bytes(slots)
            } else {
                0
            }),
            missing: column(if flags & MISSING != 0 {
                bit_bytes(slots)
            } else {
                0
            }),
            end: SlabOffset(0),
        };
        result.end = SlabOffset(u32::try_from(next).map_err(|_| Error::Overflow)?);
        Ok(result)
    }

    fn resolve(self, bytes: ptr::NonNull<u8>) -> Layout<ColumnPointer> {
        Layout {
            symbol_width: self.symbol_width,
            waste: ColumnPointer(self.waste.pointer(bytes)),
            start_byte_base: ColumnPointer(self.start_byte_base.pointer(bytes)),
            start_byte_delta: ColumnPointer(self.start_byte_delta.pointer(bytes)),
            end_byte_base: ColumnPointer(self.end_byte_base.pointer(bytes)),
            end_byte_delta: ColumnPointer(self.end_byte_delta.pointer(bytes)),
            span_max: ColumnPointer(self.span_max.pointer(bytes)),
            span_delta: ColumnPointer(self.span_delta.pointer(bytes)),
            symbol: ColumnPointer(self.symbol.pointer(bytes)),
            field: ColumnPointer(self.field.pointer(bytes)),
            supertype: ColumnPointer(self.supertype.pointer(bytes)),
            last: ColumnPointer(self.last.pointer(bytes)),
            grammar: ColumnPointer(self.grammar.pointer(bytes)),
            extra: ColumnPointer(self.extra.pointer(bytes)),
            error: ColumnPointer(self.error.pointer(bytes)),
            missing: ColumnPointer(self.missing.pointer(bytes)),
            end: self.end,
        }
    }
}

impl<Column: Copy> Layout<Column> {
    fn columns(self, groups: u32, flags: u32) -> [(Column, usize); 15] {
        let slots = groups * GROUP_SIZE;
        [
            (self.waste, aligned_bytes(groups, 2) as usize),
            (self.start_byte_base, aligned_bytes(groups, 4) as usize),
            (self.start_byte_delta, aligned_bytes(slots, 1) as usize),
            (self.end_byte_base, aligned_bytes(groups, 4) as usize),
            (self.end_byte_delta, aligned_bytes(slots, 2) as usize),
            (self.span_max, aligned_bytes(groups, 4) as usize),
            (
                self.span_delta,
                aligned_bytes(slots, SPAN_BITS / 8) as usize,
            ),
            (
                self.symbol,
                aligned_bytes(slots, self.symbol_width) as usize,
            ),
            (self.field, aligned_bytes(slots, 2) as usize),
            (self.supertype, aligned_bytes(slots, 2) as usize),
            (self.last, bit_bytes(slots) as usize),
            (
                self.grammar,
                if flags & SEPARATE_GRAMMAR != 0 {
                    aligned_bytes(slots, self.symbol_width) as usize
                } else {
                    0
                },
            ),
            (
                self.extra,
                if flags & EXTRAS != 0 {
                    bit_bytes(slots) as usize
                } else {
                    0
                },
            ),
            (
                self.error,
                if flags & ERRORS != 0 {
                    bit_bytes(slots) as usize
                } else {
                    0
                },
            ),
            (
                self.missing,
                if flags & MISSING != 0 {
                    bit_bytes(slots) as usize
                } else {
                    0
                },
            ),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ColumnPointer(pub(crate) *mut u8);

// Column access borrows the owning forest; published slabs are immutable.
unsafe impl Send for ColumnPointer {}
unsafe impl Sync for ColumnPointer {}

impl ColumnPointer {
    #[inline]
    pub fn as_ptr(self) -> *mut u8 {
        self.0
    }

    #[cfg(test)]
    pub fn offset(self, bytes: ptr::NonNull<u8>) -> usize {
        self.0 as usize - bytes.as_ptr() as usize
    }
}

pub(crate) trait SlabAddress: Copy {
    fn pointer(self, bytes: ptr::NonNull<u8>) -> *mut u8;
}

impl SlabAddress for SlabOffset {
    #[inline]
    fn pointer(self, bytes: ptr::NonNull<u8>) -> *mut u8 {
        bytes.as_ptr().wrapping_add(self.ix())
    }
}

impl SlabAddress for ColumnPointer {
    #[inline]
    fn pointer(self, _bytes: ptr::NonNull<u8>) -> *mut u8 {
        self.0
    }
}

// reader addresses never dispatch through the owner
#[allow(dead_code)]
enum Storage {
    Owned(Vec<u64>),
    Retained(Box<dyn Send + Sync>),
}

pub(crate) struct Slab {
    storage: Storage,
    bytes: ptr::NonNull<u8>,
    length: usize,
}

// Readers borrow immutable published storage; writers require exclusive access.
unsafe impl Send for Slab {}
unsafe impl Sync for Slab {}

impl Slab {
    /// # Safety
    /// The initializer must write all `length` bytes before returning `Ok(())`.
    pub(crate) unsafe fn initialize(
        length: usize,
        initialize: impl FnOnce(*mut u8) -> Result<(), Error>,
    ) -> Result<Self, Error> {
        if !length.is_multiple_of(ALIGNMENT) {
            return Err(Error::InvalidArgument);
        }
        let mut words = Vec::<u64>::new();
        words
            .try_reserve_exact(length / 8)
            .map_err(|_| Error::Allocation)?;
        let bytes = ptr::NonNull::new(words.as_mut_ptr().cast()).unwrap();
        // Keep unfinished storage outside the length, including during unwinding.
        initialize(bytes.as_ptr())?;
        unsafe {
            words.set_len(length / 8);
        }
        Ok(Self {
            storage: Storage::Owned(words),
            bytes,
            length,
        })
    }

    #[cfg(test)]
    fn zeroed(length: usize) -> Result<Self, Error> {
        unsafe {
            Self::initialize(length, |destination| {
                ptr::write_bytes(destination, 0, length);
                Ok(())
            })
        }
    }

    pub(crate) fn copy(bytes: &[u8]) -> Result<Self, Error> {
        unsafe {
            Self::initialize(bytes.len(), |destination| {
                ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len());
                Ok(())
            })
        }
    }

    pub(crate) fn retained(owner: impl StableSlab) -> Result<Self, Error> {
        let owner = Box::new(owner);
        let bytes = owner.bytes();
        if !(bytes.as_ptr() as usize).is_multiple_of(ALIGNMENT)
            || !bytes.len().is_multiple_of(ALIGNMENT)
        {
            return Err(Error::InvalidArgument);
        }
        let pointer = ptr::NonNull::from(bytes).cast();
        let length = bytes.len();
        Ok(Self {
            storage: Storage::Retained(owner),
            bytes: pointer,
            length,
        })
    }

    fn borrowed(bytes: &[u8]) -> Result<Self, Error> {
        if !(bytes.as_ptr() as usize).is_multiple_of(ALIGNMENT)
            || !bytes.len().is_multiple_of(ALIGNMENT)
        {
            return Err(Error::InvalidArgument);
        }
        Ok(Self {
            storage: Storage::Retained(Box::new(())),
            bytes: ptr::NonNull::from(bytes).cast(),
            length: bytes.len(),
        })
    }

    #[inline]
    pub(crate) fn bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.bytes.as_ptr(), self.length) }
    }

    pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
        assert!(matches!(self.storage, Storage::Owned(_)));
        unsafe { slice::from_raw_parts_mut(self.bytes.as_ptr(), self.length) }
    }

    pub(crate) fn grow(&mut self, length: usize) -> Result<(), Error> {
        let Storage::Owned(words) = &mut self.storage else {
            return Err(Error::InvalidArgument);
        };
        if !length.is_multiple_of(8) || length < self.length {
            return Err(Error::InvalidArgument);
        }
        words
            .try_reserve(length / 8 - words.len())
            .map_err(|_| Error::Allocation)?;
        words.resize(length / 8, 0);
        self.bytes = ptr::NonNull::new(words.as_mut_ptr().cast()).unwrap();
        self.length = length;
        Ok(())
    }
}

const _: () = assert!(align_of::<u64>() >= ALIGNMENT);

pub(crate) struct ForestData {
    pub layout: Layout<ColumnPointer>,
    storage: Slab,
    pub trees: SmallVec<[TreeData; 1]>,
    pub regions: SmallVec<[RegionData; 1]>,
    pub presence_cache: Option<PresenceCache>,
    pub point_data: Option<PointsData>,
}

#[derive(Clone)]
pub(crate) struct TreeData {
    pub region: RegionIx,
    pub tables: ptr::NonNull<GrammarView>,
    // trees occupy whole groups; the final group may contain waste
    pub slots: Range<SlotIx>,
}

// The region language retains the immutable tables, including in forest copies.
unsafe impl Send for TreeData {}
unsafe impl Sync for TreeData {}

impl TreeData {
    #[inline]
    pub(crate) fn tables(&self) -> &GrammarView {
        unsafe { self.tables.as_ref() }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegionOrder {
    Unordered,
    ByStart,
    NonOverlapping,
}

pub(crate) struct RegionData {
    pub slots: Range<SlotIx>,
    pub trees: Range<TreeIx>,
    pub language: Language,
    pub order: RegionOrder,
    pub presence: Option<PresenceView>,
}

impl RegionData {
    pub(crate) fn group_count(&self) -> u32 {
        self.slots.end.group() - self.slots.start.group()
    }
}

/// Owns independent packed trees in physical input order. Core bytes and attached
/// side data have separate storage; nodes and tree views borrow this owner.
pub struct Forest {
    pub(crate) data: Box<ForestData>,
}

/// A borrowed root node. Nodes returned from this handle borrow its forest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct Tree<'forest>(pub(crate) Node<'forest>);

impl<'forest> Tree<'forest> {
    pub fn root_node(&self) -> Node<'forest> {
        self.0
    }
    pub fn language(&self) -> &'forest Language {
        self.0.language()
    }
    pub fn has_points(&self) -> bool {
        self.0.has_points()
    }
    pub fn walk(&self) -> TreeCursor<'forest> {
        self.0.walk()
    }
}

impl<'forest> Deref for Tree<'forest> {
    type Target = Node<'forest>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone, Copy)]
pub struct ForestRegion<'forest> {
    pub(crate) forest: &'forest ForestData,
    pub(crate) index: RegionIx,
}

impl<'forest> ForestRegion<'forest> {
    pub(crate) fn data(self) -> &'forest RegionData {
        &self.forest.regions[self.index.ix()]
    }
    pub fn index(&self) -> RegionIx {
        self.index
    }
    pub fn language(&self) -> &'forest Language {
        &self.data().language
    }
    pub(crate) fn group_count(&self) -> u32 {
        self.data().group_count()
    }
    pub fn trees(
        &self,
    ) -> impl DoubleEndedIterator<Item = Tree<'forest>> + ExactSizeIterator + use<'forest> {
        let range = self.data().trees.clone();
        let forest = self.forest;
        (range.start.raw()..range.end.raw()).map(move |index| forest.tree(TreeIx::from_raw(index)))
    }
}

impl ForestData {
    pub(crate) fn tree(&self, index: TreeIx) -> Tree<'_> {
        let tree = &self.trees[index.ix()];
        let root = self.group_end(tree.slots.end.group() - 1) - 1;
        Tree(Node::new(self, NodeId::new(index, root)))
    }
}

// Capture the slab address once: raw stores otherwise make LLVM reload it
// from the descriptor. The borrow excludes resizing while the writer is used.
pub(crate) struct SlabWriter<'tree> {
    bytes: ptr::NonNull<u8>,
    borrow: PhantomData<&'tree mut [u8]>,
}

impl SlabWriter<'_> {
    pub fn put_byte(&mut self, address: impl SlabAddress, index: u32, value: u8) {
        unsafe {
            *address.pointer(self.bytes).add(index as usize) = value;
        }
    }

    pub fn put_short(&mut self, address: impl SlabAddress, index: u32, value: u16) {
        unsafe {
            address
                .pointer(self.bytes)
                .add(index as usize * 2)
                .cast::<u16>()
                .write_unaligned(value.to_le());
        }
    }
}

impl ForestData {
    // Column pointers are resolved on allocation and refreshed after slab relocation.
    #[inline]
    pub fn byte(&self, address: impl SlabAddress, index: u32) -> u8 {
        unsafe { *address.pointer(self.storage.bytes).add(index as usize) }
    }

    #[inline]
    pub fn short(&self, address: impl SlabAddress, index: u32) -> u16 {
        u16::from_le(unsafe {
            address
                .pointer(self.storage.bytes)
                .add(index as usize * 2)
                .cast::<u16>()
                .read_unaligned()
        })
    }

    #[inline]
    pub fn word(&self, address: impl SlabAddress, index: u32) -> u32 {
        u32::from_le(unsafe {
            address
                .pointer(self.storage.bytes)
                .add(index as usize * 4)
                .cast::<u32>()
                .read_unaligned()
        })
    }

    #[inline]
    pub fn long(&self, address: impl SlabAddress, index: u32) -> u64 {
        u64::from_le(unsafe {
            address
                .pointer(self.storage.bytes)
                .add(index as usize * 8)
                .cast::<u64>()
                .read_unaligned()
        })
    }

    #[inline]
    pub fn bit(&self, address: impl SlabAddress, index: u32) -> bool {
        self.byte(address, index / 8) & (1 << (index % 8)) != 0
    }

    #[inline]
    pub fn flags(&self) -> u32 {
        self.word(SlabOffset(0), 0)
    }

    #[inline]
    pub fn groups(&self) -> u32 {
        self.word(SlabOffset(0), 1)
    }

    #[inline]
    pub fn capacity(&self) -> u32 {
        self.word(SlabOffset(0), 2)
    }

    #[inline]
    pub fn waste(&self, group: GroupIx) -> u32 {
        self.short(Layout::WASTE, group.raw()) as u32
    }

    #[inline]
    pub fn group_end(&self, group: GroupIx) -> SlotIx {
        (group + 1).first_slot() - self.waste(group)
    }

    #[inline]
    pub fn first_slot(&self, slot: SlotIx) -> SlotIx {
        slot - (self.span_max(slot.group()) - self.span_delta(slot))
    }

    #[inline]
    pub fn span_max(&self, group: GroupIx) -> SlotSpan {
        SlotSpan(self.word(self.layout.span_max, group.raw()))
    }

    #[inline]
    pub fn span_delta(&self, slot: SlotIx) -> u32 {
        if SPAN_BITS == 16 {
            self.short(self.layout.span_delta, slot.raw()) as u32
        } else {
            self.byte(self.layout.span_delta, slot.raw()) as u32
        }
    }

    #[inline]
    pub fn previous_slot(&self, slot: SlotIx) -> Option<SlotIx> {
        let previous = SlotIx(slot.raw().checked_sub(1)?);
        Some(if slot.is_group_start() {
            previous - self.waste(previous.group())
        } else {
            previous
        })
    }

    #[inline]
    pub fn symbol_index(&self, slot: SlotIx) -> SquatterKindId {
        SquatterKindId(self.symbol_id(self.layout.symbol, slot, self.layout.symbol_width))
    }

    #[inline]
    pub fn grammar_index(&self, slot: SlotIx) -> SquatterGrammarId {
        let column = if self.flags() & SEPARATE_GRAMMAR != 0 {
            self.layout.grammar
        } else {
            self.layout.symbol
        };
        SquatterGrammarId(self.symbol_id(column, slot, self.layout.symbol_width))
    }

    #[inline]
    fn symbol_id(&self, column: ColumnPointer, slot: SlotIx, width: u32) -> u16 {
        if width == 1 {
            u16::from(self.byte(column, slot.raw()))
        } else {
            self.short(column, slot.raw())
        }
    }

    pub fn has_points(&self) -> bool {
        self.point_data.is_some()
    }

    pub fn slice(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.storage.bytes.as_ptr(), self.storage.length) }
    }

    #[inline]
    pub(crate) fn column_slice(&self, column: ColumnPointer, start: usize, length: usize) -> &[u8] {
        // Resolved column pointers and group offsets remain within the retained slab.
        unsafe { slice::from_raw_parts(column.as_ptr().add(start), length) }
    }

    pub(crate) fn writer(&mut self) -> SlabWriter<'_> {
        SlabWriter {
            bytes: self.storage.bytes,
            borrow: PhantomData,
        }
    }

    #[cfg(test)]
    pub(crate) fn put_byte(&mut self, address: impl SlabAddress, index: u32, value: u8) {
        self.writer().put_byte(address, index, value);
    }

    pub(crate) fn put_short(&mut self, address: impl SlabAddress, index: u32, value: u16) {
        self.writer().put_short(address, index, value);
    }

    pub(crate) fn put_word(&mut self, address: impl SlabAddress, index: u32, value: u32) {
        unsafe {
            address
                .pointer(self.storage.bytes)
                .add(index as usize * 4)
                .cast::<u32>()
                .write_unaligned(value.to_le());
        }
    }

    pub(crate) fn put_long(&mut self, address: impl SlabAddress, index: u32, value: u64) {
        unsafe {
            address
                .pointer(self.storage.bytes)
                .add(index as usize * 8)
                .cast::<u64>()
                .write_unaligned(value.to_le());
        }
    }

    #[cfg(test)]
    pub(crate) fn put_bit(&mut self, address: impl SlabAddress, index: u32, value: bool) {
        let mask = 1 << (index % 8);
        self.put_byte(
            address,
            index / 8,
            (self.byte(address, index / 8) & !mask) | if value { mask } else { 0 },
        );
    }
}

impl Forest {
    #[inline]
    pub(crate) fn data(&self) -> &ForestData {
        &self.data
    }
    pub(crate) fn data_mut(&mut self) -> &mut ForestData {
        &mut self.data
    }

    pub fn tree(&self, index: TreeIx) -> Option<Tree<'_>> {
        (index.ix() < self.data.trees.len()).then(|| self.data.tree(index))
    }

    pub fn trees(&self) -> impl DoubleEndedIterator<Item = Tree<'_>> + ExactSizeIterator {
        (0..self.data.trees.len() as u32).map(|index| self.data.tree(TreeIx::from_raw(index)))
    }

    pub fn regions(&self) -> impl DoubleEndedIterator<Item = ForestRegion<'_>> + ExactSizeIterator {
        (0..self.data.regions.len() as u32).map(|index| ForestRegion {
            forest: &self.data,
            index: RegionIx(index),
        })
    }

    /// Returns the root of a single-tree forest. Panics for empty or multi-tree forests.
    pub fn root_node(&self) -> Node<'_> {
        assert_eq!(self.data.trees.len(), 1, "expected a single-tree forest");
        self.data.tree(TreeIx::from_raw(0)).root_node()
    }

    /// Returns the language of a single-tree forest.
    pub fn language(&self) -> &Language {
        self.root_node().language()
    }
    pub fn walk(&self) -> TreeCursor<'_> {
        self.root_node().walk()
    }
    #[cfg(test)]
    pub(crate) fn node_at_slot(&self, slot: SlotIx) -> Option<Node<'_>> {
        self.root_node().node_at_slot(slot)
    }
    pub fn language_cache(&self) -> Result<Vec<u8>, Error> {
        self.language().cache()
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.data.slice()
    }
    pub(crate) fn group_count(&self) -> u32 {
        self.data.groups()
    }
    pub(crate) fn group_capacity(&self) -> u32 {
        self.data.capacity()
    }
    pub fn has_points(&self) -> bool {
        self.data.has_points()
    }

    fn allocate(layout: Layout<SlabOffset>, storage: Slab) -> Self {
        let columns = layout.resolve(storage.bytes);
        Self {
            data: Box::new(ForestData {
                layout: columns,
                storage,
                trees: SmallVec::new(),
                regions: SmallVec::new(),
                presence_cache: None,
                point_data: None,
            }),
        }
    }

    pub(crate) fn empty(languages: &[Language], capacity: u32) -> Result<Self, Error> {
        let count = u32::try_from(languages.len()).map_err(|_| Error::Overflow)?;
        let flags = FOREST_FORMAT | OPTIONAL | id_width_flags(languages);
        let layout = Layout::new(capacity, flags)?;
        let length = slab_length(layout, count)?;
        let storage = unsafe {
            Slab::initialize(length, |destination| {
                for (index, word) in [flags, 0, capacity, count].into_iter().enumerate() {
                    destination
                        .add(index * 4)
                        .cast::<u32>()
                        .write_unaligned(word.to_le());
                }
                ptr::write_bytes(destination.add(16), 0, layout.end.ix() - 16);
                for index in 0..count {
                    let descriptor = destination.add(layout.end.ix() + index as usize * 8);
                    descriptor.cast::<u32>().write_unaligned(index.to_le());
                    descriptor.add(4).cast::<u32>().write_unaligned(0);
                }
                Ok(())
            })?
        };
        let mut forest = Self::allocate(layout, storage);
        forest
            .data
            .regions
            .try_reserve(languages.len())
            .map_err(|_| Error::Allocation)?;
        for language in languages {
            forest.data.regions.push(RegionData {
                slots: SlotIx(0)..SlotIx(0),
                trees: TreeIx(0)..TreeIx(0),
                language: language.clone(),
                order: RegionOrder::NonOverlapping,
                presence: None,
            });
        }
        Ok(forest)
    }

    /// Copies core bytes and checks the invariants needed for memory-safe access.
    /// Supply grammar bindings in the same order used to pack regions; repeated
    /// grammars may share a binding. Side data loads separately.
    ///
    /// Checks are identical in every build profile. Use [`Self::validate`] for
    /// full content validation, including any subsequently attached side data.
    pub fn from_bytes(languages: &[Language], bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 16 || bytes.len() > u32::MAX as usize {
            return Err(Error::InvalidSlab);
        }
        Self::load::<false>(languages, Slab::copy(bytes)?)
    }

    /// Equivalent to [`Self::from_bytes`].
    pub fn from_bytes_safety_checked(languages: &[Language], bytes: &[u8]) -> Result<Self, Error> {
        Self::from_bytes(languages, bytes)
    }

    /// Copies trusted core bytes without scanning node contents.
    /// Header/layout checks and tree metadata reconstruction still run.
    ///
    /// # Safety
    /// The bytes must satisfy the memory-safety invariants checked by
    /// [`Self::from_bytes`] for the supplied grammar bindings: valid group bounds,
    /// nested spans and live sibling destinations, in-range table indexes, and
    /// byte coordinates that do not overflow. Unmodified bytes
    /// from packing or a successful safe load with those bindings satisfy them.
    pub unsafe fn from_bytes_unchecked(
        languages: &[Language],
        bytes: &[u8],
    ) -> Result<Self, Error> {
        if bytes.len() < 16 || bytes.len() > u32::MAX as usize {
            return Err(Error::InvalidSlab);
        }
        Self::load::<true>(languages, Slab::copy(bytes)?)
    }

    /// Retains immutable aligned storage with the checks of [`Self::from_bytes`].
    /// Byte access never calls the owner again.
    pub fn from_retained(languages: &[Language], owner: impl StableSlab) -> Result<Self, Error> {
        Self::load::<false>(languages, Slab::retained(owner)?)
    }

    /// Retains trusted immutable aligned storage without scanning node contents.
    ///
    /// # Safety
    /// The owner's bytes must meet the requirements of [`Self::from_bytes_unchecked`].
    pub unsafe fn from_retained_unchecked(
        languages: &[Language],
        owner: impl StableSlab,
    ) -> Result<Self, Error> {
        Self::load::<true>(languages, Slab::retained(owner)?)
    }

    /// Borrows aligned core bytes with the checks of [`Self::from_bytes`].
    pub fn from_bytes_borrowed<'bytes>(
        languages: &[Language],
        bytes: &'bytes [u8],
    ) -> Result<BorrowedForest<'bytes>, Error> {
        Ok(BorrowedForest {
            forest: Self::load::<false>(languages, Slab::borrowed(bytes)?)?,
            bytes: PhantomData,
        })
    }

    /// Borrows trusted aligned core bytes without scanning node contents.
    ///
    /// # Safety
    /// The bytes must meet the requirements of [`Self::from_bytes_unchecked`].
    pub unsafe fn from_bytes_borrowed_unchecked<'bytes>(
        languages: &[Language],
        bytes: &'bytes [u8],
    ) -> Result<BorrowedForest<'bytes>, Error> {
        Ok(BorrowedForest {
            forest: Self::load::<true>(languages, Slab::borrowed(bytes)?)?,
            bytes: PhantomData,
        })
    }

    pub fn compact_size(&self) -> usize {
        slab_length(
            Layout::new(self.group_count(), self.data.flags()).unwrap(),
            self.data.regions.len() as u32,
        )
        .unwrap()
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let length = self.compact_size();
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| Error::Allocation)?;
        self.copy_compact_into(&mut bytes.spare_capacity_mut()[..length])?;
        // copy_compact_into initialized every byte in the reserved prefix.
        unsafe {
            bytes.set_len(length);
        }
        Ok(bytes)
    }

    pub fn copy_compact_into<'bytes>(
        &self,
        destination: &'bytes mut [MaybeUninit<u8>],
    ) -> Result<&'bytes mut [u8], Error> {
        if destination.len() != self.compact_size() {
            return Err(Error::InvalidArgument);
        }
        let layout = Layout::new(self.group_count(), self.data.flags())?;
        unsafe {
            self.copy_columns(
                destination.as_mut_ptr().cast(),
                layout,
                self.data.flags(),
                self.group_count(),
            );
        }
        Ok(
            unsafe {
                slice::from_raw_parts_mut(destination.as_mut_ptr().cast(), destination.len())
            },
        )
    }

    unsafe fn copy_columns(
        &self,
        destination: *mut u8,
        next: Layout<SlabOffset>,
        flags: u32,
        capacity: u32,
    ) {
        let data = &self.data;
        unsafe {
            ptr::copy_nonoverlapping(data.storage.bytes.as_ptr(), destination, 16);
            destination.cast::<u32>().write_unaligned(flags.to_le());
            destination
                .add(8)
                .cast::<u32>()
                .write_unaligned(capacity.to_le());
            let mut initialized = 16;
            for ((source, _), (target, length)) in data
                .layout
                .columns(data.groups(), flags)
                .into_iter()
                .zip(next.columns(data.groups(), flags))
            {
                let start = target.ix();
                ptr::write_bytes(destination.add(initialized), 0, start - initialized);
                ptr::copy_nonoverlapping(source.as_ptr(), destination.add(start), length);
                initialized = start + length;
            }
            ptr::write_bytes(destination.add(initialized), 0, next.end.ix() - initialized);
            // Only regions are serialized. Tree boundaries follow from root spans.
            for (index, region) in data.regions.iter().enumerate() {
                let descriptor = destination.add(next.end.ix() + index * 8);
                let grammar_index = data.word(data.layout.end + index as u32 * 8, 0);
                descriptor
                    .cast::<u32>()
                    .write_unaligned(grammar_index.to_le());
                descriptor
                    .add(4)
                    .cast::<u32>()
                    .write_unaligned(region.slots.end.raw().to_le());
            }
        }
    }

    pub(crate) fn resize(&mut self, capacity: u32, flags: u32) -> Result<(), Error> {
        let layout = Layout::new(capacity, flags)?;
        let storage = unsafe {
            Slab::initialize(
                slab_length(layout, self.data.regions.len() as u32)?,
                |destination| {
                    self.copy_columns(destination, layout, flags, capacity);
                    Ok(())
                },
            )?
        };
        self.data.layout = layout.resolve(storage.bytes);
        self.data.storage = storage;
        Ok(())
    }

    pub(crate) fn finish_layout(
        &mut self,
        capacity: u32,
        optional_columns: u32,
    ) -> Result<(), Error> {
        let flags = (self.data.flags() & !OPTIONAL) | optional_columns;
        assert!(capacity >= self.group_count() && capacity <= self.group_capacity());
        assert_eq!(optional_columns & !OPTIONAL, 0);
        let next = Layout::new(capacity, flags)?;
        let length = slab_length(next, self.data.regions.len() as u32)?;
        let data = &mut self.data;
        if !matches!(data.storage.storage, Storage::Owned(_)) {
            return self.resize(capacity, flags);
        }
        let old = data.layout;
        // All destinations move earlier. Copy columns before the descriptor tail.
        unsafe {
            for ((source, _), (destination, length)) in old
                .columns(data.groups(), flags)
                .into_iter()
                .zip(next.columns(data.groups(), flags))
            {
                ptr::copy(
                    source.as_ptr(),
                    destination.pointer(data.storage.bytes),
                    length,
                );
            }
            ptr::copy(
                old.end.pointer(data.storage.bytes),
                next.end.pointer(data.storage.bytes),
                data.regions.len() * 8,
            );
        }
        data.layout = next.resolve(data.storage.bytes);
        data.put_word(SlabOffset(0), 0, flags);
        data.put_word(SlabOffset(0), 2, capacity);
        data.storage.length = length;
        if let Storage::Owned(words) = &mut data.storage.storage {
            words.truncate(length / 8);
        }
        for index in 0..data.regions.len() {
            let end = data.regions[index].slots.end.raw();
            data.put_word(next.end + index as u32 * 8, 1, end);
        }
        self.shrink_allocation(next, 256)
    }

    fn shrink_allocation(
        &mut self,
        layout: Layout<SlabOffset>,
        threshold: usize,
    ) -> Result<(), Error> {
        let storage = &mut self.data.storage;
        let Storage::Owned(words) = &mut storage.storage else {
            return Err(Error::InvalidArgument);
        };
        let allocated = words.capacity() * 8;
        let excess = allocated - storage.length;
        if excess != 0 && excess >= threshold.min(allocated / 2) {
            words.shrink_to_fit();
            storage.bytes = ptr::NonNull::new(words.as_mut_ptr().cast()).unwrap();
            self.data.layout = layout.resolve(storage.bytes);
        }
        Ok(())
    }

    pub fn compact(&mut self) -> Result<(), Error> {
        self.finish_layout(self.group_count(), self.data.flags() & OPTIONAL)
    }

    /// Copies compact core columns and attached side data into independent storage.
    pub fn to_compacted(&self) -> Result<Self, Error> {
        let layout = Layout::new(self.group_count(), self.data.flags())?;
        let storage = unsafe {
            Slab::initialize(self.compact_size(), |destination| {
                self.copy_columns(destination, layout, self.data.flags(), self.group_count());
                Ok(())
            })?
        };
        self.copy_with_storage(layout, storage)
    }

    /// Copies core and attached side data without changing group capacity or IDs.
    pub fn detach(&self) -> Result<Self, Error> {
        let layout = Layout::new(self.group_capacity(), self.data.flags())?;
        self.copy_with_storage(layout, Slab::copy(self.as_bytes())?)
    }

    fn copy_with_storage(&self, layout: Layout<SlabOffset>, storage: Slab) -> Result<Self, Error> {
        let mut forest = Self::allocate(layout, storage);
        forest
            .data
            .trees
            .try_reserve(self.data.trees.len())
            .map_err(|_| Error::Allocation)?;
        forest.data.trees.extend(self.data.trees.iter().cloned());
        forest
            .data
            .regions
            .try_reserve(self.data.regions.len())
            .map_err(|_| Error::Allocation)?;
        forest
            .data
            .regions
            .extend(self.data.regions.iter().map(|region| RegionData {
                slots: region.slots.clone(),
                trees: region.trees.clone(),
                language: region.language.clone(),
                order: region.order,
                presence: None,
            }));
        if let Some(cache) = &self.data.presence_cache {
            forest.set_presence_cache_trusted(cache.copy()?);
        }
        if let Some(points) = &self.data.point_data {
            forest.set_point_data_trusted(points.copy()?);
        }
        Ok(forest)
    }

    fn load<const TRUSTED: bool>(languages: &[Language], storage: Slab) -> Result<Self, Error> {
        let bytes = storage.bytes();
        if bytes.len() < 16 || bytes.len() > u32::MAX as usize {
            return Err(Error::InvalidSlab);
        }
        let header =
            |index: usize| u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap());
        let flags = header(0);
        let groups = header(1);
        let capacity = header(2);
        let region_count = header(3);
        if flags & !(OPTIONAL | BYTE_IDS) != FOREST_FORMAT
            || groups > capacity
            || (groups == 0) != (region_count == 0)
            || (flags & MISSING != 0 && flags & ERRORS == 0)
        {
            return Err(Error::InvalidSlab);
        }
        let layout = Layout::new(capacity, flags).map_err(|_| Error::InvalidSlab)?;
        if slab_length(layout, region_count).map_err(|_| Error::InvalidSlab)? != bytes.len() {
            return Err(Error::InvalidSlab);
        }
        let mut forest = Self::allocate(layout, storage);
        forest
            .data
            .regions
            .try_reserve(region_count as usize)
            .map_err(|_| Error::Allocation)?;
        let mut start = 0;
        for index in 0..region_count {
            let offset = layout.end + index * 8;
            let grammar_index = forest.data.word(offset, 0);
            let end = forest.data.word(offset, 1);
            let language = languages
                .get(grammar_index as usize)
                .ok_or(Error::InvalidSlab)?;
            if end <= start
                || end > groups * GROUP_SIZE
                || !SlotIx(end).is_group_start()
                || (flags & BYTE_IDS != 0 && id_width_flags([language]) == 0)
            {
                return Err(Error::InvalidSlab);
            }
            forest.data.regions.push(RegionData {
                slots: SlotIx(start)..SlotIx(end),
                trees: TreeIx(0)..TreeIx(0),
                language: language.clone(),
                order: RegionOrder::NonOverlapping,
                presence: None,
            });
            start = end;
        }
        if start != groups * GROUP_SIZE {
            return Err(Error::InvalidSlab);
        }
        forest.reconstruct_trees()?;
        if !TRUSTED {
            forest.validate_nodes::<false>()?;
        }
        forest.classify_regions();
        Ok(forest)
    }

    fn reconstruct_trees(&mut self) -> Result<(), Error> {
        for region_index in 0..self.data.regions.len() {
            let slots = self.data.regions[region_index].slots.clone();
            let tables = ptr::NonNull::from(self.data.regions[region_index].language.tables());
            let first_tree = self.data.trees.len();
            let mut end = slots.end.raw();
            while end > slots.start.raw() {
                let group = SlotIx(end.checked_sub(1).ok_or(Error::InvalidSlab)?).group();
                let waste = self.data.waste(group);
                if waste >= GROUP_SIZE {
                    return Err(Error::InvalidSlab);
                }
                let root = end.checked_sub(waste + 1).ok_or(Error::InvalidSlab)?;
                if root < slots.start.raw() || root >= self.data.groups() * GROUP_SIZE {
                    return Err(Error::InvalidSlab);
                }
                let span = self
                    .data
                    .span_max(group)
                    .checked_sub(self.data.span_delta(SlotIx(root)))
                    .ok_or(Error::InvalidSlab)?;
                let start = SlotIx(root).checked_sub(span).ok_or(Error::InvalidSlab)?;
                if start < slots.start || start.raw() >= end || !start.is_group_start() {
                    return Err(Error::InvalidSlab);
                }
                if self.data.trees.len() >= u32::MAX as usize {
                    return Err(Error::InvalidSlab);
                }
                self.data
                    .trees
                    .try_reserve(1)
                    .map_err(|_| Error::Allocation)?;
                self.data.trees.push(TreeData {
                    region: RegionIx(region_index as u32),
                    tables,
                    slots: start..SlotIx(end),
                });
                end = start.raw();
            }
            self.data.trees[first_tree..].reverse();
            self.data.regions[region_index].trees =
                TreeIx(first_tree as u32)..TreeIx(self.data.trees.len() as u32);
        }
        Ok(())
    }

    pub(crate) fn classify_regions(&mut self) {
        for index in 0..self.data.regions.len() {
            let mut order = RegionOrder::NonOverlapping;
            let mut previous: Option<Range<usize>> = None;
            let trees = self.data.regions[index].trees.clone();
            for tree in trees.start.raw()..trees.end.raw() {
                let current = self.data.tree(TreeIx(tree)).byte_range();
                if let Some(previous) = previous {
                    if previous.start > current.start {
                        order = RegionOrder::Unordered;
                    } else if previous.end > current.start && order == RegionOrder::NonOverlapping {
                        order = RegionOrder::ByStart;
                    }
                }
                previous = Some(current);
            }
            self.data.regions[index].order = order;
        }
    }
}

fn slab_length(layout: Layout<SlabOffset>, regions: u32) -> Result<usize, Error> {
    let length = u64::from(layout.end.raw()) + u64::from(regions) * 8;
    Ok(u32::try_from(length).map_err(|_| Error::Overflow)? as usize)
}

impl fmt::Debug for Forest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Forest")
            .field("trees", &self.data.trees.len())
            .field("regions", &self.data.regions.len())
            .field("bytes", &self.as_bytes().len())
            .finish()
    }
}

pub struct BorrowedForest<'bytes> {
    forest: Forest,
    bytes: PhantomData<&'bytes [u8]>,
}
impl Deref for BorrowedForest<'_> {
    type Target = Forest;
    fn deref(&self) -> &Self::Target {
        &self.forest
    }
}

/// Immutable storage whose address and length remain stable until drop.
///
/// # Safety
/// Bytes must stay alive and immutable across owner moves and concurrent access.
/// No other party may resize, mutate, or unmap the storage.
pub unsafe trait StableSlab: Send + Sync + 'static {
    fn bytes(&self) -> &[u8];
}

impl Forest {
    /// Checks core topology, IDs, fields, supertype encodings and byte ranges,
    /// then validates each attached presence and point cache against the forest.
    /// Does not reparse or compare coordinates with source text.
    ///
    /// This content check is explicit in every build profile. Safe loading only
    /// checks the invariants needed for memory-safe access.
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_nodes::<true>()?;
        if let Some(cache) = self.presence_cache() {
            cache.validate_for(self)?;
        }
        if let Some(points) = self.point_data() {
            points.validate_for(self)?;
        }
        Ok(())
    }

    fn validate_nodes<const FULL: bool>(&self) -> Result<(), Error> {
        let data = self.data();
        let mut ends = SmallVec::<[u32; 64]>::new();
        for region in &data.regions {
            if !region.slots.start.is_group_start() || !region.slots.end.is_group_start() {
                return Err(Error::InvalidSlab);
            }
        }
        for tree in self.trees() {
            let slots = tree.0.tree_data().slots.clone();
            if !slots.start.is_group_start() || !slots.end.is_group_start() {
                return Err(Error::InvalidSlab);
            }
            let tables = tree.language().tables();
            let symbols = tables.kind_count + 2;
            let groups = (slots.start.group().raw()..slots.end.group().raw()).map(GroupIx);
            for group in groups.clone() {
                if data.waste(group) >= GROUP_SIZE {
                    return Err(Error::InvalidSlab);
                }
            }
            let root = data.group_end(slots.end.group() - 1) - 1;
            ends.clear();
            for group in groups.rev() {
                let span_max = data.span_max(group);
                let start_base = data.word(data.layout.start_byte_base, group.raw()) as u64;
                let end_base = data.word(data.layout.end_byte_base, group.raw());
                for slot in (group.first_slot().raw()..data.group_end(group).raw()).rev() {
                    while ends.last().is_some_and(|end| *end > slot) {
                        ends.pop();
                    }
                    let span = span_max
                        .checked_sub(data.span_delta(SlotIx(slot)))
                        .ok_or(Error::InvalidSlab)?;
                    if span > SlotIx(slot) - slots.start {
                        return Err(Error::InvalidSlab);
                    }
                    let end = (SlotIx(slot) - span).raw();
                    if end != slots.start.raw()
                        && end > data.group_end(SlotIx(end - 1).group()).raw()
                    {
                        return Err(Error::InvalidSlab);
                    }
                    let last = data.bit(data.layout.last, slot);
                    // Sibling access subtracts one without a bounds check.
                    if end == slots.start.raw() && !last {
                        return Err(Error::InvalidSlab);
                    }
                    // Reverse postorder follows child spans to the parent's boundary.
                    if slot == root.raw() {
                        if end != slots.start.raw() {
                            return Err(Error::InvalidSlab);
                        }
                    } else if !ends
                        .last()
                        .is_some_and(|parent| end >= *parent && (!FULL || last == (end == *parent)))
                    {
                        return Err(Error::InvalidSlab);
                    }
                    let symbol = u32::from(data.symbol_index(SlotIx(slot)).raw());
                    if symbol >= symbols || (FULL && symbol == 0) {
                        return Err(Error::InvalidSlab);
                    }
                    let grammar = u32::from(data.grammar_index(SlotIx(slot)).raw());
                    if grammar >= tables.compact_grammar_count + 2 || (FULL && grammar == 0) {
                        return Err(Error::InvalidSlab);
                    }
                    let supertype = data.short(data.layout.supertype, slot) as u32;
                    if supertype
                        >= if tables.supertype_count > 8 {
                            tables.dictionary_count
                        } else if FULL {
                            1 << tables.supertype_count
                        } else {
                            1 << 16
                        }
                    {
                        return Err(Error::InvalidSlab);
                    }
                    let start = start_base + data.byte(data.layout.start_byte_delta, slot) as u64;
                    let end_delta = data.short(data.layout.end_byte_delta, slot) as u32;
                    if start > u32::MAX as u64 || end_delta > end_base {
                        return Err(Error::InvalidSlab);
                    }
                    if FULL {
                        let field = data.short(data.layout.field, slot) as u32;
                        if start > (end_base - end_delta) as u64
                            || field > tables.field_count
                            || (slot == root.raw() && (field != 0 || supertype != 0))
                        {
                            return Err(Error::InvalidSlab);
                        }
                    }
                    ends.try_reserve(1).map_err(|_| Error::Allocation)?;
                    ends.push(end);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SlabOwner(Slab);

    unsafe impl StableSlab for SlabOwner {
        fn bytes(&self) -> &[u8] {
            self.0.bytes()
        }
    }

    fn assert_loaders_reject(languages: &[Language], bytes: &[u8]) {
        assert!(Forest::from_bytes(languages, bytes).is_err());
        assert!(Forest::from_bytes_safety_checked(languages, bytes).is_err());
        assert!(Forest::from_bytes_borrowed(languages, bytes).is_err());
        assert!(Forest::from_retained(languages, SlabOwner(Slab::copy(bytes).unwrap())).is_err());
    }

    #[test]
    fn safe_loaders_reject_invalid_addresses_and_table_indexes() {
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let grammar = Language::new(&language).unwrap();
        let languages = slice::from_ref(&grammar);
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let source = format!("[{}0]", "0,".repeat(64));
        let original = Forest::parse(&grammar, &mut parser, &source).unwrap();
        assert!(original.group_count() > 1);

        let mutations: &[fn(&mut ForestData)] = &[
            |data| data.put_byte(data.layout.symbol, 0, u8::MAX),
            |data| data.put_short(data.layout.waste, 0, GROUP_SIZE as u16),
            |data| data.put_short(data.layout.span_delta, 0, u16::MAX),
            |data| data.put_word(data.layout.span_max, 0, u32::MAX),
            |data| data.put_bit(data.layout.last, 0, false),
            |data| {
                data.put_word(data.layout.start_byte_base, 0, u32::MAX);
                data.put_byte(data.layout.start_byte_delta, 0, 1);
            },
            |data| {
                data.put_word(data.layout.end_byte_base, 0, 0);
                data.put_short(data.layout.end_byte_delta, 0, 1);
            },
        ];
        for mutation in mutations {
            let mut forest = original.detach().unwrap();
            mutation(forest.data_mut());
            assert_loaders_reject(languages, forest.as_bytes());
        }
    }

    #[test]
    fn safe_loaders_reject_crossing_subtrees_and_waste_boundaries() {
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let grammar = Language::new(&language).unwrap();
        let languages = slice::from_ref(&grammar);
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let mut forest = Forest::parse(&grammar, &mut parser, "[[0],[1]]").unwrap();
        let array = forest
            .root_node()
            .named_child(crate::NamedChildIx::new(0))
            .unwrap();
        let left = array.named_child(crate::NamedChildIx::new(0)).unwrap();
        let child = left.named_child(crate::NamedChildIx::new(0)).unwrap();
        let slot = child.slot();
        let boundary = left.first_slot() - 1;
        let maximum = forest.data().span_max(slot.group());
        let delta = (maximum - (slot - boundary)).raw() as u16;
        let data = forest.data_mut();
        data.put_short(data.layout.span_delta, slot.raw(), delta);
        assert_loaders_reject(languages, forest.as_bytes());

        let source = format!("[\"{}\",0]", "x".repeat(400));
        let mut forest = Forest::parse(&grammar, &mut parser, &source).unwrap();
        let boundary = (1..forest.group_count())
            .map(GroupIx)
            .find(|group| forest.data().waste(*group - 1) != 0)
            .unwrap()
            .first_slot();
        let maximum = forest.data().span_max(boundary.group()).raw();
        // A leaf at a group boundary must include the preceding padding in its span.
        let data = forest.data_mut();
        data.put_short(data.layout.span_delta, boundary.raw(), maximum as u16);
        assert_loaders_reject(languages, forest.as_bytes());
    }

    #[test]
    fn full_core_validation_is_explicit() {
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let grammar = Language::new(&language).unwrap();
        let languages = slice::from_ref(&grammar);
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let original = Forest::parse(&grammar, &mut parser, "[0,1]").unwrap();
        original.validate().unwrap();

        let mutations: &[fn(&mut ForestData)] = &[
            |data| data.put_byte(data.layout.symbol, 0, 0),
            |data| data.put_short(data.layout.field, 0, u16::MAX),
            |data| data.put_short(data.layout.supertype, 0, u16::MAX),
            |data| data.put_byte(data.layout.start_byte_delta, 0, u8::MAX),
            |data| data.put_bit(data.layout.last, 1, true),
        ];
        for mutation in mutations {
            let mut forest = original.detach().unwrap();
            mutation(forest.data_mut());
            let copied = Forest::from_bytes(languages, forest.as_bytes()).unwrap();
            let safety = Forest::from_bytes_safety_checked(languages, forest.as_bytes()).unwrap();
            let borrowed = Forest::from_bytes_borrowed(languages, forest.as_bytes()).unwrap();
            let retained =
                Forest::from_retained(languages, SlabOwner(Slab::copy(forest.as_bytes()).unwrap()))
                    .unwrap();
            for loaded in [&copied, &safety, &borrowed, &retained] {
                assert_eq!(loaded.validate(), Err(Error::InvalidSlab));
                for node in loaded.root_node().preorder().nodes() {
                    let _ = node.attributes();
                }
            }
        }
    }

    #[test]
    fn column_copies_initialize_gaps_and_unused_capacity() {
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let language = Language::new(&language).unwrap();
        for optional in 0..=OPTIONAL {
            for width in [0, BYTE_IDS] {
                let flags = FOREST_FORMAT | optional | width;
                let layout = Layout::new(5, flags).unwrap();
                let mut tree = Forest::empty(slice::from_ref(&language), 5).unwrap();
                unsafe {
                    tree.resize(5, flags).unwrap();
                    ptr::write_bytes(tree.data().storage.bytes.as_ptr(), 0x5a, layout.end.ix());
                }
                tree.data_mut().put_word(SlabOffset(0), 0, flags);
                tree.data_mut().put_word(SlabOffset(0), 1, 3);
                tree.data_mut().put_word(SlabOffset(0), 2, 5);
                for capacity in [3, 9] {
                    let next = Layout::new(capacity, flags).unwrap();
                    let mut destination = vec![0xff; slab_length(next, 1).unwrap()];
                    unsafe {
                        tree.copy_columns(destination.as_mut_ptr(), next, flags, capacity);
                    }
                    assert_eq!(&destination[..8], &tree.as_bytes()[..8]);
                    assert_eq!(&destination[12..16], &tree.as_bytes()[12..16]);
                    let mut copied = vec![false; destination.len()];
                    for (offset, length) in next.columns(3, flags) {
                        let start = offset.ix();
                        copied[start..start + length].fill(true);
                    }
                    for index in 16..next.end.ix() {
                        assert_eq!(destination[index], if copied[index] { 0x5a } else { 0 });
                    }
                    tree.resize(capacity, flags).unwrap();
                    destination[8..12].copy_from_slice(&capacity.to_le_bytes());
                    assert_eq!(tree.as_bytes(), destination);
                }
            }
        }
    }

    #[test]
    fn shrinking_respects_absolute_and_relative_thresholds() {
        let layout = Layout::new(1, FOREST_FORMAT | BYTE_IDS).unwrap();
        let length = layout.end.raw();
        for (excess, threshold, shrink) in [
            (0, 0, false),
            (8, 0, true),
            (248, 256, false),
            (256, 256, true),
            (length - 8, u32::MAX, false),
            (length, u32::MAX, true),
        ] {
            let mut tree =
                Forest::allocate(layout, Slab::zeroed((length + excess) as usize).unwrap());
            tree.data_mut().storage.length = length as usize;
            if let Storage::Owned(words) = &mut tree.data_mut().storage.storage {
                words.truncate(length as usize / 8);
            }
            tree.shrink_allocation(layout, threshold as usize).unwrap();
            assert_eq!(
                match &tree.data().storage.storage {
                    Storage::Owned(words) => words.capacity() as u32 * 8,
                    _ => unreachable!(),
                },
                if shrink { length } else { length + excess }
            );
            assert_eq!(tree.data().layout.end, layout.end);
        }
    }
}
