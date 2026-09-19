//! Typed scans over stored columns. No decoded column arrays are retained.
//!
//! ```
//! # fn example(root: tree_squatter::Node<'_>, kinds: &tree_squatter::KindSet) {
//! let matches = root.all().overlapping_bytes(10..100).filter_kind_ids(kinds);
//! for node in matches.nodes() {
//!     println!("{}", node.kind());
//! }
//! let backwards = root.postorder().rev().nodes();
//! # }
//! ```
//! Scans and returned groups borrow the tree, not the iterator:
//!
//! ```compile_fail
//! # fn example(tree: tree_squatter::Tree) {
//! let group = tree.root_node().all().groups().next().unwrap();
//! drop(tree);
//! let _ = group.nodes().next();
//! # }
//! ```
use crate::{KindSet, Node, RawNode};
use std::{ffi::c_void, iter::FusedIterator, marker::PhantomData, ops::Range};

#[derive(Clone, Copy)]
#[repr(C)]
struct RawColumns {
    data: *const u8,
    supertypes: *const u16,
    supertype_masks: *const u64,
    size: u32,
    supertype_count: u32,
    supertype_mask_count: u32,
    layout: ColumnLayout,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct ColumnLayout {
    group_shift: u32,
    symbol_count: u32,
    symbol_shift: u32,
    waste: u32,
    span_base: u32,
    span_delta: u32,
    start_byte_base: u32,
    start_byte_delta: u32,
    end_byte_base: u32,
    end_byte_delta: u32,
    symbol: u32,
    field: u32,
    supertype: u32,
    extra: u32,
    missing: u32,
}

#[derive(Clone, Copy)]
struct Columns<'tree> {
    layout: ColumnLayout,
    data: &'tree [u8],
    supertypes: &'tree [u16],
    supertype_masks: &'tree [u64],
    root: Node<'tree>,
}
impl<'tree> Columns<'tree> {
    fn new(root: Node<'tree>) -> Self {
        unsafe extern "C" {
            fn sq_tree_scan_columns(tree: *const c_void, columns: *mut RawColumns);
        }
        let mut raw = std::mem::MaybeUninit::uninit();
        // The bridge initializes every field. Slab and grammar tables are immutable
        // and retained by the tree borrowed by root, including externally owned slabs.
        let raw = unsafe {
            sq_tree_scan_columns(root.raw.tree, raw.as_mut_ptr());
            raw.assume_init()
        };
        // Empty grammar tables may have null pointers, which cannot form slices.
        unsafe {
            Self {
                layout: raw.layout,
                data: std::slice::from_raw_parts(raw.data, raw.size as usize),
                supertypes: if raw.supertype_count == 0 {
                    &[]
                } else {
                    std::slice::from_raw_parts(raw.supertypes, raw.supertype_count as usize)
                },
                supertype_masks: if raw.supertype_mask_count == 0 {
                    &[]
                } else {
                    std::slice::from_raw_parts(
                        raw.supertype_masks,
                        raw.supertype_mask_count as usize
                            * (raw.supertype_count as usize).div_ceil(64),
                    )
                },
                root,
            }
        }
    }
    #[inline]
    fn group_size(self) -> u32 {
        1 << self.layout.group_shift
    }
    #[inline]
    fn byte(self, offset: u32, index: u32) -> u8 {
        self.data[offset as usize + index as usize]
    }
    #[inline]
    fn short(self, offset: u32, index: u32) -> u16 {
        let offset = offset as usize + index as usize * 2;
        u16::from_le_bytes(self.data[offset..offset + 2].try_into().unwrap())
    }
    #[inline]
    fn word(self, offset: u32, index: u32) -> u32 {
        let offset = offset as usize + index as usize * 4;
        u32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap())
    }
    #[inline]
    fn group(self, index: u32) -> GroupRef<'tree> {
        GroupRef {
            columns: self,
            index,
        }
    }
    #[inline]
    fn first_slot(self, slot: u32) -> u32 {
        slot - self.word(self.layout.span_base, slot >> self.layout.group_shift)
            - u32::from(self.byte(self.layout.span_delta, slot))
    }
    #[inline]
    fn previous_slot(self, slot: u32) -> Option<u32> {
        let previous = slot.checked_sub(1)?;
        // Slots and subtree boundaries have a live predecessor within a group;
        // only crossing a physical group boundary requires skipping its waste.
        if slot & (self.group_size() - 1) != 0 {
            Some(previous)
        } else {
            Some(
                previous
                    - u32::from(self.short(self.layout.waste, previous >> self.layout.group_shift)),
            )
        }
    }
    #[inline]
    fn node(self, slot: u32) -> Node<'tree> {
        Node {
            raw: RawNode {
                tree: self.root.raw.tree,
                slot,
            },
            lifetime: std::marker::PhantomData,
        }
    }
}

