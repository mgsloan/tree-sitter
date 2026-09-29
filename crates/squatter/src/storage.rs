use crate::{
    Error, KindId, Language,
    native::GrammarView,
    side_data::{PointsData, PresenceCache, SideDataError},
    types::{SlabOffset, SquatterGrammarId, SquatterKindId},
};
use std::{
    alloc::{Layout as Allocation, alloc, alloc_zeroed, dealloc, handle_alloc_error, realloc},
    marker::PhantomData,
    mem::MaybeUninit,
    ops::Deref,
    ptr::{self, NonNull},
};

// implied by tree storage version 0
pub(crate) const GROUP_SIZE: u32 = 32;
pub(crate) const SPAN_BITS: u32 = 16;
pub(crate) const ALIGNMENT: usize = 8;

// type: bits 31–24; version: bits 23–16; flags: bits 15–0
pub(crate) const fn slab_format(slab_type: u8, version: u8) -> u32 {
    ((slab_type as u32) << 24) | ((version as u32) << 16)
}

pub(crate) const TREE_FORMAT: u32 = slab_format(0xff, 0);
pub(crate) const EXTRAS: u32 = 1 << 3;
pub(crate) const ERRORS: u32 = 1 << 2;
pub(crate) const MISSING: u32 = 1 << 1;
pub(crate) const SEPARATE_GRAMMAR: u32 = 1;
pub(crate) const BYTE_IDS: u32 = 1 << 4;
pub(crate) const BYTE_GRAMMAR_IDS: u32 = 1 << 5;
pub(crate) const OPTIONAL: u32 = EXTRAS | ERRORS | MISSING | SEPARATE_GRAMMAR;

// Reserve room for both remapped error IDs in every grammar sharing the slab.
pub(crate) fn id_width_flags<'language>(
    languages: impl IntoIterator<Item = &'language Language>,
) -> u32 {
    let mut flags = BYTE_IDS | BYTE_GRAMMAR_IDS;
    for language in languages {
        if language.tables().kind_count > 254 {
            flags &= !BYTE_IDS;
        }
        if language.tables().compact_grammar_count > 254 {
            flags &= !BYTE_GRAMMAR_IDS;
        }
    }
    flags
}

