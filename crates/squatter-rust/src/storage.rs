use crate::{
    Error, Grammar, KindId,
    native::GrammarView,
    types::{RemappedGrammarKindId, RemappedKindId, SlabOffset, SymbolCode},
};
use std::{
    alloc::{Layout as Allocation, alloc, alloc_zeroed, dealloc, handle_alloc_error, realloc},
    marker::PhantomData,
    mem::MaybeUninit,
    ops::Deref,
    ptr::{self, NonNull},
};

include!(concat!(env!("OUT_DIR"), "/format.rs"));
pub(crate) const VERSION: u32 = 0x5351_0001
    | match GROUP_SIZE {
        32 => 2,
        64 => 4,
        _ => 0,
    }
    | if ALIGNMENT == 64 { 8 } else { 0 };
pub(crate) const NO_POINTS: u32 = 0x100;
pub(crate) const PRESENCE: u32 = 0x200;
pub(crate) const WIDE_SUPERTYPES: u32 = 0x400;
pub(crate) const SEPARATE_GRAMMAR: u32 = 0x800;
pub(crate) const EXTRAS: u32 = 0x1000;
pub(crate) const MISSING: u32 = 0x2000;
pub(crate) const ERRORS: u32 = 0x4000;
pub(crate) const OPTIONAL: u32 = SEPARATE_GRAMMAR | EXTRAS | MISSING | ERRORS;