/// Bits address physical slots within a group; unused slots are always clear.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Mask(u64);
impl Mask {
    pub fn bits(self) -> u64 {
        self.0
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn count_ones(self) -> u32 {
        self.0.count_ones()
    }
    pub fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
    pub fn contains(self, slot: u32) -> bool {
        slot < 64 && self.0 & (1u64 << slot) != 0
    }
    fn lower(length: u32) -> Self {
        Self(if length == 64 {
            u64::MAX
        } else {
            (1u64 << length) - 1
        })
    }
    #[inline]
    fn pop(&mut self, descending: bool) -> Option<u32> {
        if self.is_empty() {
            return None;
        }
        let slot = if descending {
            63 - self.0.leading_zeros()
        } else {
            self.0.trailing_zeros()
        };
        self.0 &= !(1u64 << slot);
        Some(slot)
    }
    // Inlining avoids copying captured column metadata for each group.
    #[inline(always)]
    fn retain(self, mut predicate: impl FnMut(u32) -> bool) -> Self {
        let mut remaining = self.0;
        let mut matches = 0;
        while remaining != 0 {
            let slot = remaining.trailing_zeros();
            remaining &= remaining - 1;
            matches |= u64::from(predicate(slot)) << slot;
        }
        Self(matches)
    }
}

/// An immutable physical group, borrowing the tree independently of a scan.
#[derive(Clone, Copy)]
pub struct GroupRef<'tree> {
    columns: Columns<'tree>,
    index: u32,
}
impl<'tree> GroupRef<'tree> {
    pub fn index(self) -> u32 {
        self.index
    }
    pub fn first_slot(self) -> u32 {
        self.index << self.columns.layout.group_shift
    }
    #[inline]
    pub fn valid_mask(self) -> Mask {
        Mask::lower(self.used())
    }
    /// Resolve a group-relative physical slot; waste and out-of-group slots fail.
    pub fn node(self, slot: u32) -> Option<Node<'tree>> {
        (slot < self.used()).then(|| self.columns.node(self.first_slot() + slot))
    }
    #[inline]
    fn used(self) -> u32 {
        self.columns.group_size()
            - u32::from(self.columns.short(self.columns.layout.waste, self.index))
    }
    #[inline]
    fn kind(self, slot: u32) -> u16 {
        let layout = self.columns.layout;
        let symbol = u32::from(self.columns.short(layout.symbol, self.first_slot() + slot))
            >> layout.symbol_shift;
        if symbol == layout.symbol_count - 2 {
            u16::MAX
        } else if symbol == layout.symbol_count - 1 {
            u16::MAX - 1
        } else {
            symbol as u16
        }
    }
    #[inline]
    fn equal_ids(&self, offset: u32, shift: u32, target: u16, candidates: Mask) -> Mask {
        if candidates.0.is_power_of_two() {
            return candidates.retain(|slot| {
                self.columns.short(offset, self.first_slot() + slot) >> shift == target
            });
        }
        let start = offset as usize + self.first_slot() as usize * 2;
        let bytes = &self.columns.data[start..start + self.columns.group_size() as usize * 2];
        let mut matches = 0;
        #[cfg(target_arch = "x86_64")]
        {
            use std::arch::x86_64::*;
            // SSE2 is baseline on x86_64. The checked slice covers both unaligned
            // loads; full groups contain a multiple of 16 little-endian IDs.
            unsafe {
                let target = _mm_set1_epi16(target as i16);
                let shift = _mm_cvtsi32_si128(shift as i32);
                let matching = |bytes: &[u8]| {
                    let low = _mm_loadu_si128(bytes.as_ptr().cast());
                    let high = _mm_loadu_si128(bytes.as_ptr().add(16).cast());
                    let low = _mm_cmpeq_epi16(_mm_srl_epi16(low, shift), target);
                    let high = _mm_cmpeq_epi16(_mm_srl_epi16(high, shift), target);
                    _mm_movemask_epi8(_mm_packs_epi16(low, high)) as u64
                };
                let mut chunks = bytes.chunks_exact(32);
                // A 16-slot group needs no loop-carried mask or chunk offset.
                if let Some(first) = chunks.next() {
                    matches = matching(first);
                }
                for (index, bytes) in chunks.enumerate() {
                    matches |= matching(bytes) << ((index + 1) * 16);
                }
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        for (slot, bytes) in bytes.chunks_exact(2).enumerate() {
            let value = u16::from_le_bytes([bytes[0], bytes[1]]) >> shift;
            matches |= u64::from(value == target) << slot;
        }
        candidates.intersection(Mask(matches))
    }
    #[inline(always)]
    fn equal_id_set<const N: usize>(
        &self,
        offset: u32,
        shift: u32,
        targets: &[u16; N],
        candidates: Mask,
    ) -> Mask {
        if N == 0 {
            return Mask::default();
        }
        if N == 1 {
            return self.equal_ids(offset, shift, targets[0], candidates);
        }
        if N == 2 {
            return Mask(
                self.equal_ids(offset, shift, targets[0], candidates).0
                    | self.equal_ids(offset, shift, targets[1], candidates).0,
            );
        }
        if candidates.0.is_power_of_two() {
            return candidates.retain(|slot| {
                targets.contains(&(self.columns.short(offset, self.first_slot() + slot) >> shift))
            });
        }
        #[cfg(target_arch = "x86_64")]
        {
            use std::arch::x86_64::*;
            let start = offset as usize + self.first_slot() as usize * 2;
            let bytes = &self.columns.data[start..start + self.columns.group_size() as usize * 2];
            let mut matches = 0;
            // SSE2 is baseline. Each checked chunk contains both vector loads;
            // the const-sized target loop can unroll independently of group size.
            unsafe {
                let targets = targets.map(|target| _mm_set1_epi16(target as i16));
                let shift = _mm_cvtsi32_si128(shift as i32);
                for (index, bytes) in bytes.chunks_exact(32).enumerate() {
                    let low = _mm_srl_epi16(_mm_loadu_si128(bytes.as_ptr().cast()), shift);
                    let high = _mm_srl_epi16(_mm_loadu_si128(bytes.as_ptr().add(16).cast()), shift);
                    let mut low_matches = _mm_setzero_si128();
                    let mut high_matches = _mm_setzero_si128();
                    for target in targets {
                        low_matches = _mm_or_si128(low_matches, _mm_cmpeq_epi16(low, target));
                        high_matches = _mm_or_si128(high_matches, _mm_cmpeq_epi16(high, target));
                    }
                    matches |= (_mm_movemask_epi8(_mm_packs_epi16(low_matches, high_matches))
                        as u64)
                        << (index * 16);
                }
            }
            candidates.intersection(Mask(matches))
        }
        #[cfg(not(target_arch = "x86_64"))]
        candidates.retain(|slot| {
            targets.contains(&(self.columns.short(offset, self.first_slot() + slot) >> shift))
        })
    }
    #[inline]
    fn bitmap(self, offset: u32) -> u64 {
        if offset == 0 {
            return 0;
        }
        let mut bits = 0;
        for byte in 0..self.columns.group_size() / 8 {
            bits |=
                u64::from(self.columns.byte(offset, self.first_slot() / 8 + byte)) << (byte * 8);
        }
        // Predicates intersect these bits with an already-valid candidate mask.
        bits
    }
}

/// An ordered fragment of a physical group. Postorder may revisit the same group.
#[derive(Clone, Copy)]
pub struct GroupMatches<'tree> {
    group: GroupRef<'tree>,
    matches: Mask,
    descending: bool,
}
impl<'tree> GroupMatches<'tree> {
    pub fn group(self) -> GroupRef<'tree> {
        self.group
    }
    pub fn matches(self) -> Mask {
        self.matches
    }
    #[inline]
    pub fn nodes(self) -> GroupNodes<'tree> {
        GroupNodes {
            base: self.group.columns.node(self.group.first_slot()),
            matches: self.matches,
            descending: self.descending,
        }
    }
}