/// Identifies the packed slab format for persistence compatibility.
///
/// **Not in Tree-sitter**
pub fn representation_id() -> u64 {
    TREE_FORMAT as u64
}

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Layout<Column> {
    pub symbol_width: u32,
    pub grammar_width: u32,
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
        let grammar_width = if flags & BYTE_GRAMMAR_IDS != 0 { 1 } else { 2 };
        let slots = capacity
            .checked_mul(GROUP_SIZE)
            .filter(|_| capacity != 0)
            .ok_or(Error::Overflow)?;
        let mut next = Self::WASTE.raw() as u64;
        let mut column = |length: u64| {
            let offset = SlabOffset(next as u32);
            next = (next + length + ALIGNMENT as u64 - 1) & !(ALIGNMENT as u64 - 1);
            offset
        };
        let mut result = Self {
            symbol_width,
            grammar_width,
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
                aligned_bytes(slots, grammar_width)
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

    fn resolve(self, bytes: NonNull<u8>) -> Layout<ColumnPointer> {
        Layout {
            symbol_width: self.symbol_width,
            grammar_width: self.grammar_width,
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
                    aligned_bytes(slots, self.grammar_width) as usize
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

// Column access borrows the owning tree; published slabs are immutable.
unsafe impl Send for ColumnPointer {}
unsafe impl Sync for ColumnPointer {}

impl ColumnPointer {
    #[inline]
    pub fn as_ptr(self) -> *mut u8 {
        self.0
    }

    #[cfg(test)]
    pub fn offset(self, bytes: NonNull<u8>) -> usize {
        self.0 as usize - bytes.as_ptr() as usize
    }
}

pub(crate) trait SlabAddress: Copy {
    fn pointer(self, bytes: NonNull<u8>) -> *mut u8;
}

impl SlabAddress for SlabOffset {
    #[inline]
    fn pointer(self, bytes: NonNull<u8>) -> *mut u8 {
        bytes.as_ptr().wrapping_add(self.raw() as usize)
    }
}

impl SlabAddress for ColumnPointer {
    #[inline]
    fn pointer(self, _bytes: NonNull<u8>) -> *mut u8 {
        self.0
    }
}

pub(crate) struct TreeData {
    pub language: Language,
    pub layout: Layout<ColumnPointer>,
    pub bytes: NonNull<u8>,
    pub length: u32,
    // Small final shrinks retain the allocation; deallocation needs its original size.
    allocation_length: u32,
    owned: bool,
    pub presence_cache: Option<PresenceCache>,
    pub point_data: Option<PointsData>,
}

// Capture the slab address once: raw stores otherwise make LLVM reload it
// from the descriptor. The borrow excludes resizing while the writer is used.
pub(crate) struct SlabWriter<'tree> {
    bytes: NonNull<u8>,
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

impl TreeData {
    #[inline]
    pub fn tables(&self) -> &GrammarView {
        self.language.tables()
    }

    // Column pointers are resolved on allocation and refreshed after slab relocation.
    #[inline]
    pub fn byte(&self, address: impl SlabAddress, index: u32) -> u8 {
        unsafe { *address.pointer(self.bytes).add(index as usize) }
    }

    #[inline]
    pub fn short(&self, address: impl SlabAddress, index: u32) -> u16 {
        u16::from_le(unsafe {
            address
                .pointer(self.bytes)
                .add(index as usize * 2)
                .cast::<u16>()
                .read_unaligned()
        })
    }

    #[inline]
    pub fn word(&self, address: impl SlabAddress, index: u32) -> u32 {
        u32::from_le(unsafe {
            address
                .pointer(self.bytes)
                .add(index as usize * 4)
                .cast::<u32>()
                .read_unaligned()
        })
    }

    #[inline]
    pub fn long(&self, address: impl SlabAddress, index: u32) -> u64 {
        u64::from_le(unsafe {
            address
                .pointer(self.bytes)
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
    pub fn waste(&self, group: u32) -> u32 {
        self.short(Layout::WASTE, group) as u32
    }

    #[inline]
    pub fn group_end(&self, group: u32) -> u32 {
        (group + 1) * GROUP_SIZE - self.waste(group)
    }

    #[inline]
    pub fn first_slot(&self, slot: u32) -> u32 {
        slot - (self.word(self.layout.span_max, slot / GROUP_SIZE) - self.span_delta(slot))
    }

    #[inline]
    pub fn span_delta(&self, slot: u32) -> u32 {
        if SPAN_BITS == 16 {
            self.short(self.layout.span_delta, slot) as u32
        } else {
            self.byte(self.layout.span_delta, slot) as u32
        }
    }

    #[inline]
    pub fn previous_slot(&self, slot: u32) -> Option<u32> {
        let previous = slot.checked_sub(1)?;
        Some(if slot % GROUP_SIZE == 0 {
            previous - self.waste(previous / GROUP_SIZE)
        } else {
            previous
        })
    }

    #[inline]
    pub fn symbol_index(&self, slot: u32) -> SquatterKindId {
        SquatterKindId(self.symbol_id(self.layout.symbol, slot, self.layout.symbol_width))
    }

    #[inline]
    pub fn grammar_index(&self, slot: u32) -> SquatterGrammarId {
        if self.flags() & SEPARATE_GRAMMAR != 0 {
            SquatterGrammarId(self.symbol_id(self.layout.grammar, slot, self.layout.grammar_width))
        } else {
            self.tables().default_grammar(self.symbol_index(slot))
        }
    }

    #[inline]
    fn symbol_id(&self, column: ColumnPointer, slot: u32, width: u32) -> u16 {
        if width == 1 {
            u16::from(self.byte(column, slot))
        } else {
            self.short(column, slot)
        }
    }

    pub fn has_points(&self) -> bool {
        self.point_data.is_some()
    }

    pub fn slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bytes.as_ptr(), self.length as usize) }
    }

    #[inline]
    pub(crate) fn column_slice(&self, column: ColumnPointer, start: usize, length: usize) -> &[u8] {
        // Resolved column pointers and group offsets remain within the retained slab.
        unsafe { std::slice::from_raw_parts(column.as_ptr().add(start), length) }
    }

    pub(crate) fn writer(&mut self) -> SlabWriter<'_> {
        SlabWriter {
            bytes: self.bytes,
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
                .pointer(self.bytes)
                .add(index as usize * 4)
                .cast::<u32>()
                .write_unaligned(value.to_le());
        }
    }

    pub(crate) fn put_long(&mut self, address: impl SlabAddress, index: u32, value: u64) {
        unsafe {
            address
                .pointer(self.bytes)
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

fn prefix() -> usize {
    (size_of::<TreeData>() + ALIGNMENT - 1) & !(ALIGNMENT - 1)
}

fn allocation(length: u32, owned: bool) -> Result<Allocation, Error> {
    let length = prefix()
        .checked_add(if owned { length as usize } else { 0 })
        .ok_or(Error::Overflow)?;
    Allocation::from_size_align(length, ALIGNMENT.max(align_of::<TreeData>()))
        .map_err(|_| Error::Overflow)
}

/// A tree that represents the syntactic structure of a source code file.
///
/// **Different than Tree-sitter:** Owns an immutable packed snapshot with separately attachable
/// side data. Loading slab bytes does not restore point data or presence caches. Use
/// explicit copying or detachment; `Clone`, offset views, and incremental change tracking
/// are not provided.
pub struct Tree(pub(crate) NonNull<TreeData>);
unsafe impl Send for Tree {}
unsafe impl Sync for Tree {}
impl Drop for Tree {
    fn drop(&mut self) {
        unsafe {
            let descriptor = self.0.as_ptr();
            let layout = allocation((*descriptor).allocation_length, (*descriptor).owned).unwrap();
            ptr::drop_in_place(descriptor);
            dealloc(descriptor.cast(), layout);
        }
    }
}

impl Tree {
    #[inline]
    pub(crate) fn data(&self) -> &TreeData {
        unsafe { self.0.as_ref() }
    }

    pub(crate) fn data_mut(&mut self) -> &mut TreeData {
        unsafe { self.0.as_mut() }
    }

    fn allocate(
        language: &Language,
        layout: Layout<SlabOffset>,
        length: u32,
        borrowed: Option<&[u8]>,
        zeroed: bool,
    ) -> Result<Self, Error> {
        // Owned slabs share one allocation with the descriptor. Borrowed slabs
        // allocate only the descriptor; their public wrapper retains the owner.
        let allocation = allocation(length, borrowed.is_none())?;
        let pointer = unsafe {
            if zeroed {
                alloc_zeroed(allocation)
            } else {
                alloc(allocation)
            }
        };
        let pointer = NonNull::new(pointer).unwrap_or_else(|| handle_alloc_error(allocation));
        let bytes = match borrowed {
            Some(bytes) => NonNull::from(bytes).cast(),
            None => NonNull::new(pointer.as_ptr().wrapping_add(prefix())).unwrap(),
        };
        unsafe {
            pointer.cast::<TreeData>().as_ptr().write(TreeData {
                language: language.clone(),
                layout: layout.resolve(bytes),
                bytes,
                length,
                allocation_length: length,
                owned: borrowed.is_none(),
                presence_cache: None,
                point_data: None,
            });
        }
        Ok(Self(pointer.cast()))
    }

    pub(crate) fn empty(language: &Language, capacity: u32) -> Result<Self, Error> {
        // Reserve optional columns while packing. Omit the grammar column when all
        // emitted IDs match, and flag columns when all their bits are zero.
        let flags = TREE_FORMAT | OPTIONAL | id_width_flags([language]);
        let layout = Layout::new(capacity, flags)?;
        let mut tree = Self::allocate(language, layout, layout.end.raw(), None, true)?;
        let data = tree.data_mut();
        data.put_word(SlabOffset(0), 0, flags);
        data.put_word(SlabOffset(0), 1, 0);
        data.put_word(SlabOffset(0), 2, capacity);
        data.put_word(SlabOffset(0), 3, language.tables().dictionary_count);
        Ok(tree)
    }

    /// Borrows the packed slab, excluding separately attached side
    /// data.
    ///
    /// **Not in Tree-sitter**
    pub fn as_bytes(&self) -> &[u8] {
        self.data().slice()
    }

    /// Returns the number of physical groups in use.
    ///
    /// **Not in Tree-sitter**
    pub fn group_count(&self) -> u32 {
        self.data().groups()
    }

    /// Returns the allocated number of physical groups.
    ///
    /// **Not in Tree-sitter**
    pub fn group_capacity(&self) -> u32 {
        self.data().capacity()
    }

    /// Returns the physical slot count, including waste. This
    /// bounds node and child counts within `u32`.
    ///
    /// **Not in Tree-sitter**
    pub fn slot_count(&self) -> u32 {
        self.group_count() * GROUP_SIZE
    }

    /// Reports whether point data is attached.
    ///
    /// **Not in Tree-sitter**
    ///
    /// **Different behavior than Tree-sitter:** Without point data, positions use row zero and
    /// the byte offset as column. Check `has_points()` before relying on line/column
    /// coordinates.
    pub fn has_points(&self) -> bool {
        self.data().has_points()
    }

    /// Serializes the prepared grammar for separate persistence.
    ///
    /// **Not in Tree-sitter**
    pub fn language_cache(&self) -> Result<Vec<u8>, Error> {
        self.data().language.cache()
    }

    /// Copies and validates a packed slab for this grammar. Point
    /// data and presence caches must be attached separately.
    ///
    /// **Not in Tree-sitter**
    pub fn from_bytes(language: &Language, bytes: &[u8]) -> Result<Self, Error> {
        Self::load(language, bytes, false, true)
    }

    /// Copies a packed slab after checking the structural
    /// invariants needed for safe access. Side data is not restored.
    ///
    /// **Not in Tree-sitter**
    pub fn from_bytes_safety_checked(language: &Language, bytes: &[u8]) -> Result<Self, Error> {
        Self::load(language, bytes, false, false)
    }

    /// Validates and borrows immutable, eight-byte-aligned slab
    /// bytes without copying. The returned view cannot outlive those bytes. Side data is
    /// not restored.
    ///
    /// **Not in Tree-sitter**
    pub fn from_bytes_borrowed<'bytes>(
        language: &Language,
        bytes: &'bytes [u8],
    ) -> Result<BorrowedTree<'bytes>, Error> {
        Ok(BorrowedTree {
            tree: Self::load(language, bytes, true, true)?,
            bytes: PhantomData,
        })
    }

    /// Validates and retains an immutable, eight-byte-aligned slab
    /// owner without copying its bytes. Side data is not restored.
    ///
    /// **Not in Tree-sitter**
    pub fn from_retained(
        language: &Language,
        owner: impl StableSlab,
    ) -> Result<RetainedTree, Error> {
        let owner: Box<dyn StableSlab> = Box::new(owner);
        Ok(RetainedTree {
            tree: Self::load(language, owner.bytes(), true, false)?,
            _owner: owner,
        })
    }

    /// Returns the byte length needed for a compact slab copy,
    /// excluding side data.
    ///
    /// **Not in Tree-sitter**
    pub fn compact_size(&self) -> usize {
        let data = self.data();
        Layout::new(data.groups(), data.flags()).unwrap().end.raw() as usize
    }

    /// Copies a compact slab into a destination of exactly
    /// `compact_size()` bytes. Side data is excluded. Loading the result without copying
    /// requires eight-byte alignment.
    ///
    /// **Not in Tree-sitter**
    pub fn copy_compact_into<'bytes>(
        &self,
        destination: &'bytes mut [MaybeUninit<u8>],
    ) -> Result<&'bytes mut [u8], Error> {
        if destination.len() != self.compact_size() {
            return Err(Error::InvalidArgument);
        }
        let data = self.data();
        let layout = Layout::new(data.groups(), data.flags())?;
        unsafe {
            let destination = destination.as_mut_ptr().cast::<u8>();
            self.copy_columns(destination, layout, data.flags());
            destination
                .add(8)
                .cast::<u32>()
                .write_unaligned(data.groups().to_le());
        }
        Ok(unsafe {
            std::slice::from_raw_parts_mut(destination.as_mut_ptr().cast(), destination.len())
        })
    }

    unsafe fn copy_columns(&self, destination: *mut u8, next: Layout<SlabOffset>, flags: u32) {
        let data = self.data();
        unsafe {
            ptr::copy_nonoverlapping(data.bytes.as_ptr(), destination, 16);
        }
        let mut previous = 16;
        for ((offset, _), (target, length)) in data
            .layout
            .columns(data.groups(), flags)
            .into_iter()
            .zip(next.columns(data.groups(), flags))
        {
            unsafe {
                ptr::write_bytes(
                    destination.add(previous),
                    0,
                    target.raw() as usize - previous,
                );
                ptr::copy_nonoverlapping(
                    offset.as_ptr(),
                    destination.add(target.raw() as usize),
                    length,
                );
            }
            previous = target.raw() as usize + length;
        }
        unsafe {
            ptr::write_bytes(
                destination.add(previous),
                0,
                next.end.raw() as usize - previous,
            );
        }
    }

    pub(crate) fn resize(&mut self, capacity: u32, flags: u32) -> Result<(), Error> {
        let data = self.data();
        let layout = Layout::new(capacity, flags)?;
        let mut replacement =
            Self::allocate(&data.language, layout, layout.end.raw(), None, false)?;
        unsafe {
            self.copy_columns(replacement.data().bytes.as_ptr(), layout, flags);
        }
        replacement.data_mut().put_word(SlabOffset(0), 0, flags);
        replacement.data_mut().put_word(SlabOffset(0), 2, capacity);
        *self = replacement;
        Ok(())
    }

    pub(crate) fn finish_layout(
        &mut self,
        capacity: u32,
        optional_columns: u32,
    ) -> Result<(), Error> {
        self.finish_layout_with_progress(capacity, optional_columns, &mut Default::default())
    }

    // Cancellation can leave moved columns behind; the caller must discard the tree.
    pub(crate) fn finish_layout_with_progress(
        &mut self,
        capacity: u32,
        optional_columns: u32,
        progress: &mut crate::packing::Progress<'_>,
    ) -> Result<(), Error> {
        progress.poll()?;
        let data = self.data();
        assert!(data.owned);
        assert!(capacity >= data.groups());
        assert!(capacity <= data.capacity());
        assert_eq!(optional_columns & !OPTIONAL, 0);
        assert_eq!(optional_columns & !data.flags(), 0);

        let flags = (data.flags() & !OPTIONAL) | optional_columns;
        let next = Layout::new(capacity, flags)?;
        let data = self.data_mut();
        let previous = data.layout;

        // Shrinking capacity or removing columns only moves offsets earlier.
        // Copy left to right so no destination overwrites a later column's source.
        for ((source, _), (destination, length)) in previous
            .columns(data.groups(), flags)
            .into_iter()
            .zip(next.columns(data.groups(), flags))
        {
            let destination = destination.pointer(data.bytes);
            if source.as_ptr() != destination && length != 0 {
                let chunk_size = if progress.enabled() {
                    64 * 1024
                } else {
                    length
                };
                for offset in (0..length).step_by(chunk_size) {
                    progress.poll()?;
                    unsafe {
                        ptr::copy(
                            source.as_ptr().add(offset),
                            destination.add(offset),
                            chunk_size.min(length - offset),
                        );
                    }
                }
            }
        }
        data.layout = next.resolve(data.bytes);
        data.length = next.end.raw();
        data.put_word(SlabOffset(0), 0, flags);
        data.put_word(SlabOffset(0), 2, capacity);
        self.shrink_allocation(next, 256)
    }

    fn shrink_allocation(
        &mut self,
        layout: Layout<SlabOffset>,
        threshold: u32,
    ) -> Result<(), Error> {
        let data = self.data();
        let allocated = data.allocation_length;
        let length = data.length;
        let excess = allocated - length;
        if excess == 0 || excess < threshold.min(allocated / 2) {
            return Ok(());
        }
        let old = allocation(allocated, true)?;
        let new = allocation(length, true)?;
        let pointer = unsafe { realloc(self.0.as_ptr().cast(), old, new.size()) };
        self.0 = NonNull::new(pointer)
            .unwrap_or_else(|| handle_alloc_error(new))
            .cast();
        let pointer = self.0.as_ptr();
        let data = self.data_mut();
        data.bytes = NonNull::new(pointer.cast::<u8>().wrapping_add(prefix())).unwrap();
        data.layout = layout.resolve(data.bytes);
        data.allocation_length = length;
        Ok(())
    }

    /// Compact this tree's columns without copying its attached side data.
    /// Small unused allocation tails may be retained.
    ///
    /// **Not in Tree-sitter**. Retains attached side data while changing the slab
    /// allocation. Release borrowed views before mutation.
    pub fn repack_in_place(&mut self) -> Result<(), Error> {
        let data = self.data();
        self.finish_layout(data.groups(), data.flags() & OPTIONAL)
    }

    /// Return a compact copy, preserving this tree and copying its attached side data.
    ///
    /// **Not in Tree-sitter**. Explicitly copies the slab and attached side data into a
    /// compact owned tree.
    pub fn repack(&self) -> Result<Self, Error> {
        let layout = Layout::new(self.group_count(), self.data().flags())?;
        let mut result = Self::allocate(
            &self.data().language,
            layout,
            self.compact_size() as u32,
            None,
            false,
        )?;
        unsafe {
            let destination = std::slice::from_raw_parts_mut(
                result.data().bytes.as_ptr().cast::<MaybeUninit<u8>>(),
                result.data().length as usize,
            );
            self.copy_compact_into(destination)?;
        }
        if let Some(cache) = &self.data().presence_cache {
            result
                .set_presence_cache(PresenceCache::copy_from_bytes(&result, cache.as_bytes())?)?;
        }
        if let Some(points) = &self.data().point_data {
            result.set_point_data(PointsData::copy_from_bytes(&result, points.as_bytes())?)?;
        }
        Ok(result)
    }
}

impl std::fmt::Debug for Tree {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Tree")
            .field("groups", &self.group_count())
            .field("bytes", &self.as_bytes().len())
            .finish()
    }
}

/// Borrows immutable, eight-byte-aligned slab bytes for its
/// lifetime. Release this view and its nodes before mutating or releasing the backing
/// storage.
///
/// **Not in Tree-sitter**
pub struct BorrowedTree<'bytes> {
    tree: Tree,
    bytes: PhantomData<&'bytes [u8]>,
}

