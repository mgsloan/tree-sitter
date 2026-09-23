use crate::{
    Error, Grammar, KindId,
    native::GrammarView,
    side_data::{PointData, PresenceCache, SideDataError},
    types::{RemappedGrammarKindId, RemappedKindId, SlabOffset, SymbolCode},
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
pub(crate) const OPTIONAL: u32 = EXTRAS | ERRORS | MISSING | SEPARATE_GRAMMAR;

pub fn representation_id() -> u64 {
    TREE_FORMAT as u64
}

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Layout<Column> {
    pub waste: Column,
    pub start_byte_base: Column,
    pub start_byte_delta: Column,
    pub end_byte_base: Column,
    pub end_byte_delta: Column,
    pub span_base: Column,
    pub span_delta: Column,
    pub symbol: Column,
    pub field: Column,
    pub supertype: Column,
    pub last: Column,
    pub extra: Column,
    pub error: Column,
    pub missing: Column,
    pub grammar: Column,
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
        let slots = capacity
            .checked_mul(GROUP_SIZE)
            .filter(|_| capacity != 0)
            .ok_or(Error::Overflow)?;
        let mut next = Self::WASTE.get() as u64;
        let mut column = |length: u64| {
            let offset = SlabOffset(next as u32);
            next = (next + length + ALIGNMENT as u64 - 1) & !(ALIGNMENT as u64 - 1);
            offset
        };
        let mut result = Self {
            waste: column(aligned_bytes(capacity, 2)),
            start_byte_base: column(aligned_bytes(capacity, 4)),
            start_byte_delta: column(aligned_bytes(slots, 1)),
            end_byte_base: column(aligned_bytes(capacity, 4)),
            end_byte_delta: column(aligned_bytes(slots, 2)),
            span_base: column(aligned_bytes(capacity, 4)),
            span_delta: column(aligned_bytes(slots, SPAN_BITS / 8)),
            symbol: column(aligned_bytes(slots, 2)),
            field: column(aligned_bytes(slots, 2)),
            supertype: column(aligned_bytes(slots, 2)),
            last: column(bit_bytes(slots)),
            extra: column(if flags & EXTRAS != 0 {
                bit_bytes(slots)
            } else {
                0
            }),
            error: column(if flags & ERRORS != 0 {
                bit_bytes(capacity)
            } else {
                0
            }),
            missing: column(if flags & MISSING != 0 {
                bit_bytes(slots)
            } else {
                0
            }),
            grammar: column(if flags & SEPARATE_GRAMMAR != 0 {
                aligned_bytes(slots, 2)
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
            waste: ColumnPointer(self.waste.pointer(bytes)),
            start_byte_base: ColumnPointer(self.start_byte_base.pointer(bytes)),
            start_byte_delta: ColumnPointer(self.start_byte_delta.pointer(bytes)),
            end_byte_base: ColumnPointer(self.end_byte_base.pointer(bytes)),
            end_byte_delta: ColumnPointer(self.end_byte_delta.pointer(bytes)),
            span_base: ColumnPointer(self.span_base.pointer(bytes)),
            span_delta: ColumnPointer(self.span_delta.pointer(bytes)),
            symbol: ColumnPointer(self.symbol.pointer(bytes)),
            field: ColumnPointer(self.field.pointer(bytes)),
            supertype: ColumnPointer(self.supertype.pointer(bytes)),
            last: ColumnPointer(self.last.pointer(bytes)),
            extra: ColumnPointer(self.extra.pointer(bytes)),
            error: ColumnPointer(self.error.pointer(bytes)),
            missing: ColumnPointer(self.missing.pointer(bytes)),
            grammar: ColumnPointer(self.grammar.pointer(bytes)),
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
            (self.span_base, aligned_bytes(groups, 4) as usize),
            (
                self.span_delta,
                aligned_bytes(slots, SPAN_BITS / 8) as usize,
            ),
            (self.symbol, aligned_bytes(slots, 2) as usize),
            (self.field, aligned_bytes(slots, 2) as usize),
            (self.supertype, aligned_bytes(slots, 2) as usize),
            (self.last, bit_bytes(slots) as usize),
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
                    bit_bytes(groups) as usize
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
            (
                self.grammar,
                if flags & SEPARATE_GRAMMAR != 0 {
                    aligned_bytes(slots, 2) as usize
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

    #[inline]
    pub fn add(self, bytes: usize) -> Self {
        Self(self.0.wrapping_add(bytes))
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
        bytes.as_ptr().wrapping_add(self.get() as usize)
    }
}

impl SlabAddress for ColumnPointer {
    #[inline]
    fn pointer(self, _bytes: NonNull<u8>) -> *mut u8 {
        self.0
    }
}

pub(crate) struct TreeData {
    pub grammar: Grammar,
    pub layout: Layout<ColumnPointer>,
    pub bytes: NonNull<u8>,
    pub length: u32,
    // Small final shrinks retain the allocation; deallocation needs its original size.
    allocation_length: u32,
    owned: bool,
    pub presence_cache: Option<PresenceCache>,
    pub point_data: Option<PointData>,
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
        self.grammar.tables()
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
        slot - self.word(self.layout.span_base, slot / GROUP_SIZE) - self.span_delta(slot)
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
    pub fn symbol_code(&self, slot: u32) -> SymbolCode {
        SymbolCode(self.short(self.layout.symbol, slot))
    }

    #[inline]
    pub fn symbol_index(&self, slot: u32) -> RemappedKindId {
        RemappedKindId(self.symbol_code(slot).get() >> self.tables().symbol_shift)
    }

    #[inline]
    pub fn grammar_index(&self, slot: u32) -> RemappedGrammarKindId {
        let tables = self.tables();
        let code = self.symbol_code(slot).get() as u32;
        let kind = if self.flags() & SEPARATE_GRAMMAR != 0 {
            self.short(self.layout.grammar, slot)
        } else if tables.separate != 0 {
            code as u16
        } else if tables.encoding == 2 {
            (code & 255) as u16
        } else {
            let selector = code & ((1 << tables.symbol_shift) - 1);
            unsafe {
                if tables.encoding == 1 {
                    if selector == 0 {
                        *tables.defaults.add((code >> tables.symbol_shift) as usize)
                    } else {
                        *tables.grammar_ids.add(selector as usize)
                    }
                } else {
                    *tables.grammar_ids.add(code as usize)
                }
            }
        };
        RemappedGrammarKindId(kind)
    }

    pub fn has_points(&self) -> bool {
        self.point_data.is_some()
    }

    pub fn slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bytes.as_ptr(), self.length as usize) }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(not(feature = "typed-seek"), not(feature = "typed-presence-scan"))
    ))]
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
        grammar: &Grammar,
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
                grammar: grammar.clone(),
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

    pub(crate) fn empty(grammar: &Grammar, capacity: u32) -> Result<Self, Error> {
        // Reserve optional columns while packing. Their presence is only known
        // after traversal, when unused columns can be removed together.
        let flags = TREE_FORMAT
            | EXTRAS
            | ERRORS
            | MISSING
            | if grammar.tables().separate != 0 {
                SEPARATE_GRAMMAR
            } else {
                0
            };
        let layout = Layout::new(capacity, flags)?;
        let mut tree = Self::allocate(grammar, layout, layout.end.get(), None, true)?;
        let data = tree.data_mut();
        data.put_word(SlabOffset(0), 0, flags);
        data.put_word(SlabOffset(0), 1, 0);
        data.put_word(SlabOffset(0), 2, capacity);
        data.put_word(SlabOffset(0), 3, grammar.tables().dictionary_count);
        Ok(tree)
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.data().slice()
    }

    pub fn group_count(&self) -> u32 {
        self.data().groups()
    }

    pub fn group_capacity(&self) -> u32 {
        self.data().capacity()
    }

    pub fn slot_count(&self) -> u32 {
        self.group_count() * GROUP_SIZE
    }

    pub fn has_points(&self) -> bool {
        self.data().has_points()
    }

    pub fn grammar_cache(&self) -> Result<Vec<u8>, Error> {
        self.data().grammar.cache()
    }

    pub fn from_bytes(grammar: &Grammar, bytes: &[u8]) -> Result<Self, Error> {
        Self::load(grammar, bytes, false, true)
    }

    pub fn from_bytes_safety_checked(grammar: &Grammar, bytes: &[u8]) -> Result<Self, Error> {
        Self::load(grammar, bytes, false, false)
    }

    pub fn from_bytes_borrowed<'bytes>(
        grammar: &Grammar,
        bytes: &'bytes [u8],
    ) -> Result<BorrowedTree<'bytes>, Error> {
        Ok(BorrowedTree {
            tree: Self::load(grammar, bytes, true, true)?,
            bytes: PhantomData,
        })
    }

    pub fn from_owned_slab(grammar: &Grammar, owner: impl StableSlab) -> Result<BackedTree, Error> {
        let owner: Box<dyn StableSlab> = Box::new(owner);
        Ok(BackedTree {
            tree: Self::load(grammar, owner.bytes(), true, false)?,
            _owner: owner,
        })
    }

    pub fn compact_size(&self) -> usize {
        let data = self.data();
        Layout::new(data.groups(), data.flags()).unwrap().end.get() as usize
            + (data.length - data.layout.end.get()) as usize
    }

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
            self.copy_columns(destination, layout, data.flags(), true);
            ptr::copy_nonoverlapping(
                data.bytes.as_ptr().add(data.layout.end.get() as usize),
                destination.add(layout.end.get() as usize),
                (data.length - data.layout.end.get()) as usize,
            );
            destination
                .add(8)
                .cast::<u32>()
                .write_unaligned(data.groups().to_le());
        }
        Ok(unsafe {
            std::slice::from_raw_parts_mut(destination.as_mut_ptr().cast(), destination.len())
        })
    }

    unsafe fn copy_columns(
        &self,
        destination: *mut u8,
        next: Layout<SlabOffset>,
        flags: u32,
        padding: bool,
    ) {
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
                if padding {
                    ptr::write_bytes(
                        destination.add(previous),
                        0,
                        target.get() as usize - previous,
                    );
                }
                ptr::copy_nonoverlapping(
                    offset.as_ptr(),
                    destination.add(target.get() as usize),
                    length,
                );
            }
            previous = target.get() as usize + length;
        }
        if padding {
            unsafe {
                ptr::write_bytes(
                    destination.add(previous),
                    0,
                    next.end.get() as usize - previous,
                );
            }
        }
    }

    pub(crate) fn resize(
        &mut self,
        capacity: u32,
        flags: u32,
        trailing: u32,
        preserve: bool,
    ) -> Result<(), Error> {
        let data = self.data();
        let layout = Layout::new(capacity, flags)?;
        let length = layout
            .end
            .get()
            .checked_add(trailing)
            .ok_or(Error::Overflow)?;
        let mut replacement = Self::allocate(&data.grammar, layout, length, None, true)?;
        unsafe {
            self.copy_columns(replacement.data().bytes.as_ptr(), layout, flags, false);
            if preserve {
                ptr::copy_nonoverlapping(
                    data.bytes.as_ptr().add(data.layout.end.get() as usize),
                    replacement
                        .data()
                        .bytes
                        .as_ptr()
                        .add(layout.end.get() as usize),
                    trailing as usize,
                );
            }
        }
        replacement.data_mut().put_word(SlabOffset(0), 0, flags);
        replacement.data_mut().put_word(SlabOffset(0), 2, capacity);
        *self = replacement;
        Ok(())
    }

    pub(crate) fn finish_layout(
        &mut self,
        capacity: u32,
        flags: u32,
        trailing: u32,
    ) -> Result<(), Error> {
        if capacity != self.group_capacity() {
            return self.resize(capacity, flags, trailing, false);
        }
        let next = Layout::new(capacity, flags)?;
        let length = next
            .end
            .get()
            .checked_add(trailing)
            .ok_or(Error::Overflow)?;
        let data = self.data_mut();
        let previous = data.layout;

        // Only the optional tail moves when capacity is unchanged. Copy from
        // left to right so removing columns cannot overwrite a later source.
        for (flag, source, destination, end) in [
            (ERRORS, previous.error, next.error, next.missing),
            (MISSING, previous.missing, next.missing, next.grammar),
            (SEPARATE_GRAMMAR, previous.grammar, next.grammar, next.end),
        ] {
            if flags & flag != 0 {
                unsafe {
                    ptr::copy(
                        source.as_ptr(),
                        data.bytes.as_ptr().add(destination.get() as usize),
                        (end.get() - destination.get()) as usize,
                    );
                }
            }
        }
        data.layout = next.resolve(data.bytes);
        data.length = length;
        data.put_word(SlabOffset(0), 0, flags);
        let allocated = data.allocation_length;

        if length > allocated || allocated - length >= 256 {
            let old = allocation(allocated, true)?;
            let new = allocation(length, true)?;
            let pointer = unsafe { realloc(self.0.as_ptr().cast(), old, new.size()) };
            self.0 = NonNull::new(pointer)
                .unwrap_or_else(|| handle_alloc_error(new))
                .cast();
            let pointer = self.0.as_ptr();
            let data = self.data_mut();
            data.bytes = NonNull::new(pointer.cast::<u8>().wrapping_add(prefix())).unwrap();
            data.layout = next.resolve(data.bytes);
            data.allocation_length = length;
        }
        Ok(())
    }

    pub fn repack(&self) -> Result<Self, Error> {
        let layout = Layout::new(self.group_count(), self.data().flags())?;
        let mut result = Self::allocate(
            &self.data().grammar,
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
            result.set_point_data(PointData::copy_from_bytes(&result, points.as_bytes())?)?;
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
pub unsafe trait StableSlab: Send + Sync + 'static {
    fn bytes(&self) -> &[u8];
}

/// A tree retaining its immutable slab owner.
///
/// The inner tree cannot be replaced independently of its backing storage.
///
/// ```compile_fail
/// # fn example(mut backed: tree_squatter::BackedTree, replacement: tree_squatter::Tree) {
/// let escaped = std::mem::replace(&mut *backed, replacement);
/// # }
/// ```
pub struct BackedTree {
    // The descriptor must be destroyed before its backing storage.
    tree: Tree,
    _owner: Box<dyn StableSlab>,
}

impl Deref for BackedTree {
    type Target = Tree;
    fn deref(&self) -> &Tree {
        &self.tree
    }
}

impl BackedTree {
    pub fn set_presence_cache(&mut self, cache: PresenceCache) -> Result<(), SideDataError> {
        self.tree.set_presence_cache(cache)
    }

    pub fn set_point_data(&mut self, points: PointData) -> Result<(), SideDataError> {
        self.tree.set_point_data(points)
    }

    pub fn drop_presence_cache(&mut self) {
        self.tree.drop_presence_cache();
    }

    pub fn drop_point_data(&mut self) {
        self.tree.drop_point_data();
    }

    pub fn detach(&self) -> Result<Tree, Error> {
        let mut tree = Tree::from_bytes_safety_checked(&self.data().grammar, self.as_bytes())?;
        if let Some(cache) = self.presence_cache() {
            tree.set_presence_cache(PresenceCache::copy_from_bytes(&tree, cache.as_bytes())?)?;
        }
        if let Some(points) = self.point_data() {
            tree.set_point_data(PointData::copy_from_bytes(&tree, points.as_bytes())?)?;
        }
        Ok(tree)
    }
}

impl Tree {
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
            return cache.has(group, symbol.get() as usize, self.groups());
        }
        (group * GROUP_SIZE..self.group_end(group)).any(|slot| self.symbol_index(slot) == symbol)
    }
}