pub struct GroupNodes<'tree> {
    base: Node<'tree>,
    matches: Mask,
    descending: bool,
}
impl<'tree> GroupNodes<'tree> {
    #[inline]
    fn pop(&mut self, back: bool) -> Option<Node<'tree>> {
        self.matches.pop(self.descending ^ back).map(|slot| {
            let mut node = self.base;
            node.raw.slot += slot;
            node
        })
    }
}
impl<'tree> Iterator for GroupNodes<'tree> {
    type Item = Node<'tree>;
    #[inline]
    fn fold<B, F>(mut self, mut accumulator: B, mut fold: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        while let Some(node) = self.pop(false) {
            accumulator = fold(accumulator, node);
        }
        accumulator
    }
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.pop(false)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len(), Some(self.len()))
    }
    fn count(self) -> usize {
        self.len()
    }
}
impl DoubleEndedIterator for GroupNodes<'_> {
    #[inline]
    fn rfold<B, F>(mut self, mut accumulator: B, mut fold: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        while let Some(node) = self.pop(true) {
            accumulator = fold(accumulator, node);
        }
        accumulator
    }
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.pop(true)
    }
}
impl ExactSizeIterator for GroupNodes<'_> {
    fn len(&self) -> usize {
        self.matches.count_ones() as usize
    }
}
impl FusedIterator for GroupNodes<'_> {}

mod sealed {
    pub trait Source {}
    pub trait Predicate {}
    pub trait IdSelection {}
}

/// Internal protocol exposed for generic scan consumers. Implementations are sealed.
pub trait GroupScan<'tree>: sealed::Source + Sized {
    type Reversed: GroupScan<'tree, Reversed = Self>;
    type Slots: Iterator<Item = u32> + ExactSizeIterator;
    const DESCENDING: bool;

    /// Iterate a mask produced by this source, or an empty mask, in traversal order.
    fn slots(matches: Mask) -> Self::Slots;

    /// Change direction before consuming the scan.
    fn reverse(self) -> Self::Reversed;
    /// Metadata for the current fragment, or the root before iteration starts.
    fn group(&self) -> &GroupRef<'tree>;
    /// Advance the current group and return its nonempty matching mask.
    fn next_mask(&mut self) -> Option<Mask>;
    /// Advance without constructing a mask when the source can produce slots directly.
    #[inline(always)]
    fn next_slots(&mut self) -> Option<Self::Slots> {
        self.next_mask().map(Self::slots)
    }
    #[inline]
    fn count(self) -> usize {
        self.count_matches(Identity)
    }
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        count_groups(self, predicate)
    }
}
#[inline(always)]
fn count_groups<'tree, S: GroupScan<'tree>, P: Predicate>(mut source: S, predicate: P) -> usize {
    let mut count = 0;
    while let Some(matches) = source.next_mask() {
        count += predicate
            .retain_matches(source.group(), matches)
            .count_ones() as usize;
    }
    count
}

/// A typed pipeline. Select ranges before filters, then consume nodes or groups.
pub struct Scan<'tree, S> {
    source: S,
    lifetime: PhantomData<&'tree crate::Tree>,
}
impl<'tree, S> Scan<'tree, S> {
    fn new(source: S) -> Self {
        Self {
            source,
            lifetime: PhantomData,
        }
    }
}
impl<'tree, S: GroupScan<'tree>> Scan<'tree, S> {
    pub fn nodes(self) -> Nodes<'tree, S> {
        Nodes {
            base: 0,
            source: self.source,
            slots: S::slots(Mask::default()),
            lifetime: PhantomData,
        }
    }
    pub fn groups(self) -> Groups<'tree, S> {
        Groups(self.source, PhantomData)
    }
    /// Count matching nodes without constructing handles.
    pub fn count(self) -> usize {
        self.source.count()
    }
    /// Reverse this scan before calling `nodes()` or `groups()`.
    /// The resulting type retains only the selected traversal's state.
    pub fn rev(self) -> Scan<'tree, S::Reversed> {
        Scan::new(self.source.reverse())
    }
    /// Match public kind IDs. Arrays preserve their length for kernel specialization;
    /// borrowed `KindSet`s support dynamically sized sets.
    pub fn filter_kind_ids<K: IdSelection>(
        self,
        kinds: K,
    ) -> Scan<'tree, Filtered<S, K::KindPredicate>> {
        self.filtered(kinds.into_kind_predicate())
    }
    /// Zero matches nodes with no field, including the tree root.
    pub fn filter_field_id(self, field: u16) -> Scan<'tree, Filtered<S, FieldId>> {
        self.filtered(FieldId(field))
    }
    /// Match any selected field ID. Zero includes nodes with no field; an empty
    /// selection matches nothing. Arrays specialize the kernel for their length.
    pub fn filter_field_ids<F: IdSelection>(
        self,
        fields: F,
    ) -> Scan<'tree, Filtered<S, F::FieldPredicate>> {
        self.filtered(fields.into_field_predicate())
    }
    pub fn filter_supertype_id(self, supertype: u16) -> Scan<'tree, Filtered<S, SupertypeId>> {
        self.filtered(SupertypeId {
            symbol: supertype,
            index: None,
        })
    }
    pub fn filter_extra(self, value: bool) -> Scan<'tree, Filtered<S, Extra>> {
        self.filtered(Extra(value))
    }
    pub fn filter_missing(self, value: bool) -> Scan<'tree, Filtered<S, Missing>> {
        self.filtered(Missing(value))
    }
    fn filtered<P: Predicate>(self, mut predicate: P) -> Scan<'tree, Filtered<S, P>> {
        predicate.prepare(self.source.group());
        Scan::new(Filtered {
            source: self.source,
            predicate,
        })
    }
}
impl<'tree, S: GroupScan<'tree>> IntoIterator for Scan<'tree, S> {
    type Item = Node<'tree>;
    type IntoIter = Nodes<'tree, S>;
    fn into_iter(self) -> Self::IntoIter {
        self.nodes()
    }
}