impl Deref for BorrowedTree<'_> {
    type Target = Tree;
    fn deref(&self) -> &Tree {
        &self.tree
    }
}

/// Immutable slab storage whose address and length remain stable until drop.
///
/// # Safety
/// All returned bytes must stay alive and immutable, including across owner moves
/// and concurrent access. No other party may resize, mutate, or unmap the storage.
///
/// **Not in Tree-sitter**. Provides retained backing storage for packed trees and side
/// data.
pub unsafe trait StableSlab: Send + Sync + 'static {
    fn bytes(&self) -> &[u8];
}

/// A tree retaining its immutable slab owner.
///
/// The inner tree cannot be replaced independently of its retained storage.
///
/// ```compile_fail
/// # fn example(mut retained: tree_squatter::RetainedTree, replacement: tree_squatter::Tree) {
/// let escaped = std::mem::replace(&mut *retained, replacement);
/// # }
/// ```
///
/// **Not in Tree-sitter**. Keeps the stable slab owner alive. Loading retains the bytes
/// without copying; side data remains separate.
pub struct RetainedTree {
    // The descriptor must be destroyed before its retained storage.
    tree: Tree,
    _owner: Box<dyn StableSlab>,
}

impl Deref for RetainedTree {
    type Target = Tree;
    fn deref(&self) -> &Tree {
        &self.tree
    }
}