pub fn representation_id() -> u64 {
    VERSION as u64 | ((GROUP_SIZE as u64) << 32) | ((ALIGNMENT as u64) << 40)
}

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Layout {
    pub waste: SlabOffset,
    pub start_byte_base: SlabOffset,
    pub start_byte_delta: SlabOffset,
    pub end_byte_base: SlabOffset,
    pub end_byte_delta: SlabOffset,
    pub span_base: SlabOffset,
    pub span_delta: SlabOffset,
    pub symbol: SlabOffset,
    pub field: SlabOffset,
    pub supertype: SlabOffset,
    pub last: SlabOffset,
    pub start_point_base: SlabOffset,
    pub start_point: SlabOffset,
    pub end_point_base: SlabOffset,
    pub end_point: SlabOffset,
    pub extra: SlabOffset,
    pub missing: SlabOffset,
    pub error: SlabOffset,
    pub grammar: SlabOffset,
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

impl Layout {
    // The first column follows the fixed header, independent of capacity and flags.
    const WASTE: SlabOffset = SlabOffset(((16 + ALIGNMENT - 1) & !(ALIGNMENT - 1)) as u32);

    pub fn new(capacity: u32, flags: u32) -> Result<Self, Error> {
        let slots = capacity
            .checked_mul(GROUP_SIZE)
            .filter(|_| capacity != 0)
            .ok_or(Error::Overflow)?;
        let points = flags & NO_POINTS == 0;
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
            span_delta: column(aligned_bytes(slots, 1)),
            symbol: column(aligned_bytes(slots, 2)),
            field: column(aligned_bytes(slots, 2)),
            supertype: column(aligned_bytes(slots, 2)),
            last: column(bit_bytes(slots)),
            start_point_base: column(if points {
                aligned_bytes(capacity, 8)
            } else {
                0
            }),
            start_point: column(if points { aligned_bytes(slots, 2) } else { 0 }),
            end_point_base: column(if points {
                aligned_bytes(capacity, 8)
            } else {
                0
            }),
            end_point: column(if points { aligned_bytes(slots, 2) } else { 0 }),
            extra: column(if flags & EXTRAS != 0 {
                bit_bytes(slots)
            } else {
                0
            }),
            missing: column(if flags & MISSING != 0 {
                bit_bytes(slots)
            } else {
                0
            }),
            error: column(if flags & ERRORS != 0 {
                bit_bytes(capacity)
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

    fn columns(self, groups: u32, flags: u32) -> [(SlabOffset, usize); 19] {
        let slots = groups * GROUP_SIZE;
        let points = flags & NO_POINTS == 0;
        [
            (self.waste, aligned_bytes(groups, 2) as usize),
            (self.start_byte_base, aligned_bytes(groups, 4) as usize),
            (self.start_byte_delta, aligned_bytes(slots, 1) as usize),
            (self.end_byte_base, aligned_bytes(groups, 4) as usize),
            (self.end_byte_delta, aligned_bytes(slots, 2) as usize),
            (self.span_base, aligned_bytes(groups, 4) as usize),
            (self.span_delta, aligned_bytes(slots, 1) as usize),
            (self.symbol, aligned_bytes(slots, 2) as usize),
            (self.field, aligned_bytes(slots, 2) as usize),
            (self.supertype, aligned_bytes(slots, 2) as usize),
            (self.last, bit_bytes(slots) as usize),
            (
                self.start_point_base,
                if points {
                    aligned_bytes(groups, 8) as usize
                } else {
                    0
                },
            ),
            (
                self.start_point,
                if points {
                    aligned_bytes(slots, 2) as usize
                } else {
                    0
                },
            ),
            (
                self.end_point_base,
                if points {
                    aligned_bytes(groups, 8) as usize
                } else {
                    0
                },
            ),
            (
                self.end_point,
                if points {
                    aligned_bytes(slots, 2) as usize
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
                self.missing,
                if flags & MISSING != 0 {
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

pub(crate) struct TreeData {
    pub grammar: Grammar,
    pub layout: Layout,
    pub bytes: NonNull<u8>,
    pub length: u32,
    // Small final shrinks retain the allocation; deallocation needs its original size.
    allocation_length: u32,
    owned: bool,
}

impl TreeData {
    #[inline]
    pub fn tables(&self) -> &GrammarView {
        self.grammar.tables()
    }

    // Offsets are established by the encoder or loader before a descriptor is published.
    #[inline]
    pub fn byte(&self, offset: SlabOffset, index: u32) -> u8 {
        unsafe {
            *self
                .bytes
                .as_ptr()
                .add(offset.get() as usize + index as usize)
        }
    }

    #[inline]
    pub fn short(&self, offset: SlabOffset, index: u32) -> u16 {
        u16::from_le(unsafe {
            self.bytes
                .as_ptr()
                .add(offset.get() as usize + index as usize * 2)
                .cast::<u16>()
                .read_unaligned()
        })
    }

    #[inline]
    pub fn word(&self, offset: SlabOffset, index: u32) -> u32 {
        u32::from_le(unsafe {
            self.bytes
                .as_ptr()
                .add(offset.get() as usize + index as usize * 4)
                .cast::<u32>()
                .read_unaligned()
        })
    }

    #[inline]
    pub fn long(&self, offset: SlabOffset, index: u32) -> u64 {
        u64::from_le(unsafe {
            self.bytes
                .as_ptr()
                .add(offset.get() as usize + index as usize * 8)
                .cast::<u64>()
                .read_unaligned()
        })
    }

    #[inline]
    pub fn bit(&self, offset: SlabOffset, index: u32) -> bool {
        self.byte(offset, index / 8) & (1 << (index % 8)) != 0
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
        slot - self.word(self.layout.span_base, slot / GROUP_SIZE)
            - self.byte(self.layout.span_delta, slot) as u32
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
        RemappedGrammarKindId(self.grammar_index_raw(slot) as u16)
    }

    fn grammar_index_raw(&self, slot: u32) -> u32 {
        let tables = self.tables();
        let code = self.symbol_code(slot).get() as u32;
        if self.flags() & SEPARATE_GRAMMAR != 0 {
            return self.short(self.layout.grammar, slot) as u32;
        }
        if tables.separate != 0 {
            return code;
        }
        if tables.encoding == 2 {
            return code & 255;
        }
        let selector = code & ((1 << tables.symbol_shift) - 1);
        unsafe {
            if tables.encoding == 1 {
                if selector == 0 {
                    *tables.defaults.add((code >> tables.symbol_shift) as usize) as u32
                } else {
                    *tables.grammar_ids.add(selector as usize) as u32
                }
            } else {
                *tables.grammar_ids.add(code as usize) as u32
            }
        }
    }

    pub fn has_points(&self) -> bool {
        self.flags() & NO_POINTS == 0
    }

    pub fn slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bytes.as_ptr(), self.length as usize) }
    }

    pub(crate) fn put_byte(&mut self, offset: SlabOffset, index: u32, value: u8) {
        unsafe {
            *self
                .bytes
                .as_ptr()
                .add(offset.get() as usize + index as usize) = value;
        }
    }

    pub(crate) fn put_short(&mut self, offset: SlabOffset, index: u32, value: u16) {
        unsafe {
            self.bytes
                .as_ptr()
                .add(offset.get() as usize + index as usize * 2)
                .cast::<u16>()
                .write_unaligned(value.to_le());
        }
    }

    pub(crate) fn put_word(&mut self, offset: SlabOffset, index: u32, value: u32) {
        unsafe {
            self.bytes
                .as_ptr()
                .add(offset.get() as usize + index as usize * 4)
                .cast::<u32>()
                .write_unaligned(value.to_le());
        }
    }

    pub(crate) fn put_long(&mut self, offset: SlabOffset, index: u32, value: u64) {
        unsafe {
            self.bytes
                .as_ptr()
                .add(offset.get() as usize + index as usize * 8)
                .cast::<u64>()
                .write_unaligned(value.to_le());
        }
    }

    pub(crate) fn put_bit(&mut self, offset: SlabOffset, index: u32, value: bool) {
        let mask = 1 << (index % 8);
        self.put_byte(
            offset,
            index / 8,
            (self.byte(offset, index / 8) & !mask) | if value { mask } else { 0 },
        );
    }

    fn presence_size(&self) -> u64 {
        presence_size(self.tables().symbol_count + 2, self.groups())
    }
}

pub(crate) fn presence_size(symbols: u32, groups: u32) -> u64 {
    (bit_bytes(symbols) + symbols as u64 * (groups as u64).div_ceil(32) * 4 + 7) & !7
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
        layout: Layout,
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
            Some(bytes) => unsafe { NonNull::new_unchecked(bytes.as_ptr().cast_mut()) },
            None => unsafe { NonNull::new_unchecked(pointer.as_ptr().add(prefix())) },
        };
        unsafe {
            pointer.cast::<TreeData>().as_ptr().write(TreeData {
                grammar: grammar.clone(),
                layout,
                bytes,
                length,
                allocation_length: length,
                owned: borrowed.is_none(),
            });
        }
        Ok(Self(pointer.cast()))
    }

    pub(crate) fn empty(grammar: &Grammar, capacity: u32, points: bool) -> Result<Self, Error> {
        // Reserve optional columns while packing. Their presence is only known
        // after traversal, when unused columns can be removed together.
        let flags = VERSION
            | WIDE_SUPERTYPES
            | EXTRAS
            | MISSING
            | ERRORS
            | if grammar.tables().separate != 0 {
                SEPARATE_GRAMMAR
            } else {
                0
            }
            | if points { 0 } else { NO_POINTS };
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

    unsafe fn copy_columns(&self, destination: *mut u8, next: Layout, flags: u32, padding: bool) {
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
                    data.bytes.as_ptr().add(offset.get() as usize),
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
            (MISSING, previous.missing, next.missing, next.error),
            (ERRORS, previous.error, next.error, next.grammar),
            (SEPARATE_GRAMMAR, previous.grammar, next.grammar, next.end),
        ] {
            if flags & flag != 0 {
                unsafe {
                    ptr::copy(
                        data.bytes.as_ptr().add(source.get() as usize),
                        data.bytes.as_ptr().add(destination.get() as usize),
                        (end.get() - destination.get()) as usize,
                    );
                }
            }
        }
        data.layout = next;
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
            data.bytes = unsafe { NonNull::new_unchecked(pointer.cast::<u8>().add(prefix())) };
            data.allocation_length = length;
        }
        Ok(())
    }

    pub fn repack(&self) -> Result<Self, Error> {
        let layout = Layout::new(self.group_count(), self.data().flags())?;
        let result = Self::allocate(
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
    pub fn detach(&self) -> Result<Tree, Error> {
        Tree::from_bytes_safety_checked(&self.data().grammar, self.as_bytes())
    }
}

#[derive(Default)]
pub(crate) struct PresenceScratch {
    counts: Vec<u32>,
    bitmap: Vec<u8>,
}

impl PresenceScratch {
    pub fn trim(&mut self) {
        *self = Self::default();
    }
}

impl Tree {
    pub(crate) fn build_presence(&mut self, scratch: &mut PresenceScratch) {
        let data = self.data_mut();
        let groups = data.groups();
        if groups <= 32 {
            return;
        }
        let symbols = data.tables().symbol_count + 2;
        // Rare symbols store descending slot IDs in the same space as a group
        // bitmap. Convert once the occurrence list no longer fits.
        let entry_bytes = groups.div_ceil(32) * 4;
        let entry_slots = entry_bytes / 4;
        scratch.counts.resize(symbols as usize, 0);
        scratch.counts.fill(0);
        scratch.bitmap.resize(entry_bytes as usize, 0);
        let offset = data.layout.end;
        let entries = offset + bit_bytes(symbols) as u32;
        unsafe {
            ptr::write_bytes(
                data.bytes.as_ptr().add(offset.get() as usize),
                0,
                data.presence_size() as usize,
            );
            ptr::write_bytes(
                data.bytes.as_ptr().add(entries.get() as usize),
                255,
                symbols as usize * entry_bytes as usize,
            );
        }
        data.put_word(SlabOffset(0), 0, data.flags() | PRESENCE);
        for group in (0..groups).rev() {
            for slot in (group * GROUP_SIZE..data.group_end(group)).rev() {
                let symbol = u32::from(data.symbol_index(slot).get());
                let entry = entries + symbol * entry_bytes;
                let count = &mut scratch.counts[symbol as usize];
                if *count > entry_slots {
                    data.put_bit(entry, group, true);
                } else if *count < entry_slots {
                    data.put_word(entry, *count, slot);
                    *count += 1;
                } else {
                    scratch.bitmap.fill(0);
                    for index in 0..entry_slots {
                        let previous = data.word(entry, index) / GROUP_SIZE;
                        scratch.bitmap[previous as usize / 8] |= 1 << (previous % 8);
                    }
                    scratch.bitmap[group as usize / 8] |= 1 << (group % 8);
                    unsafe {
                        ptr::copy_nonoverlapping(
                            scratch.bitmap.as_ptr(),
                            data.bytes.as_ptr().add(entry.get() as usize),
                            entry_bytes as usize,
                        );
                    }
                    data.put_bit(offset, symbol, true);
                    *count = entry_slots + 1;
                }
            }
        }
    }

    pub fn group_has_symbol(&self, group: u32, symbol: KindId) -> bool {
        self.data().group_has_symbol(group, symbol)
    }
}

impl TreeData {
    pub fn group_has_symbol(&self, group: u32, symbol: KindId) -> bool {
        let symbol = u32::from(self.tables().remap_kind(symbol).get());
        if group >= self.groups() || symbol >= self.tables().symbol_count + 2 {
            return false;
        }
        if self.flags() & PRESENCE == 0 {
            return (group * GROUP_SIZE..self.group_end(group))
                .any(|slot| u32::from(self.symbol_index(slot).get()) == symbol);
        }
        let entry_bytes = self.groups().div_ceil(32) * 4;
        let entry = self.layout.end
            + bit_bytes(self.tables().symbol_count + 2) as u32
            + symbol * entry_bytes;
        if self.bit(self.layout.end, symbol) {
            return self.bit(entry, group);
        }
        for index in 0..entry_bytes / 4 {
            let slot = self.word(entry, index);
            if slot == u32::MAX || slot / GROUP_SIZE < group {
                break;
            }
            if slot / GROUP_SIZE == group {
                return true;
            }
        }
        false
    }
}

impl Tree {
    fn load(grammar: &Grammar, bytes: &[u8], borrowed: bool, full: bool) -> Result<Self, Error> {
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
        if flags & !(NO_POINTS | PRESENCE | WIDE_SUPERTYPES | OPTIONAL) != VERSION
            || groups == 0
            || groups > capacity
            || flags & WIDE_SUPERTYPES == 0
            || (flags & MISSING != 0 && flags & ERRORS == 0)
            || (flags & SEPARATE_GRAMMAR != 0 && grammar.tables().separate == 0)
            || header(3) != grammar.tables().dictionary_count
        {
            return Err(Error::InvalidSlab);
        }
        let layout = Layout::new(capacity, flags).map_err(|_| Error::InvalidSlab)?;
        let trailing = if flags & PRESENCE != 0 {
            if groups <= 32 {
                return Err(Error::InvalidSlab);
            }
            presence_size(grammar.tables().symbol_count + 2, groups)
        } else {
            0
        };
        if layout.end.get() as u64 + trailing != bytes.len() as u64 {
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
        // Safety-only loading may trust presence hints, but never node spans or
        // table indexes: those determine subsequent unchecked memory accesses.
        if full && flags & PRESENCE != 0 {
            tree.validate_presence()?;
        }
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
            let start_point = if data.has_points() {
                data.long(data.layout.start_point_base, group)
            } else {
                0
            };
            let end_point = if data.has_points() {
                data.long(data.layout.end_point_base, group)
            } else {
                0
            };
            for slot in (group * GROUP_SIZE..data.group_end(group)).rev() {
                while ends.last().is_some_and(|end| *end > slot) {
                    ends.pop();
                }
                let span = span_base + data.byte(data.layout.span_delta, slot) as u64;
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
                if data.has_points() {
                    let start_delta = data.short(data.layout.start_point, slot) as u64;
                    let end_delta = data.short(data.layout.end_point, slot) as u64;
                    let start_row = (start_point >> 32) + (start_delta >> 8);
                    let start_column = (start_point & 0xffff_ffff) + (start_delta & 255);
                    let end_row = (end_point >> 32)
                        .checked_sub(end_delta >> 8)
                        .ok_or(Error::InvalidSlab)?;
                    let end_column = (end_point & 0xffff_ffff)
                        .checked_sub(end_delta & 255)
                        .ok_or(Error::InvalidSlab)?;
                    if start_row > u32::MAX as u64
                        || start_column > u32::MAX as u64
                        || (start_row, start_column) > (end_row, end_column)
                    {
                        return Err(Error::InvalidSlab);
                    }
                }
                ends.push(end);
            }
        }
        Ok(())
    }

    fn validate_presence(&self) -> Result<(), Error> {
        let data = self.data();
        let symbols = data.tables().symbol_count + 2;
        let entry_bytes = data.groups().div_ceil(32) * 4;
        let entry_slots = entry_bytes / 4;
        let modes = data.layout.end;
        let mode_bytes = bit_bytes(symbols) as u32;
        let entries = modes + mode_bytes;
        let mut local = [[0u32; 3]; 256];
        let mut allocated = if symbols > 256 {
            vec![[0u32; 3]; symbols as usize]
        } else {
            Vec::new()
        };
        let counts = if symbols <= 256 {
            &mut local[..symbols as usize]
        } else {
            &mut allocated[..]
        };
        for group in (0..data.groups()).rev() {
            for slot in (group * GROUP_SIZE..data.group_end(group)).rev() {
                let symbol = u32::from(data.symbol_index(slot).get());
                let entry = entries + symbol * entry_bytes;
                let count = &mut counts[symbol as usize];
                if data.bit(modes, symbol) {
                    if !data.bit(entry, group) {
                        return Err(Error::InvalidSlab);
                    }
                    if count[0] == 0 || count[2] != group {
                        count[1] += 1;
                        count[2] = group;
                    }
                } else if count[0] >= entry_slots || data.word(entry, count[0]) != slot {
                    return Err(Error::InvalidSlab);
                }
                count[0] += 1;
            }
        }
        for symbol in 0..symbols {
            let count = counts[symbol as usize];
            let entry = entries + symbol * entry_bytes;
            let bitmap = data.bit(modes, symbol);
            if bitmap != (count[0] > entry_slots) {
                return Err(Error::InvalidSlab);
            }
            if bitmap {
                if (0..entry_slots)
                    .map(|index| data.word(entry, index).count_ones())
                    .sum::<u32>()
                    != count[1]
                {
                    return Err(Error::InvalidSlab);
                }
            } else {
                for index in count[0]..entry_slots {
                    if data.word(entry, index) != u32::MAX {
                        return Err(Error::InvalidSlab);
                    }
                }
            }
        }
        if symbols % 64 != 0 && data.long(modes + mode_bytes - 8, 0) >> (symbols % 64) != 0 {
            return Err(Error::InvalidSlab);
        }
        for index in (mode_bytes as u64 + symbols as u64 * entry_bytes as u64)..data.presence_size()
        {
            if data.byte(modes, index as u32) != 0 {
                return Err(Error::InvalidSlab);
            }
        }
        Ok(())
    }
}