pub struct Groups<'tree, S>(S, PhantomData<&'tree crate::Tree>);
impl<'tree, S: GroupScan<'tree>> Iterator for Groups<'tree, S> {
    type Item = GroupMatches<'tree>;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let matches = self.0.next_mask()?;
        Some(GroupMatches {
            group: *self.0.group(),
            matches,
            descending: S::DESCENDING,
        })
    }
}
impl<'tree, S: GroupScan<'tree>> FusedIterator for Groups<'tree, S> {}

pub struct Nodes<'tree, S: GroupScan<'tree>> {
    source: S,
    base: u32,
    slots: S::Slots,
    lifetime: PhantomData<&'tree crate::Tree>,
}
impl<'tree, S: GroupScan<'tree>> Iterator for Nodes<'tree, S> {
    type Item = Node<'tree>;
    #[inline]
    fn fold<B, F>(mut self, mut accumulator: B, mut fold: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        let columns = self.source.group().columns;
        accumulator = self.slots.fold(accumulator, |accumulator, slot| {
            fold(accumulator, columns.node(self.base + slot))
        });
        while let Some(slots) = self.source.next_slots() {
            let group = self.source.group();
            let base = group.first_slot();
            accumulator = slots.fold(accumulator, |accumulator, slot| {
                fold(accumulator, columns.node(base + slot))
            });
        }
        accumulator
    }
    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(slot) = self.slots.next() {
                return Some(self.source.group().columns.node(self.base + slot));
            }
            self.slots = self.source.next_slots()?;
            let group = self.source.group();
            self.base = group.first_slot();
        }
    }
    fn count(self) -> usize {
        self.slots.len() + self.source.count()
    }
}
impl<'tree, S: GroupScan<'tree>> FusedIterator for Nodes<'tree, S> {}

/// Sparse fragment slots, with extraction direction fixed by the source type.
pub struct MatchingSlots<S> {
    matches: Mask,
    source: PhantomData<fn() -> S>,
}
impl<S> MatchingSlots<S> {
    fn new(matches: Mask) -> Self {
        Self {
            matches,
            source: PhantomData,
        }
    }
}
impl<'tree, S: GroupScan<'tree>> Iterator for MatchingSlots<S> {
    type Item = u32;
    #[inline]
    fn next(&mut self) -> Option<u32> {
        self.matches.pop(S::DESCENDING)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len(), Some(self.len()))
    }
    fn count(self) -> usize {
        self.len()
    }
}
impl<'tree, S: GroupScan<'tree>> ExactSizeIterator for MatchingSlots<S> {
    fn len(&self) -> usize {
        self.matches.count_ones() as usize
    }
}
impl<'tree, S: GroupScan<'tree>> FusedIterator for MatchingSlots<S> {}

/// Contiguous physical groups, descending in logical preorder.
pub struct Preorder<'tree> {
    group: GroupRef<'tree>,
    groups: Range<u32>,
    slots: Range<u32>,
}
impl<'tree> Preorder<'tree> {
    pub(crate) fn scan(root: Node<'tree>) -> Scan<'tree, Self> {
        Scan::new(Self::new(Columns::new(root)))
    }
    fn new(columns: Columns<'tree>) -> Self {
        let root = columns.root;
        let first = columns.first_slot(root.slot());
        Self {
            group: columns.group(root.slot() >> columns.layout.group_shift),
            groups: (first >> columns.layout.group_shift)
                ..(root.slot() >> columns.layout.group_shift) + 1,
            slots: first..root.slot() + 1,
        }
    }
    #[inline]
    fn mask(&self) -> Mask {
        let first = self.slots.start.saturating_sub(self.group.first_slot());
        let end = (self.slots.end - self.group.first_slot()).min(self.group.used());
        Mask(Mask::lower(end).0 & !Mask::lower(first).0)
    }
    #[inline]
    fn next_range<const REVERSE: bool>(&mut self) -> Option<Range<u32>> {
        loop {
            self.group.index = if REVERSE {
                self.groups.next()?
            } else {
                self.groups.next_back()?
            };
            let first = self.slots.start.saturating_sub(self.group.first_slot());
            let end = (self.slots.end - self.group.first_slot()).min(self.group.used());
            if first < end {
                return Some(first..end);
            }
        }
    }
}
impl sealed::Source for Preorder<'_> {}
impl<'tree> GroupScan<'tree> for Preorder<'tree> {
    type Reversed = ReversePreorder<'tree>;
    type Slots = std::iter::Rev<Range<u32>>;
    const DESCENDING: bool = true;
    #[inline]
    fn slots(matches: Mask) -> Self::Slots {
        (matches.0.trailing_zeros()..64 - matches.0.leading_zeros()).rev()
    }
    #[inline]
    fn next_slots(&mut self) -> Option<Self::Slots> {
        // Unfiltered preorder fragments are contiguous; masks add work per node.
        self.next_range::<false>().map(Iterator::rev)
    }
    #[inline]
    fn reverse(self) -> Self::Reversed {
        ReversePreorder(self)
    }
    #[inline]
    fn count(self) -> usize {
        self.groups
            .map(|index| {
                let group = self.group.columns.group(index);
                let first = self.slots.start.saturating_sub(group.first_slot());
                let end = (self.slots.end - group.first_slot()).min(group.used());
                end.saturating_sub(first) as usize
            })
            .sum()
    }
    #[inline]
    fn group(&self) -> &GroupRef<'tree> {
        &self.group
    }
    #[inline(always)]
    fn next_mask(&mut self) -> Option<Mask> {
        loop {
            self.group.index = self.groups.next_back()?;
            let matches = self.mask();
            if !matches.is_empty() {
                return Some(matches);
            }
        }
    }
}