impl RetainedTree {
    /// Validates and attaches separately loaded symbol-presence
    /// data. Release borrowed tree views before replacing side data.
    pub fn set_presence_cache(&mut self, cache: PresenceCache) -> Result<(), SideDataError> {
        self.tree.set_presence_cache(cache)
    }

    /// Validates and attaches separately loaded point data. Release
    /// borrowed tree views before replacing side data.
    pub fn set_point_data(&mut self, points: PointsData) -> Result<(), SideDataError> {
        self.tree.set_point_data(points)
    }

    /// Drops the optional cache. Scan results stay the same; scan
    /// cost can change.
    pub fn drop_presence_cache(&mut self) {
        self.tree.drop_presence_cache();
    }

    /// Drops point data. Point-dependent APIs then use row zero and
    /// byte offsets as columns.
    pub fn drop_point_data(&mut self) {
        self.tree.drop_point_data();
    }

    /// Copies the retained slab and attached side data into an
    /// independent owned tree.
    pub fn detach(&self) -> Result<Tree, Error> {
        let mut tree = Tree::from_bytes_safety_checked(&self.data().language, self.as_bytes())?;
        if let Some(cache) = self.presence_cache() {
            tree.set_presence_cache(PresenceCache::copy_from_bytes(&tree, cache.as_bytes())?)?;
        }
        if let Some(points) = self.point_data() {
            tree.set_point_data(PointsData::copy_from_bytes(&tree, points.as_bytes())?)?;
        }
        Ok(tree)
    }
}