impl Tree {
    fn load(grammar: &Grammar, bytes: &[u8], borrowed: bool, _full: bool) -> Result<Self, Error> {
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
        if flags & !OPTIONAL != TREE_FORMAT
            || groups == 0
            || groups > capacity
            || (flags & MISSING != 0 && flags & ERRORS == 0)
            || (flags & SEPARATE_GRAMMAR != 0 && grammar.tables().separate == 0)
            || header(3) != grammar.tables().dictionary_count
        {
            return Err(Error::InvalidSlab);
        }
        let layout = Layout::new(capacity, flags).map_err(|_| Error::InvalidSlab)?;
        if layout.end.get() as usize != bytes.len() {
            return Err(Error::InvalidSlab);
        }
        let tree = Self::allocate(
            grammar,
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
        let symbols = tables.symbol_count + 2;
        for group in 0..data.groups() {
            if data.waste(group) >= GROUP_SIZE {
                return Err(Error::InvalidSlab);
            }
        }
        let root = data.group_end(data.groups() - 1) - 1;
        let mut ends = Vec::with_capacity(64);
        for group in (0..data.groups()).rev() {
            let span_base = data.word(data.layout.span_base, group) as u64;
            let start_base = data.word(data.layout.start_byte_base, group) as u64;
            let end_base = data.word(data.layout.end_byte_base, group);
            for slot in (group * GROUP_SIZE..data.group_end(group)).rev() {
                while ends.last().is_some_and(|end| *end > slot) {
                    ends.pop();
                }
                let span = span_base + data.span_delta(slot) as u64;
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
                let code = data.short(data.layout.symbol, slot) as u32;
                let symbol = code >> tables.symbol_shift;
                if symbol >= symbols
                    || field > tables.field_count
                    || (symbol < tables.symbol_count
                        && unsafe { *tables.public_symbols.add(symbol as usize) } as u32 != symbol)
                {
                    return Err(Error::InvalidSlab);
                }
                if tables.separate != 0 || tables.encoding == 2 {
                    if u32::from(data.grammar_index(slot).get()) >= symbols {
                        return Err(Error::InvalidSlab);
                    }
                } else {
                    let variant = code & ((1 << tables.symbol_shift) - 1);
                    let count = unsafe { *tables.counts.add(symbol as usize) } as u32;
                    if if tables.encoding == 1 {
                        count == 0
                            || variant >= tables.dictionary_length
                            || (variant == 0 && count != 1)
                    } else {
                        variant >= count
                    } {
                        return Err(Error::InvalidSlab);
                    }
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