/// Reverse preorder over the same physical group and subtree bounds.
pub struct ReversePreorder<'tree>(Preorder<'tree>);
impl sealed::Source for ReversePreorder<'_> {}
impl<'tree> GroupScan<'tree> for ReversePreorder<'tree> {
    type Reversed = Preorder<'tree>;
    type Slots = Range<u32>;
    const DESCENDING: bool = false;
    #[inline]
    fn slots(matches: Mask) -> Self::Slots {
        matches.0.trailing_zeros()..64 - matches.0.leading_zeros()
    }
    #[inline]
    fn next_slots(&mut self) -> Option<Self::Slots> {
        self.0.next_range::<true>()
    }
    #[inline]
    fn reverse(self) -> Self::Reversed {
        self.0
    }
    #[inline]
    fn count(self) -> usize {
        self.0.count()
    }
    #[inline]
    fn group(&self) -> &GroupRef<'tree> {
        &self.0.group
    }
    #[inline(always)]
    fn next_mask(&mut self) -> Option<Mask> {
        loop {
            self.0.group.index = self.0.groups.next()?;
            let matches = self.0.mask();
            if !matches.is_empty() {
                return Some(matches);
            }
        }
    }
}

/// Topology traversal over reverse-preorder storage; currently singleton fragments.
pub struct Postorder<'tree> {
    group: GroupRef<'tree>,
    traversal: ForwardPostorder,
}
#[derive(Default)]
struct ForwardPostorder {
    ancestors: Vec<(u32, u32)>,
    next: Option<u32>,
    first: u32,
    started: bool,
}
impl ForwardPostorder {
    // Keep topology visible to node consumers so singleton masks can simplify.
    #[inline(always)]
    fn next(&mut self, columns: &Columns<'_>) -> Option<u32> {
        if !self.started {
            self.started = true;
            self.next = Some(columns.root.slot());
            self.first = columns.first_slot(columns.root.slot());
        }
        loop {
            if self
                .ancestors
                .last()
                .is_some_and(|&(_, first)| self.next.is_none_or(|slot| slot < first))
            {
                return self.ancestors.pop().map(|(slot, _)| slot);
            }
            let slot = self.next?;
            self.next = columns
                .previous_slot(slot)
                .filter(|&next| next >= self.first);
            let first = columns.first_slot(slot);
            if first == slot {
                return Some(slot);
            }
            self.ancestors.push((slot, first));
        }
    }
}
impl<'tree> Postorder<'tree> {
    pub(crate) fn scan(root: Node<'tree>) -> Scan<'tree, Self> {
        Scan::new(Self::new(Columns::new(root)))
    }
    fn new(columns: Columns<'tree>) -> Self {
        Self {
            group: columns.group(columns.root.slot() >> columns.layout.group_shift),
            traversal: ForwardPostorder::default(),
        }
    }
}
impl sealed::Source for Postorder<'_> {}
impl<'tree> GroupScan<'tree> for Postorder<'tree> {
    type Reversed = ReversePostorder<'tree>;
    type Slots = MatchingSlots<Self>;
    const DESCENDING: bool = true;
    #[inline]
    fn slots(matches: Mask) -> Self::Slots {
        MatchingSlots::new(matches)
    }
    #[inline]
    fn reverse(self) -> Self::Reversed {
        ReversePostorder {
            group: self.group,
            pending: Vec::new(),
            expand: None,
            started: false,
        }
    }
    #[inline]
    fn count(self) -> usize {
        if !self.traversal.started {
            Preorder::new(self.group.columns).count()
        } else {
            count_groups(self, Identity)
        }
    }
    #[inline]
    fn group(&self) -> &GroupRef<'tree> {
        &self.group
    }
    #[inline]
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        // Counts do not observe order; partial scans must retain their topology.
        if !self.traversal.started {
            Preorder::new(self.group.columns).count_matches(predicate)
        } else {
            count_groups(self, predicate)
        }
    }
    #[inline(always)]
    fn next_mask(&mut self) -> Option<Mask> {
        let slot = self.traversal.next(&self.group.columns)?;
        self.group.index = slot >> self.group.columns.layout.group_shift;
        Some(Mask(1u64 << (slot & (self.group.columns.group_size() - 1))))
    }
}