impl Tree {
    /// Tests a physical group for a displayed kind. Without a
    /// presence cache this scans the group, with identical results.
    ///
    /// **Not in Tree-sitter**
    pub fn group_has_symbol(&self, group: u32, symbol: KindId) -> bool {
        self.data().group_has_symbol(group, symbol)
    }
}

impl TreeData {
    pub fn group_has_symbol(&self, group: u32, symbol: KindId) -> bool {
        let Some(symbol) = self.tables().remap_kind(symbol) else {
            return false;
        };
        if group >= self.groups() {
            return false;
        }
        if let Some(cache) = &self.presence_cache {
            return cache.has(group, symbol.raw() as usize, self.groups());
        }
        (group * GROUP_SIZE..self.group_end(group)).any(|slot| self.symbol_index(slot) == symbol)
    }
}

impl Tree {
    fn load(language: &Language, bytes: &[u8], borrowed: bool, _full: bool) -> Result<Self, Error> {
        if bytes.len() < 16 || bytes.len() > u32::MAX as usize {
            return Err(Error::InvalidSlab);
        }
        if borrowed && bytes.as_ptr() as usize % ALIGNMENT != 0 {
            return Err(Error::InvalidArgument);
        }
        let header =
            |index: usize| u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap());
        let flags = header(0);
        let groups = header(1);
        let capacity = header(2);
        if flags & !(OPTIONAL | BYTE_IDS | BYTE_GRAMMAR_IDS) != TREE_FORMAT
            || (flags & BYTE_IDS != 0 && language.tables().kind_count > 254)
            || (flags & BYTE_GRAMMAR_IDS != 0 && language.tables().compact_grammar_count > 254)
            || groups == 0
            || groups > capacity
            || (flags & MISSING != 0 && flags & ERRORS == 0)
            || header(3) != language.tables().dictionary_count
        {
            return Err(Error::InvalidSlab);
        }
        let layout = Layout::new(capacity, flags).map_err(|_| Error::InvalidSlab)?;
        if layout.end.raw() as usize != bytes.len() {
            return Err(Error::InvalidSlab);
        }
        let tree = Self::allocate(
            language,
            layout,
            bytes.len() as u32,
            borrowed.then_some(bytes),
            false,
        )?;
        if !borrowed {
            unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), tree.data().bytes.as_ptr(), bytes.len());
            }
        }
        tree.validate_nodes()?;
        Ok(tree)
    }

    fn validate_nodes(&self) -> Result<(), Error> {
        let data = self.data();
        let tables = data.tables();
        let symbols = tables.kind_count + 2;
        for group in 0..data.groups() {
            if data.waste(group) >= GROUP_SIZE {
                return Err(Error::InvalidSlab);
            }
        }
        let root = data.group_end(data.groups() - 1) - 1;
        let mut ends = Vec::with_capacity(64);
        for group in (0..data.groups()).rev() {
            let span_max = data.word(data.layout.span_max, group) as u64;
            let start_base = data.word(data.layout.start_byte_base, group) as u64;
            let end_base = data.word(data.layout.end_byte_base, group);
            for slot in (group * GROUP_SIZE..data.group_end(group)).rev() {
                while ends.last().is_some_and(|end| *end > slot) {
                    ends.pop();
                }
                let span = span_max
                    .checked_sub(data.span_delta(slot) as u64)
                    .ok_or(Error::InvalidSlab)?;
                if span > slot as u64 {
                    return Err(Error::InvalidSlab);
                }
                let end = slot - span as u32;
                if end != 0 && end - 1 >= data.group_end((end - 1) / GROUP_SIZE) {
                    return Err(Error::InvalidSlab);
                }
                let last = data.bit(data.layout.last, slot);
                let field = data.short(data.layout.field, slot) as u32;
                if slot == root {
                    if end != 0 || !last || field != 0 {
                        return Err(Error::InvalidSlab);
                    }
                } else if !ends
                    .last()
                    .is_some_and(|parent| end >= *parent && last == (end == *parent))
                {
                    return Err(Error::InvalidSlab);
                }
                let symbol = u32::from(data.symbol_index(slot).raw());
                if symbol == 0 || symbol >= symbols || field > tables.field_count {
                    return Err(Error::InvalidSlab);
                }
                let grammar = u32::from(data.grammar_index(slot).raw());
                if grammar == 0 || grammar >= tables.compact_grammar_count + 2 {
                    return Err(Error::InvalidSlab);
                }
                let supertype = data.short(data.layout.supertype, slot) as u32;
                if supertype
                    >= if tables.supertype_count > 8 {
                        tables.dictionary_count
                    } else {
                        1 << tables.supertype_count
                    }
                {
                    return Err(Error::InvalidSlab);
                }
                let start = start_base + data.byte(data.layout.start_byte_delta, slot) as u64;
                let end_delta = data.short(data.layout.end_byte_delta, slot) as u32;
                if start > u32::MAX as u64
                    || end_delta > end_base
                    || start > (end_base - end_delta) as u64
                {
                    return Err(Error::InvalidSlab);
                }
                ends.push(end);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_copies_initialize_gaps_and_unused_capacity() {
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let language = Language::new(&language).unwrap();
        for optional in 0..=OPTIONAL {
            for width in [0, BYTE_IDS, BYTE_GRAMMAR_IDS, BYTE_IDS | BYTE_GRAMMAR_IDS] {
                let flags = TREE_FORMAT | optional | width;
                let layout = Layout::new(5, flags).unwrap();
                let mut tree =
                    Tree::allocate(&language, layout, layout.end.raw(), None, false).unwrap();
                unsafe {
                    ptr::write_bytes(tree.data().bytes.as_ptr(), 0x5a, layout.end.raw() as usize);
                }
                tree.data_mut().put_word(SlabOffset(0), 0, flags);
                tree.data_mut().put_word(SlabOffset(0), 1, 3);
                tree.data_mut().put_word(SlabOffset(0), 2, 5);
                for capacity in [3, 9] {
                    let next = Layout::new(capacity, flags).unwrap();
                    let mut destination = vec![0xff; next.end.raw() as usize];
                    unsafe {
                        tree.copy_columns(destination.as_mut_ptr(), next, flags);
                    }
                    assert_eq!(&destination[..16], &tree.as_bytes()[..16]);
                    let mut copied = vec![false; destination.len()];
                    for (offset, length) in next.columns(3, flags) {
                        let start = offset.raw() as usize;
                        copied[start..start + length].fill(true);
                    }
                    for index in 16..destination.len() {
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
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let language = Language::new(&language).unwrap();
        let layout = Layout::new(1, TREE_FORMAT | BYTE_IDS).unwrap();
        let length = layout.end.raw();
        for (excess, threshold, shrink) in [
            (0, 0, false),
            (1, 0, true),
            (255, 256, false),
            (256, 256, true),
            (length - 2, u32::MAX, false),
            (length, u32::MAX, true),
        ] {
            let mut tree = Tree::allocate(&language, layout, length + excess, None, true).unwrap();
            tree.data_mut().length = length;
            tree.shrink_allocation(layout, threshold).unwrap();
            assert_eq!(
                tree.data().allocation_length,
                if shrink { length } else { length + excess }
            );
            assert_eq!(tree.data().layout.end, layout.end);
        }
    }
}