/// Parents before children, with siblings visited right to left.
pub struct ReversePostorder<'tree> {
    group: GroupRef<'tree>,
    pending: Vec<u32>,
    expand: Option<u32>,
    started: bool,
}
impl sealed::Source for ReversePostorder<'_> {}
impl<'tree> GroupScan<'tree> for ReversePostorder<'tree> {
    type Reversed = Postorder<'tree>;
    type Slots = MatchingSlots<Self>;
    const DESCENDING: bool = false;
    #[inline]
    fn slots(matches: Mask) -> Self::Slots {
        MatchingSlots::new(matches)
    }
    #[inline]
    fn reverse(self) -> Self::Reversed {
        Postorder::new(self.group.columns)
    }
    #[inline]
    fn group(&self) -> &GroupRef<'tree> {
        &self.group
    }
    #[inline]
    fn count(self) -> usize {
        if !self.started {
            Preorder::new(self.group.columns).count()
        } else {
            count_groups(self, Identity)
        }
    }
    #[inline]
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        if !self.started {
            Preorder::new(self.group.columns).count_matches(predicate)
        } else {
            count_groups(self, predicate)
        }
    }
    #[inline(always)]
    fn next_mask(&mut self) -> Option<Mask> {
        let columns = &self.group.columns;
        // Defer expansion until the next call so yielding a parent needs no allocation.
        let slot = if !self.started {
            self.started = true;
            columns.root.slot()
        } else {
            let previous = self.expand.take()?;
            let first = columns.first_slot(previous);
            let child = (first != previous)
                .then(|| columns.previous_slot(previous))
                .flatten()
                .filter(|&child| child >= first);
            if let Some(mut child) = child {
                loop {
                    let boundary = columns.first_slot(child);
                    // The last child's subtree ends at the parent's boundary.
                    // Return it directly; unary paths need no pending stack.
                    if boundary == first {
                        break child;
                    }
                    self.pending.push(child);
                    child = columns.previous_slot(boundary).unwrap();
                }
            } else {
                self.pending.pop()?
            }
        };
        self.expand = Some(slot);
        self.group.index = slot >> columns.layout.group_shift;
        Some(Mask(1u64 << (slot & (columns.group_size() - 1))))
    }
}

/// Sources that still permit range restriction; filters do not implement this trait.
pub trait UnrestrictedScan: sealed::Source {
    fn restrict_bytes(&mut self, range: &Range<usize>);
}
impl UnrestrictedScan for Preorder<'_> {
    fn restrict_bytes(&mut self, range: &Range<usize>) {
        // Start minima decrease with physical group index. This removes only
        // groups wholly beyond the range end, preserving crossing ancestors.
        let mut lower = self.groups.start;
        let mut upper = self.groups.end;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            let columns = &self.group.columns;
            let start = columns.word(columns.layout.start_byte_base, middle) as usize;
            if start >= range.end {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        self.groups.start = lower;
    }
}
impl UnrestrictedScan for Postorder<'_> {
    fn restrict_bytes(&mut self, _: &Range<usize>) {}
}
impl UnrestrictedScan for ReversePreorder<'_> {
    fn restrict_bytes(&mut self, range: &Range<usize>) {
        self.0.restrict_bytes(range);
    }
}
impl UnrestrictedScan for ReversePostorder<'_> {
    fn restrict_bytes(&mut self, _: &Range<usize>) {}
}
impl<'tree, S: UnrestrictedScan> Scan<'tree, S> {
    /// Intersect a nonempty byte range. Zero-width nodes never overlap.
    pub fn overlapping_bytes(mut self, range: Range<usize>) -> Scan<'tree, OverlappingBytes<S>> {
        self.source.restrict_bytes(&range);
        Scan::new(OverlappingBytes {
            source: self.source,
            range,
        })
    }
}
pub type PreorderOverlappingBytes<'tree> = OverlappingBytes<Preorder<'tree>>;
pub struct OverlappingBytes<S> {
    source: S,
    range: Range<usize>,
}
impl<S: sealed::Source> sealed::Source for OverlappingBytes<S> {}
#[inline(always)]
fn overlapping_matches(group: &GroupRef<'_>, candidates: Mask, range: &Range<usize>) -> Mask {
    if range.is_empty() {
        return Mask::default();
    }
    let columns = group.columns;
    let start_base = columns.word(columns.layout.start_byte_base, group.index) as usize;
    let end_base = columns.word(columns.layout.end_byte_base, group.index) as usize;
    if start_base >= range.end || end_base <= range.start {
        return Mask::default();
    }
    let all_start = start_base.saturating_add(255) < range.end;
    let all_end = end_base.saturating_sub(65535) > range.start;
    candidates.retain(|slot| {
        let slot = group.first_slot() + slot;
        let start = start_base + usize::from(columns.byte(columns.layout.start_byte_delta, slot));
        let end = end_base - usize::from(columns.short(columns.layout.end_byte_delta, slot));
        start < end && (all_start || start < range.end) && (all_end || end > range.start)
    })
}

impl<'tree, S: GroupScan<'tree>> GroupScan<'tree> for OverlappingBytes<S> {
    type Reversed = OverlappingBytes<S::Reversed>;
    type Slots = MatchingSlots<Self>;
    const DESCENDING: bool = S::DESCENDING;
    #[inline]
    fn slots(matches: Mask) -> Self::Slots {
        MatchingSlots::new(matches)
    }
    #[inline]
    fn reverse(self) -> Self::Reversed {
        OverlappingBytes {
            source: self.source.reverse(),
            range: self.range,
        }
    }
    #[inline]
    fn count(self) -> usize {
        if self.range.is_empty() {
            return 0;
        }
        self.source.count_matches(ByteRange(self.range))
    }
    #[inline]
    fn group(&self) -> &GroupRef<'tree> {
        self.source.group()
    }
    #[inline]
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        if self.range.is_empty() {
            return 0;
        }
        self.source
            .count_matches(And(ByteRange(self.range), predicate))
    }
    #[inline]
    fn next_mask(&mut self) -> Option<Mask> {
        if self.range.is_empty() {
            return None;
        }
        loop {
            let candidates = self.source.next_mask()?;
            let matches = overlapping_matches(self.source.group(), candidates, &self.range);
            if !matches.is_empty() {
                return Some(matches);
            }
        }
    }
}

pub trait Predicate: sealed::Predicate {
    /// Called once when the predicate is attached to a scan.
    fn prepare(&mut self, _group: &GroupRef<'_>) {}
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask;
}
struct Identity;
impl sealed::Predicate for Identity {}
impl Predicate for Identity {
    #[inline(always)]
    fn retain_matches(&self, _: &GroupRef<'_>, candidates: Mask) -> Mask {
        candidates
    }
}
struct And<P, Q>(P, Q);
impl<P: Predicate, Q: Predicate> sealed::Predicate for And<P, Q> {}
impl<P: Predicate, Q: Predicate> Predicate for And<P, Q> {
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let matches = self.0.retain_matches(group, candidates);
        if matches.is_empty() {
            matches
        } else {
            self.1.retain_matches(group, matches)
        }
    }
}
struct ByteRange(Range<usize>);
impl sealed::Predicate for ByteRange {}
impl Predicate for ByteRange {
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        overlapping_matches(group, candidates, &self.0)
    }
}
pub struct Filtered<S, P> {
    source: S,
    predicate: P,
}
impl<S: sealed::Source, P: Predicate> sealed::Source for Filtered<S, P> {}
impl<'tree, S: GroupScan<'tree>, P: Predicate> GroupScan<'tree> for Filtered<S, P> {
    type Reversed = Filtered<S::Reversed, P>;
    type Slots = MatchingSlots<Self>;
    const DESCENDING: bool = S::DESCENDING;
    #[inline]
    fn slots(matches: Mask) -> Self::Slots {
        MatchingSlots::new(matches)
    }
    #[inline]
    fn reverse(self) -> Self::Reversed {
        Filtered {
            source: self.source.reverse(),
            predicate: self.predicate,
        }
    }
    #[inline]
    fn group(&self) -> &GroupRef<'tree> {
        self.source.group()
    }
    #[inline]
    fn count_matches<Q: Predicate>(self, predicate: Q) -> usize {
        self.source.count_matches(And(self.predicate, predicate))
    }
    #[inline]
    fn next_mask(&mut self) -> Option<Mask> {
        loop {
            let candidates = self.source.next_mask()?;
            let matches = self
                .predicate
                .retain_matches(self.source.group(), candidates);
            if !matches.is_empty() {
                return Some(matches);
            }
        }
    }
}

/// ID selections accepted by scans: fixed arrays or a borrowed `IdSet`.
/// Array lengths specialize the scan even when their IDs are runtime values.
pub trait IdSelection: sealed::IdSelection {
    type KindPredicate: Predicate;
    type FieldPredicate: Predicate;
    fn into_kind_predicate(self) -> Self::KindPredicate;
    fn into_field_predicate(self) -> Self::FieldPredicate;
    fn contains_id(&self, id: u16) -> bool;
    fn is_empty(&self) -> bool;
}
impl sealed::IdSelection for &crate::IdSet {}
impl<'ids> IdSelection for &'ids crate::IdSet {
    type KindPredicate = KindIds<'ids>;
    type FieldPredicate = FieldIds<'ids>;
    #[inline]
    fn into_kind_predicate(self) -> Self::KindPredicate {
        KindIds(KindStrategy::Multiple(self))
    }
    #[inline]
    fn into_field_predicate(self) -> Self::FieldPredicate {
        FieldIds(self)
    }
    #[inline]
    fn contains_id(&self, id: u16) -> bool {
        crate::IdSet::contains(self, id)
    }
    #[inline]
    fn is_empty(&self) -> bool {
        crate::IdSet::is_empty(self)
    }
}
impl<const N: usize> sealed::IdSelection for [u16; N] {}
impl<const N: usize> IdSelection for [u16; N] {
    type KindPredicate = FixedKindIds<N>;
    type FieldPredicate = FixedFieldIds<N>;
    #[inline]
    fn into_kind_predicate(self) -> Self::KindPredicate {
        FixedKindIds {
            ids: self,
            empty: N == 0,
        }
    }
    #[inline]
    fn into_field_predicate(self) -> Self::FieldPredicate {
        FixedFieldIds(self)
    }
    #[inline]
    fn contains_id(&self, id: u16) -> bool {
        self.as_slice().contains(&id)
    }
    #[inline]
    fn is_empty(&self) -> bool {
        N == 0
    }
}
impl<const N: usize> sealed::IdSelection for &[u16; N] {}
impl<const N: usize> IdSelection for &[u16; N] {
    type KindPredicate = FixedKindIds<N>;
    type FieldPredicate = FixedFieldIds<N>;
    #[inline]
    fn into_kind_predicate(self) -> Self::KindPredicate {
        (*self).into_kind_predicate()
    }
    #[inline]
    fn into_field_predicate(self) -> Self::FieldPredicate {
        (*self).into_field_predicate()
    }
    #[inline]
    fn contains_id(&self, id: u16) -> bool {
        self.as_slice().contains(&id)
    }
    #[inline]
    fn is_empty(&self) -> bool {
        N == 0
    }
}

pub struct FixedKindIds<const N: usize> {
    ids: [u16; N],
    empty: bool,
}
impl<const N: usize> sealed::Predicate for FixedKindIds<N> {}
impl<const N: usize> Predicate for FixedKindIds<N> {
    #[inline]
    fn prepare(&mut self, group: &GroupRef<'_>) {
        let layout = group.columns.layout;
        let encode = |kind| match kind {
            u16::MAX => Some((layout.symbol_count - 2) as u16),
            value if value == u16::MAX - 1 => Some((layout.symbol_count - 1) as u16),
            value if u32::from(value) < layout.symbol_count - 2 => Some(value),
            _ => None,
        };
        let Some(first) = self.ids.iter().copied().find_map(encode) else {
            self.empty = true;
            return;
        };
        // Repeating a valid target preserves membership and a fixed comparison
        // count, without needing an impossible u16 sentinel for invalid IDs.
        self.ids = self.ids.map(|kind| encode(kind).unwrap_or(first));
    }
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        if self.empty {
            return Mask::default();
        }
        let layout = group.columns.layout;
        group.equal_id_set(layout.symbol, layout.symbol_shift, &self.ids, candidates)
    }
}

pub struct KindIds<'kinds>(KindStrategy<'kinds>);
enum KindStrategy<'kinds> {
    Empty,
    Single(u16),
    Multiple(&'kinds KindSet),
}
impl sealed::Predicate for KindIds<'_> {}
impl Predicate for KindIds<'_> {
    #[inline]
    fn prepare(&mut self, group: &GroupRef<'_>) {
        let KindStrategy::Multiple(kinds) = self.0 else {
            return;
        };
        let layout = group.columns.layout;
        self.0 = match kinds.ids.as_slice() {
            [] => KindStrategy::Empty,
            &[kind] => match kind {
                u16::MAX => KindStrategy::Single((layout.symbol_count - 2) as u16),
                value if value == u16::MAX - 1 => {
                    KindStrategy::Single((layout.symbol_count - 1) as u16)
                }
                value if u32::from(value) < layout.symbol_count - 2 => KindStrategy::Single(value),
                _ => KindStrategy::Empty,
            },
            _ => KindStrategy::Multiple(kinds),
        };
    }
    // Inlining lets node consumers discard unused group metadata.
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let kinds = match self.0 {
            KindStrategy::Empty => return Mask::default(),
            KindStrategy::Single(target) => {
                let layout = group.columns.layout;
                return group.equal_ids(layout.symbol, layout.symbol_shift, target, candidates);
            }
            KindStrategy::Multiple(kinds) => kinds,
        };
        #[cfg(target_arch = "x86_64")]
        if kinds.ids.len() <= 4 && !candidates.0.is_power_of_two() {
            let layout = group.columns.layout;
            let mut matches = Mask::default();
            for &kind in &kinds.ids {
                let target = match kind {
                    u16::MAX => (layout.symbol_count - 2) as u16,
                    value if value == u16::MAX - 1 => (layout.symbol_count - 1) as u16,
                    value if u32::from(value) < layout.symbol_count - 2 => value,
                    _ => continue,
                };
                matches.0 |= group
                    .equal_ids(layout.symbol, layout.symbol_shift, target, candidates)
                    .0;
            }
            return matches;
        }
        if candidates.0.is_power_of_two() {
            return candidates.retain(|slot| kinds.contains(group.kind(slot)));
        }
        let layout = group.columns.layout;
        let start = layout.symbol as usize + group.first_slot() as usize * 2;
        let bytes = &group.columns.data[start..start + group.used() as usize * 2];
        let mut matches = 0;
        for (slot, bytes) in bytes.chunks_exact(2).enumerate() {
            let symbol = u32::from(u16::from_le_bytes([bytes[0], bytes[1]])) >> layout.symbol_shift;
            let kind = if symbol == layout.symbol_count - 2 {
                u16::MAX
            } else if symbol == layout.symbol_count - 1 {
                u16::MAX - 1
            } else {
                symbol as u16
            };
            matches |= u64::from(kinds.contains(kind)) << slot;
        }
        candidates.intersection(Mask(matches))
    }
}
pub struct FixedFieldIds<const N: usize>([u16; N]);
impl<const N: usize> sealed::Predicate for FixedFieldIds<N> {}
impl<const N: usize> Predicate for FixedFieldIds<N> {
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        group.equal_id_set(group.columns.layout.field, 0, &self.0, candidates)
    }
}
pub struct FieldIds<'ids>(&'ids crate::IdSet);
impl sealed::Predicate for FieldIds<'_> {}
impl Predicate for FieldIds<'_> {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let offset = group.columns.layout.field;
        match self.0.ids.as_slice() {
            [] => Mask::default(),
            &[field] => group.equal_ids(offset, 0, field, candidates),
            fields if fields.len() <= 4 => {
                fields.iter().fold(Mask::default(), |matches, &field| {
                    Mask(matches.0 | group.equal_ids(offset, 0, field, candidates).0)
                })
            }
            _ => candidates.retain(|slot| {
                self.0
                    .contains(group.columns.short(offset, group.first_slot() + slot))
            }),
        }
    }
}
pub struct FieldId(u16);
impl sealed::Predicate for FieldId {}
impl Predicate for FieldId {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        group.equal_ids(group.columns.layout.field, 0, self.0, candidates)
    }
}
pub struct Extra(bool);
impl sealed::Predicate for Extra {}
impl Predicate for Extra {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let flags = group.bitmap(group.columns.layout.extra);
        Mask(candidates.0 & if self.0 { flags } else { !flags })
    }
}
pub struct Missing(bool);
impl sealed::Predicate for Missing {}
impl Predicate for Missing {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let flags = group.bitmap(group.columns.layout.missing);
        Mask(candidates.0 & if self.0 { flags } else { !flags })
    }
}
pub struct SupertypeId {
    symbol: u16,
    index: Option<usize>,
}
impl sealed::Predicate for SupertypeId {}
impl Predicate for SupertypeId {
    #[inline]
    fn prepare(&mut self, group: &GroupRef<'_>) {
        self.index = group.columns.supertypes.binary_search(&self.symbol).ok();
    }
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let Some(index) = self.index else {
            return Mask::default();
        };
        let columns = &group.columns;
        let words = columns.supertypes.len().div_ceil(64);
        candidates.retain(|slot| {
            let value = columns.short(columns.layout.supertype, group.first_slot() + slot);
            if columns.supertypes.len() <= 8 {
                value & (1 << index) != 0
            } else {
                columns.supertype_masks[usize::from(value) * words + index / 64]
                    & (1u64 << (index % 64))
                    != 0
            }
        })
    }
}
