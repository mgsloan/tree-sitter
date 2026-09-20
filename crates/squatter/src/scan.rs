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
//! Range filters reject reversed queries. Empty queries match only for
//! `within_*` and `containing_*`, using inclusive endpoint containment.
//! Overlap includes zero-width nodes at positions inside the half-open range.
//! Point ranges use row/column order; trees without stored points use `(0, byte_offset)`.
//!
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
use std::{
    ffi::c_void,
    iter::FusedIterator,
    marker::PhantomData,
    ops::{Bound, Bound::*, Range, RangeBounds},
};
use tree_sitter::Point;

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
    #[inline]
    fn at_most<const LIMIT: usize>(self) -> bool {
        let mut remaining = self.0;
        for _ in 0..LIMIT {
            remaining &= remaining.wrapping_sub(1);
        }
        remaining == 0
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
    use super::{Bound, GroupRef, Mask};
    use std::ops::RangeBounds;
    pub trait Source {}
    pub trait Predicate {}
    pub trait IdSelection {}

    pub trait Coordinates: Sized {
        type Position: Copy + Ord;
        const MINIMUM: Self::Position;
        const PRUNE_SUBTREES: bool;
        fn new(group: &GroupRef<'_>) -> Self;
        fn start_minimum(&self, group: &GroupRef<'_>) -> Self::Position;
        fn end_before(&self, group: &GroupRef<'_>, bound: Bound<Self::Position>) -> bool;
        fn retain<R: Relation<Self::Position>>(
            &self,
            group: &GroupRef<'_>,
            candidates: impl FnOnce() -> Mask,
            relation: &R,
        ) -> Mask;
    }
    pub trait PositionColumn {
        type Position: Copy + Ord;
        fn minimum(&self) -> Self::Position;
        fn maximum(&self) -> Self::Position;
        fn get(&self, slot: u32) -> Self::Position;
        #[inline]
        fn retain(
            &self,
            candidates: Mask,
            bounds: (Bound<Self::Position>, Bound<Self::Position>),
        ) -> Mask {
            candidates.retain(|slot| bounds.contains(&self.get(slot)))
        }
    }
    pub trait Positions {
        type Position: Copy + Ord;
        type Start: PositionColumn<Position = Self::Position>;
        type End: PositionColumn<Position = Self::Position>;
        fn start(&self) -> Self::Start;
        fn end(&self) -> Self::End;
    }
    pub trait Relation<T: Copy + Ord> {
        type Mapped<U: Copy + Ord>: Relation<U>;
        fn try_map<U: Copy + Ord>(
            &self,
            convert: impl Fn(T) -> Option<U>,
        ) -> Option<Self::Mapped<U>>;
        fn is_empty(&self) -> bool {
            false
        }
        fn start_bounds(&self) -> (Bound<T>, Bound<T>);
        fn end_lower_bound(&self) -> Bound<T>;
        fn retain<P: Positions<Position = T>>(
            &self,
            positions: P,
            candidates: impl FnOnce() -> Mask,
        ) -> Mask;
    }
}
use sealed::{Coordinates, PositionColumn, Positions, Relation};

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
    #[inline]
    fn next_matching<P: Predicate>(&mut self, predicate: &P) -> Option<Mask> {
        loop {
            let candidates = self.next_mask()?;
            let matches = predicate.retain_matches(self.group(), candidates);
            if !matches.is_empty() {
                return Some(matches);
            }
        }
    }
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
    while let Some(matches) = source.next_matching(&predicate) {
        count += matches.count_ones() as usize;
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
    #[inline(always)]
    fn next_matching_group<const REVERSE: bool, const SUBTREES: bool, P: Predicate>(
        &mut self,
        predicate: &P,
    ) -> Option<Mask> {
        loop {
            self.group.index = if REVERSE {
                self.groups.next()?
            } else {
                self.groups.next_back()?
            };
            if SUBTREES && predicate.excludes_subtrees(&self.group) {
                // The last node in preorder occupies the group's first slot.
                // Its descendants end no later, so their whole groups can be skipped.
                let span = self
                    .group
                    .columns
                    .word(self.group.columns.layout.span_base, self.group.index);
                if span != 0 {
                    // The base alone is a conservative span; avoid delta loads
                    // and short jumps when all spans fit in a byte.
                    let end =
                        (self.group.first_slot() - span).div_ceil(self.group.columns.group_size());
                    self.groups.end = self.groups.end.min(end).max(self.groups.start);
                }
                continue;
            }
            let matches = predicate.retain_group(&self.group, || self.mask());
            if !matches.is_empty() {
                return Some(matches);
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
    fn next_matching<P: Predicate>(&mut self, predicate: &P) -> Option<Mask> {
        if predicate.has_subtree_bound() {
            self.next_matching_group::<false, true, _>(predicate)
        } else {
            self.next_matching_group::<false, false, _>(predicate)
        }
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
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        self.0.count_matches(predicate)
    }
    #[inline]
    fn group(&self) -> &GroupRef<'tree> {
        &self.0.group
    }
    #[inline(always)]
    fn next_matching<P: Predicate>(&mut self, predicate: &P) -> Option<Mask> {
        self.0.next_matching_group::<true, false, _>(predicate)
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

/// Byte-offset coordinates.
pub struct Bytes;

/// Row/column coordinates, using `(0, byte_offset)` when points are not stored.
#[repr(C)]
pub struct Points {
    start_base: u32,
    start_delta: u32,
    end_base: u32,
    end_delta: u32,
}

struct BytePositions<'group, 'tree>(&'group GroupRef<'tree>);
struct PointPositions<'group, 'tree, const STORED: bool> {
    group: &'group GroupRef<'tree>,
    layout: &'group Points,
}
struct ByteColumn<'tree, const END: bool> {
    base: usize,
    deltas: ColumnDeltas<'tree>,
}
struct PointColumn<'tree, const END: bool, const STORED: bool> {
    base: u64,
    deltas: ColumnDeltas<'tree>,
}

#[derive(Clone, Copy)]
struct ColumnDeltas<'tree> {
    data: &'tree [u8],
    start: usize,
    length: usize,
}
impl<'tree> ColumnDeltas<'tree> {
    #[inline]
    fn slice(self) -> &'tree [u8] {
        // Rejected groups need only their bases, not delta-slice bounds checks.
        &self.data[self.start..self.start + self.length]
    }
}

#[inline]
fn column_deltas<'tree>(group: &GroupRef<'tree>, offset: u32, width: usize) -> ColumnDeltas<'tree> {
    ColumnDeltas {
        data: group.columns.data,
        start: offset as usize + group.first_slot() as usize * width,
        length: group.columns.group_size() as usize * width,
    }
}
#[inline]
fn point_base(group: &GroupRef<'_>, offset: u32) -> u64 {
    let offset = offset as usize + group.index as usize * 8;
    u64::from_le_bytes(group.columns.data[offset..offset + 8].try_into().unwrap())
}
#[inline]
fn point_from_key(key: u64) -> Point {
    Point::new((key >> 32) as usize, (key as u32) as usize)
}
#[inline]
fn point_key(point: Point) -> Option<u64> {
    Some(
        (u64::from(u32::try_from(point.row).ok()?) << 32)
            | u64::from(u32::try_from(point.column).ok()?),
    )
}

#[inline]
fn byte_cutoff(base: u64, position: u64, inclusive: bool, limit: u32) -> u32 {
    position.checked_sub(base).map_or(0, |delta| {
        delta
            .saturating_add(u64::from(inclusive))
            .min(u64::from(limit)) as u32
    })
}

#[inline]
fn point_cutoff(base: u64, position: u64, inclusive: bool) -> u32 {
    let Some(row) = (position >> 32).checked_sub(base >> 32) else {
        return 0;
    };
    if row > 255 {
        return 65536;
    }
    // A query column outside this row's encoded interval selects all or none
    // of that row, while earlier delta rows still qualify.
    let columns = (i64::from(position as u32) - i64::from(base as u32) + i64::from(inclusive))
        .clamp(0, 256) as u32;
    (row as u32 * 256) + columns
}

#[inline]
fn delta_bounds<T: Copy, const END: bool>(
    bounds: (Bound<T>, Bound<T>),
    limit: u32,
    cutoff: impl Fn(T, bool) -> u32,
) -> Range<u32> {
    let lower = match bounds.0 {
        Included(position) => cutoff(position, false),
        Excluded(position) => cutoff(position, true),
        Unbounded => {
            if END {
                limit
            } else {
                0
            }
        }
    };
    let upper = match bounds.1 {
        Included(position) => cutoff(position, true),
        Excluded(position) => cutoff(position, false),
        Unbounded => {
            if END {
                0
            } else {
                limit
            }
        }
    };
    if END { upper..lower } else { lower..upper }
}

#[inline]
fn retain_deltas<const WIDE: bool>(
    deltas: ColumnDeltas<'_>,
    candidates: Mask,
    bounds: Range<u32>,
) -> Mask {
    if bounds.is_empty() || candidates.is_empty() {
        return Mask::default();
    }
    if bounds.start == 0 && bounds.end == if WIDE { 65536 } else { 256 } {
        return candidates;
    }
    let deltas = deltas.slice();
    #[cfg(target_arch = "x86_64")]
    {
        let remaining = candidates.0 & (candidates.0 - 1);
        if remaining != 0 && !remaining.is_power_of_two() {
            use std::arch::x86_64::*;
            let mut matches = 0;
            let length = bounds.end - bounds.start;
            // Bias the wrapped delta-minus-lower by the sign bit so signed SIMD
            // comparisons implement an unsigned interval test. Checked chunks
            // cover complete groups; candidate clipping excludes waste lanes.
            unsafe {
                if WIDE {
                    let lower = _mm_set1_epi16((bounds.start as u16 ^ 0x8000) as i16);
                    let upper = _mm_set1_epi16((length as u16 ^ 0x8000) as i16);
                    let equal = _mm_set1_epi16(bounds.start as i16);
                    let matching = |values| {
                        if length == 1 {
                            _mm_cmpeq_epi16(values, equal)
                        } else {
                            _mm_cmpgt_epi16(upper, _mm_sub_epi16(values, lower))
                        }
                    };
                    for (index, bytes) in deltas.chunks_exact(32).enumerate() {
                        let low = matching(_mm_loadu_si128(bytes.as_ptr().cast()));
                        let high = matching(_mm_loadu_si128(bytes.as_ptr().add(16).cast()));
                        matches |=
                            (_mm_movemask_epi8(_mm_packs_epi16(low, high)) as u64) << (index * 16);
                    }
                } else {
                    let lower = _mm_set1_epi8((bounds.start as u8 ^ 0x80) as i8);
                    let upper = _mm_set1_epi8((length as u8 ^ 0x80) as i8);
                    let equal = _mm_set1_epi8(bounds.start as i8);
                    for (index, bytes) in deltas.chunks_exact(16).enumerate() {
                        let values = _mm_loadu_si128(bytes.as_ptr().cast());
                        let selected = if length == 1 {
                            _mm_cmpeq_epi8(values, equal)
                        } else {
                            _mm_cmpgt_epi8(upper, _mm_sub_epi8(values, lower))
                        };
                        matches |= (_mm_movemask_epi8(selected) as u64) << (index * 16);
                    }
                }
            }
            return candidates.intersection(Mask(matches));
        }
    }
    candidates.retain(|slot| {
        let delta = if WIDE {
            let offset = slot as usize * 2;
            u32::from(u16::from_le_bytes(
                deltas[offset..offset + 2].try_into().unwrap(),
            ))
        } else {
            u32::from(deltas[slot as usize])
        };
        bounds.contains(&delta)
    })
}

impl<const END: bool> PositionColumn for ByteColumn<'_, END> {
    type Position = usize;
    #[inline]
    fn minimum(&self) -> usize {
        if END {
            self.base.saturating_sub(65535)
        } else {
            self.base
        }
    }
    #[inline]
    fn maximum(&self) -> usize {
        if END {
            self.base
        } else {
            self.base.saturating_add(255)
        }
    }
    #[inline]
    fn get(&self, slot: u32) -> usize {
        let deltas = self.deltas.slice();
        if END {
            let offset = slot as usize * 2;
            self.base
                - usize::from(u16::from_le_bytes(
                    deltas[offset..offset + 2].try_into().unwrap(),
                ))
        } else {
            self.base + usize::from(deltas[slot as usize])
        }
    }
    #[inline(always)]
    fn retain(&self, candidates: Mask, bounds: (Bound<usize>, Bound<usize>)) -> Mask {
        let limit = if END { 65536 } else { 256 };
        let bounds = delta_bounds::<_, END>(bounds, limit, |position, inclusive| {
            if END {
                byte_cutoff(position as u64, self.base as u64, !inclusive, limit)
            } else {
                byte_cutoff(self.base as u64, position as u64, inclusive, limit)
            }
        });
        retain_deltas::<END>(self.deltas, candidates, bounds)
    }
}
impl<const END: bool, const STORED: bool> PositionColumn for PointColumn<'_, END, STORED> {
    type Position = u64;
    #[inline]
    fn minimum(&self) -> u64 {
        if END {
            self.base
                .saturating_sub(if STORED { (255 << 32) | 255 } else { 65535 })
        } else {
            self.base
        }
    }
    #[inline]
    fn maximum(&self) -> u64 {
        if END {
            self.base
        } else {
            self.base
                .saturating_add(if STORED { (255 << 32) | 255 } else { 255 })
        }
    }
    #[inline]
    fn get(&self, slot: u32) -> u64 {
        let deltas = self.deltas.slice();
        let delta = if STORED || END {
            let offset = slot as usize * 2;
            u64::from(u16::from_le_bytes(
                deltas[offset..offset + 2].try_into().unwrap(),
            ))
        } else {
            u64::from(deltas[slot as usize])
        };
        // Rows occupy the high word, so unsigned comparison orders both components.
        let delta = if STORED {
            ((delta >> 8) << 32) | (delta & 255)
        } else {
            delta
        };
        if END {
            self.base - delta
        } else {
            self.base + delta
        }
    }
    #[inline(always)]
    fn retain(&self, candidates: Mask, bounds: (Bound<u64>, Bound<u64>)) -> Mask {
        let limit = if STORED || END { 65536 } else { 256 };
        let bounds = delta_bounds::<_, END>(bounds, limit, |position, inclusive| {
            let (base, position, inclusive) = if END {
                (position, self.base, !inclusive)
            } else {
                (self.base, position, inclusive)
            };
            if STORED {
                point_cutoff(base, position, inclusive)
            } else {
                byte_cutoff(base, position, inclusive, limit)
            }
        });
        if STORED {
            retain_deltas::<true>(self.deltas, candidates, bounds)
        } else {
            retain_deltas::<END>(self.deltas, candidates, bounds)
        }
    }
}

// Oversized query coordinates cannot be packed without changing their ordering.
struct UnpackedPositions<P>(P);
struct UnpackedColumn<C>(C);
impl<C: PositionColumn<Position = u64>> PositionColumn for UnpackedColumn<C> {
    type Position = Point;
    #[inline]
    fn minimum(&self) -> Point {
        point_from_key(self.0.minimum())
    }
    #[inline]
    fn maximum(&self) -> Point {
        point_from_key(self.0.maximum())
    }
    #[inline]
    fn get(&self, slot: u32) -> Point {
        point_from_key(self.0.get(slot))
    }
}
impl<P: Positions<Position = u64>> Positions for UnpackedPositions<P> {
    type Position = Point;
    type Start = UnpackedColumn<P::Start>;
    type End = UnpackedColumn<P::End>;
    #[inline]
    fn start(&self) -> Self::Start {
        UnpackedColumn(self.0.start())
    }
    #[inline]
    fn end(&self) -> Self::End {
        UnpackedColumn(self.0.end())
    }
}
impl<'tree> Positions for BytePositions<'_, 'tree> {
    type Position = usize;
    type Start = ByteColumn<'tree, false>;
    type End = ByteColumn<'tree, true>;
    #[inline]
    fn start(&self) -> Self::Start {
        let group = self.0;
        ByteColumn {
            base: group
                .columns
                .word(group.columns.layout.start_byte_base, group.index) as usize,
            deltas: column_deltas(group, group.columns.layout.start_byte_delta, 1),
        }
    }
    #[inline]
    fn end(&self) -> Self::End {
        let group = self.0;
        ByteColumn {
            base: group
                .columns
                .word(group.columns.layout.end_byte_base, group.index) as usize,
            deltas: column_deltas(group, group.columns.layout.end_byte_delta, 2),
        }
    }
}
impl<'tree, const STORED: bool> Positions for PointPositions<'_, 'tree, STORED> {
    type Position = u64;
    type Start = PointColumn<'tree, false, STORED>;
    type End = PointColumn<'tree, true, STORED>;
    #[inline]
    fn start(&self) -> Self::Start {
        if STORED {
            PointColumn {
                base: point_base(self.group, self.layout.start_base),
                deltas: column_deltas(self.group, self.layout.start_delta, 2),
            }
        } else {
            let column = BytePositions(self.group).start();
            PointColumn {
                base: column.base as u64,
                deltas: column.deltas,
            }
        }
    }
    #[inline]
    fn end(&self) -> Self::End {
        if STORED {
            PointColumn {
                base: point_base(self.group, self.layout.end_base),
                deltas: column_deltas(self.group, self.layout.end_delta, 2),
            }
        } else {
            let column = BytePositions(self.group).end();
            PointColumn {
                base: column.base as u64,
                deltas: column.deltas,
            }
        }
    }
}
impl Coordinates for Bytes {
    type Position = usize;
    const MINIMUM: usize = 0;
    const PRUNE_SUBTREES: bool = true;
    fn new(_: &GroupRef<'_>) -> Self {
        Self
    }
    #[inline]
    fn start_minimum(&self, group: &GroupRef<'_>) -> usize {
        group
            .columns
            .word(group.columns.layout.start_byte_base, group.index) as usize
    }
    #[inline]
    fn end_before(&self, group: &GroupRef<'_>, bound: Bound<usize>) -> bool {
        before_bound(BytePositions(group).end().maximum(), bound)
    }
    #[inline(always)]
    fn retain<R: Relation<usize>>(
        &self,
        group: &GroupRef<'_>,
        candidates: impl FnOnce() -> Mask,
        relation: &R,
    ) -> Mask {
        relation.retain(BytePositions(group), candidates)
    }
}
impl Coordinates for Points {
    type Position = Point;
    const MINIMUM: Point = Point::new(0, 0);
    const PRUNE_SUBTREES: bool = false;
    fn new(group: &GroupRef<'_>) -> Self {
        unsafe extern "C" {
            fn sq_tree_scan_point_layout(tree: *const c_void, layout: *mut Points);
        }
        let mut layout = std::mem::MaybeUninit::uninit();
        // The bridge initializes all offsets; zero denotes absent point columns.
        unsafe {
            sq_tree_scan_point_layout(group.columns.root.raw.tree, layout.as_mut_ptr());
            layout.assume_init()
        }
    }
    #[inline(always)]
    fn start_minimum(&self, group: &GroupRef<'_>) -> Point {
        if self.start_base == 0 {
            Point::new(0, Bytes.start_minimum(group))
        } else {
            // Row and column bases are independent minima. Only the earliest
            // live node gives an actual start position ordered across groups.
            point_from_key(
                PointPositions::<true> {
                    group,
                    layout: self,
                }
                .start()
                .get(group.used() - 1),
            )
        }
    }
    #[inline]
    fn end_before(&self, group: &GroupRef<'_>, bound: Bound<Point>) -> bool {
        let end = if self.end_base == 0 {
            BytePositions(group).end().maximum() as u64
        } else {
            point_base(group, self.end_base)
        };
        match bound {
            Included(limit) | Excluded(limit) => {
                if let Some(limit) = point_key(limit) {
                    before_bound(
                        end,
                        match bound {
                            Included(_) => Included(limit),
                            _ => Excluded(limit),
                        },
                    )
                } else {
                    before_bound(point_from_key(end), bound)
                }
            }
            Unbounded => false,
        }
    }
    #[inline(always)]
    fn retain<R: Relation<Point>>(
        &self,
        group: &GroupRef<'_>,
        candidates: impl FnOnce() -> Mask,
        relation: &R,
    ) -> Mask {
        // Select the decoder once per group, keeping storage checks out of slot loops.
        if self.start_base == 0 {
            retain_points(
                relation,
                PointPositions::<false> {
                    group,
                    layout: self,
                },
                candidates,
            )
        } else {
            retain_points(
                relation,
                PointPositions::<true> {
                    group,
                    layout: self,
                },
                candidates,
            )
        }
    }
}

#[inline(always)]
fn retain_points<R: Relation<Point>, P: Positions<Position = u64>>(
    relation: &R,
    positions: P,
    candidates: impl FnOnce() -> Mask,
) -> Mask {
    if let Some(packed) = relation.try_map(point_key) {
        packed.retain(positions, candidates)
    } else {
        relation.retain(UnpackedPositions(positions), candidates)
    }
}

#[inline(always)]
fn before_bound<T: Ord>(position: T, bound: Bound<T>) -> bool {
    match bound {
        Included(limit) => position < limit,
        Excluded(limit) => position <= limit,
        Unbounded => false,
    }
}

// Each comparison must be monotonic over its column's conservative bounds.
#[inline(always)]
fn retain_pair<T: Copy + Ord>(
    candidates: impl FnOnce() -> Mask,
    first: impl PositionColumn<Position = T>,
    second: impl PositionColumn<Position = T>,
    first_bounds: (Bound<T>, Bound<T>),
    second_bounds: (Bound<T>, Bound<T>),
) -> Mask {
    let first_minimum = first_bounds.contains(&first.minimum());
    let first_maximum = first_bounds.contains(&first.maximum());
    let second_minimum = second_bounds.contains(&second.minimum());
    let second_maximum = second_bounds.contains(&second.maximum());
    if !(first_minimum || first_maximum) || !(second_minimum || second_maximum) {
        return Mask::default();
    }
    let candidates = candidates();
    let all_first = first_minimum && first_maximum;
    let all_second = second_minimum && second_maximum;
    let candidates = if all_first {
        candidates
    } else {
        first.retain(candidates, first_bounds)
    };
    if all_second || candidates.is_empty() {
        return candidates;
    }
    second.retain(candidates, second_bounds)
}
#[inline(always)]
fn retain_interval<T: Copy + Ord>(
    candidates: impl FnOnce() -> Mask,
    column: impl PositionColumn<Position = T>,
    range: &Range<T>,
) -> Mask {
    if column.maximum() < range.start || column.minimum() >= range.end {
        return Mask::default();
    }
    let candidates = candidates();
    if range.start <= column.minimum() && column.maximum() < range.end {
        return candidates;
    }
    column.retain(candidates, (Included(range.start), Excluded(range.end)))
}
#[inline(always)]
fn retain_equal<T: Copy + Ord>(
    candidates: impl FnOnce() -> Mask,
    column: impl PositionColumn<Position = T>,
    position: T,
) -> Mask {
    if position < column.minimum() || position > column.maximum() {
        return Mask::default();
    }
    column.retain(candidates(), (Included(position), Included(position)))
}

pub struct Overlapping<T>(Range<T>);
pub struct Within<T>(Range<T>);
pub struct Containing<T>(Range<T>);
pub struct StartingIn<T>(Range<T>);
pub struct EndingIn<T>(Range<T>);
pub struct ContainingPosition<T>(T);
pub struct StartingAt<T>(T);
pub struct EndingAt<T>(T);

macro_rules! range_relation {
    ($name:ident, $reject:tt, $lower:expr, $upper:expr, $end:expr, $this:ident, $positions:ident, $candidates:ident, $body:block) => {
        impl<T: Copy + Ord> Relation<T> for $name<T> {
            type Mapped<U: Copy + Ord> = $name<U>;
            fn try_map<U: Copy + Ord>(&self, convert: impl Fn(T) -> Option<U>) -> Option<Self::Mapped<U>> {
                Some($name(convert(self.0.start)?..convert(self.0.end)?))
            }
            fn is_empty(&self) -> bool { self.0.start $reject self.0.end }
            fn start_bounds(&$this) -> (Bound<T>, Bound<T>) { ($lower, $upper) }
            fn end_lower_bound(&$this) -> Bound<T> { $end }
            #[inline(always)]
            fn retain<P: Positions<Position = T>>(&$this, $positions: P, $candidates: impl FnOnce() -> Mask) -> Mask $body
        }
    };
}
range_relation!(Overlapping, >=, Unbounded, Excluded(self.0.end), Included(self.0.start), self, positions, candidates, {
    let starts = positions.start();
    let ends = positions.end();
    if starts.minimum() >= self.0.end || ends.maximum() < self.0.start {
        return Mask::default();
    }
    let candidates = candidates();
    let all_start = starts.maximum() < self.0.end;
    let all_end = ends.minimum() > self.0.start || starts.minimum() >= self.0.start;
    let candidates = if all_start {
        candidates
    } else {
        starts.retain(candidates, (Unbounded, Excluded(self.0.end)))
    };
    if all_end || candidates.is_empty() {
        return candidates;
    }
    let matches = ends.retain(candidates, (Excluded(self.0.start), Unbounded));
    let remaining = Mask(candidates.0 & !matches.0);
    if remaining.is_empty() {
        return matches;
    }
    Mask(matches.0 | starts.retain(remaining, (Included(self.0.start), Unbounded)).0)
});
range_relation!(Within, >, Included(self.0.start), Included(self.0.end), Unbounded, self, positions, candidates, {
    retain_pair(
        candidates,
        positions.start(),
        positions.end(),
        (Included(self.0.start), Unbounded),
        (Unbounded, Included(self.0.end)),
    )
});
range_relation!(Containing, >, Unbounded, Included(self.0.start), Included(self.0.end), self, positions, candidates, {
    retain_pair(
        candidates,
        positions.start(),
        positions.end(),
        (Unbounded, Included(self.0.start)),
        (Included(self.0.end), Unbounded),
    )
});
range_relation!(StartingIn, >=, Included(self.0.start), Excluded(self.0.end), Unbounded, self, positions, candidates, {
    retain_interval(candidates, positions.start(), &self.0)
});
range_relation!(EndingIn, >=, Unbounded, Excluded(self.0.end), Included(self.0.start), self, positions, candidates, {
    retain_interval(candidates, positions.end(), &self.0)
});
macro_rules! position_relation {
    ($name:ident, $lower:expr, $end:expr, $this:ident, $positions:ident, $candidates:ident, $body:block) => {
        impl<T: Copy + Ord> Relation<T> for $name<T> {
            type Mapped<U: Copy + Ord> = $name<U>;
            fn try_map<U: Copy + Ord>(&self, convert: impl Fn(T) -> Option<U>) -> Option<Self::Mapped<U>> {
                Some($name(convert(self.0)?))
            }
            fn start_bounds(&$this) -> (Bound<T>, Bound<T>) { ($lower, Included($this.0)) }
            fn end_lower_bound(&$this) -> Bound<T> { $end }
            #[inline(always)]
            fn retain<P: Positions<Position = T>>(&$this, $positions: P, $candidates: impl FnOnce() -> Mask) -> Mask $body
        }
    };
}
position_relation!(
    ContainingPosition,
    Unbounded,
    Excluded(self.0),
    self,
    positions,
    candidates,
    {
        retain_pair(
            candidates,
            positions.start(),
            positions.end(),
            (Unbounded, Included(self.0)),
            (Excluded(self.0), Unbounded),
        )
    }
);
position_relation!(
    StartingAt,
    Included(self.0),
    Unbounded,
    self,
    positions,
    candidates,
    { retain_equal(candidates, positions.start(), self.0) }
);
position_relation!(
    EndingAt,
    Unbounded,
    Included(self.0),
    self,
    positions,
    candidates,
    { retain_equal(candidates, positions.end(), self.0) }
);

/// Sources that still permit range restriction; filters do not implement this trait.
pub trait UnrestrictedScan: sealed::Source {
    fn restrict<C: Coordinates>(
        &mut self,
        coordinates: &C,
        bounds: (Bound<C::Position>, Bound<C::Position>),
    );
}
impl UnrestrictedScan for Preorder<'_> {
    // Specialize constant bound variants before entering either search.
    #[inline(always)]
    fn restrict<C: Coordinates>(
        &mut self,
        coordinates: &C,
        bounds: (Bound<C::Position>, Bound<C::Position>),
    ) {
        // Start minima decrease with physical group index. All relations supply
        // an upper bound on node starts, including those that only test ends.
        let mut lower = self.groups.start;
        let mut upper = self.groups.end;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            let start = coordinates.start_minimum(&self.group.columns.group(middle));
            let beyond = match bounds.1 {
                Bound::Included(limit) => start > limit,
                Bound::Excluded(limit) => start >= limit,
                Bound::Unbounded => false,
            };
            if beyond {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        self.groups.start = lower;
        if matches!(bounds.0, Included(limit) if limit == C::MINIMUM) {
            return;
        }
        if let Included(limit) | Excluded(limit) = bounds.0 {
            upper = self.groups.end;
            while lower < upper {
                let middle = lower + (upper - lower) / 2;
                let start = coordinates.start_minimum(&self.group.columns.group(middle));
                let within = match bounds.0 {
                    Included(_) => start >= limit,
                    Excluded(_) => start > limit,
                    Unbounded => unreachable!(),
                };
                if within {
                    lower = middle + 1;
                } else {
                    upper = middle;
                }
            }
            // The first group with an earlier minimum may still contain starts
            // inside the query. Keep that boundary group for slot comparisons.
            self.groups.end = self.groups.end.min(lower.saturating_add(1));
        }
    }
}
impl UnrestrictedScan for ReversePreorder<'_> {
    #[inline(always)]
    fn restrict<C: Coordinates>(
        &mut self,
        coordinates: &C,
        bounds: (Bound<C::Position>, Bound<C::Position>),
    ) {
        self.0.restrict(coordinates, bounds);
    }
}
impl UnrestrictedScan for Postorder<'_> {
    fn restrict<C: Coordinates>(&mut self, _: &C, _: (Bound<C::Position>, Bound<C::Position>)) {}
}
impl UnrestrictedScan for ReversePostorder<'_> {
    fn restrict<C: Coordinates>(&mut self, _: &C, _: (Bound<C::Position>, Bound<C::Position>)) {}
}

/// A coordinate system and a statically selected position relation.
pub struct Selection<C, R> {
    coordinates: C,
    relation: R,
}
/// A traversal restricted by a range or position before other filters.
pub struct Restricted<S, P> {
    source: S,
    selection: P,
}
pub type OverlappingBytes<S> = Restricted<S, Selection<Bytes, Overlapping<usize>>>;
pub type PreorderOverlappingBytes<'tree> = OverlappingBytes<Preorder<'tree>>;

macro_rules! selection_method {
    ($method:ident, $coordinates:ident, $relation:ident, $input:ty, $documentation:literal) => {
        #[doc = $documentation]
        pub fn $method(
            self,
            value: $input,
        ) -> Scan<
            'tree,
            Restricted<
                S,
                Selection<$coordinates, $relation<<$coordinates as Coordinates>::Position>>,
            >,
        > {
            self.selected::<$coordinates, _>($relation(value))
        }
    };
}
impl<'tree, S: UnrestrictedScan + GroupScan<'tree>> Scan<'tree, S> {
    fn selected<C: Coordinates, R: Relation<C::Position>>(
        mut self,
        relation: R,
    ) -> Scan<'tree, Restricted<S, Selection<C, R>>> {
        let coordinates = C::new(self.source.group());
        if !relation.is_empty() {
            self.source.restrict(&coordinates, relation.start_bounds());
        }
        Scan::new(Restricted {
            source: self.source,
            selection: Selection {
                coordinates,
                relation,
            },
        })
    }
    selection_method!(
        overlapping_bytes,
        Bytes,
        Overlapping,
        Range<usize>,
        "Intersect a nonempty byte range, including zero-width nodes at positions inside it."
    );
    selection_method!(
        within_bytes,
        Bytes,
        Within,
        Range<usize>,
        "Match spans wholly within a byte range. Zero-width nodes at either boundary qualify, including for empty queries."
    );
    selection_method!(
        containing_bytes,
        Bytes,
        Containing,
        Range<usize>,
        "Match spans enclosing a byte range, including equal spans. Empty queries use inclusive endpoint containment."
    );
    selection_method!(
        starting_in_bytes,
        Bytes,
        StartingIn,
        Range<usize>,
        "Match start offsets inside a half-open byte range."
    );
    selection_method!(
        ending_in_bytes,
        Bytes,
        EndingIn,
        Range<usize>,
        "Match exclusive end offsets inside a half-open byte range."
    );
    selection_method!(
        overlapping_points,
        Points,
        Overlapping,
        Range<Point>,
        "Point-coordinate counterpart of `overlapping_bytes`, including zero-width nodes."
    );
    selection_method!(
        within_points,
        Points,
        Within,
        Range<Point>,
        "Point-coordinate counterpart of `within_bytes`, including zero-width nodes at either boundary."
    );
    selection_method!(
        containing_points,
        Points,
        Containing,
        Range<Point>,
        "Match spans enclosing a point range, including equal spans. Empty queries use inclusive endpoint containment."
    );
    selection_method!(
        starting_in_points,
        Points,
        StartingIn,
        Range<Point>,
        "Match start positions inside a half-open point range."
    );
    selection_method!(
        ending_in_points,
        Points,
        EndingIn,
        Range<Point>,
        "Match exclusive end positions inside a half-open point range."
    );
    selection_method!(
        containing_byte,
        Bytes,
        ContainingPosition,
        usize,
        "Match `start <= offset < end`; zero-width nodes do not qualify."
    );
    selection_method!(
        starting_at_byte,
        Bytes,
        StartingAt,
        usize,
        "Match an exact start offset, including zero-width nodes."
    );
    selection_method!(
        ending_at_byte,
        Bytes,
        EndingAt,
        usize,
        "Match an exact exclusive end offset, including zero-width nodes."
    );
    selection_method!(
        containing_point,
        Points,
        ContainingPosition,
        Point,
        "Match `start <= position < end`; zero-width nodes do not qualify."
    );
    selection_method!(
        starting_at_point,
        Points,
        StartingAt,
        Point,
        "Match an exact start position, including zero-width nodes."
    );
    selection_method!(
        ending_at_point,
        Points,
        EndingAt,
        Point,
        "Match an exact exclusive end position, including zero-width nodes."
    );
}
impl<S: sealed::Source, P> sealed::Source for Restricted<S, P> {}
impl<C, R> sealed::Predicate for Selection<C, R> {}
impl<C: Coordinates, R: Relation<C::Position>> Predicate for Selection<C, R> {
    #[inline(always)]
    fn has_subtree_bound(&self) -> bool {
        if !C::PRUNE_SUBTREES {
            return false;
        }
        match self.relation.end_lower_bound() {
            Unbounded => false,
            Included(limit) => limit != C::MINIMUM,
            Excluded(_) => true,
        }
    }
    #[inline(always)]
    fn excludes_subtrees(&self, group: &GroupRef<'_>) -> bool {
        self.coordinates
            .end_before(group, self.relation.end_lower_bound())
    }
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        self.retain_group(group, || candidates)
    }
    #[inline(always)]
    fn retain_group(&self, group: &GroupRef<'_>, candidates: impl FnOnce() -> Mask) -> Mask {
        if self.relation.is_empty() {
            return Mask::default();
        }
        self.coordinates.retain(group, candidates, &self.relation)
    }
}
impl<'tree, S: GroupScan<'tree>, C: Coordinates, R: Relation<C::Position>> GroupScan<'tree>
    for Restricted<S, Selection<C, R>>
{
    type Reversed = Restricted<S::Reversed, Selection<C, R>>;
    type Slots = MatchingSlots<Self>;
    const DESCENDING: bool = S::DESCENDING;
    #[inline]
    fn slots(matches: Mask) -> Self::Slots {
        MatchingSlots::new(matches)
    }
    #[inline]
    fn reverse(self) -> Self::Reversed {
        Restricted {
            source: self.source.reverse(),
            selection: self.selection,
        }
    }
    #[inline]
    fn group(&self) -> &GroupRef<'tree> {
        self.source.group()
    }
    #[inline]
    fn count(self) -> usize {
        if self.selection.relation.is_empty() {
            return 0;
        }
        self.source.count_matches(self.selection)
    }
    #[inline]
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        if self.selection.relation.is_empty() {
            return 0;
        }
        self.source.count_matches(And(self.selection, predicate))
    }
    #[inline]
    fn next_mask(&mut self) -> Option<Mask> {
        if self.selection.relation.is_empty() {
            return None;
        }
        self.source.next_matching(&self.selection)
    }
}

pub trait Predicate: sealed::Predicate {
    /// Whether this query has a nontrivial lower end bound for subtree pruning.
    #[inline(always)]
    fn has_subtree_bound(&self) -> bool {
        false
    }
    /// Whether every node in this group and all its descendants must fail.
    #[inline(always)]
    fn excludes_subtrees(&self, _group: &GroupRef<'_>) -> bool {
        false
    }
    /// Called once when the predicate is attached to a scan.
    fn prepare(&mut self, _group: &GroupRef<'_>) {}
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask;
    /// Group bounds may reject a fragment before constructing its live-slot mask.
    #[inline(always)]
    fn retain_group(&self, group: &GroupRef<'_>, candidates: impl FnOnce() -> Mask) -> Mask {
        self.retain_matches(group, candidates())
    }
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
    fn has_subtree_bound(&self) -> bool {
        self.0.has_subtree_bound()
    }
    #[inline(always)]
    fn excludes_subtrees(&self, group: &GroupRef<'_>) -> bool {
        self.0.excludes_subtrees(group)
    }
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        self.retain_group(group, || candidates)
    }
    #[inline(always)]
    fn retain_group(&self, group: &GroupRef<'_>, candidates: impl FnOnce() -> Mask) -> Mask {
        let matches = self.0.retain_group(group, candidates);
        if matches.is_empty() {
            matches
        } else {
            self.1.retain_matches(group, matches)
        }
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
        if candidates.at_most::<4>() {
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
#[inline]
fn retain_supertype_masks(masks: &[u8], candidates: Mask, bit: u16) -> Mask {
    if candidates.is_empty() {
        return candidates;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let remaining = candidates.0 & (candidates.0 - 1);
        if remaining != 0 && !remaining.is_power_of_two() {
            use std::arch::x86_64::*;
            let mut absent = 0;
            // Each checked chunk covers both SSE2 loads. Candidate clipping
            // excludes waste and slots outside the subtree after mask extraction.
            unsafe {
                let bit = _mm_set1_epi16(bit as i16);
                let zero = _mm_setzero_si128();
                for (index, bytes) in masks.chunks_exact(32).enumerate() {
                    let low = _mm_loadu_si128(bytes.as_ptr().cast());
                    let high = _mm_loadu_si128(bytes.as_ptr().add(16).cast());
                    let low = _mm_cmpeq_epi16(_mm_and_si128(low, bit), zero);
                    let high = _mm_cmpeq_epi16(_mm_and_si128(high, bit), zero);
                    absent |=
                        (_mm_movemask_epi8(_mm_packs_epi16(low, high)) as u64) << (index * 16);
                }
            }
            return Mask(candidates.0 & !absent);
        }
    }
    candidates.retain(|slot| {
        let offset = slot as usize * 2;
        u16::from_le_bytes(masks[offset..offset + 2].try_into().unwrap()) & bit != 0
    })
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
        if columns.supertypes.len() <= 8 {
            return retain_supertype_masks(
                column_deltas(group, columns.layout.supertype, 2).slice(),
                candidates,
                1 << index,
            );
        }
        let words = columns.supertypes.len().div_ceil(64);
        candidates.retain(|slot| {
            let value = columns.short(columns.layout.supertype, group.first_slot() + slot);
            columns.supertype_masks[usize::from(value) * words + index / 64]
                & (1u64 << (index % 64))
                != 0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_supertype_masks_match_scalar_membership() {
        for length in [16, 32, 64] {
            for first in (0..256).step_by(length) {
                let values = (first..first + length)
                    .map(|value| value as u16)
                    .collect::<Vec<_>>();
                let bytes = values
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect::<Vec<_>>();
                let live = Mask::lower(length as u32);
                for candidates in [
                    Mask::default(),
                    live,
                    Mask(live.0 & 0xaaaa_aaaa_aaaa_aaaa),
                    Mask(live.0 & !Mask::lower(3).0),
                    Mask::lower(5),
                    Mask(1 << (length - 1)),
                    Mask(1 | 1 << (length - 1)),
                ] {
                    for index in 0..8 {
                        let bit = 1 << index;
                        let expected = candidates.retain(|slot| values[slot as usize] & bit != 0);
                        assert_eq!(retain_supertype_masks(&bytes, candidates, bit), expected);
                    }
                }
            }
        }
    }

    impl<'tree> From<&'tree [u8]> for ColumnDeltas<'tree> {
        fn from(data: &'tree [u8]) -> Self {
            Self {
                data,
                start: 0,
                length: data.len(),
            }
        }
    }

    fn check_column<C: PositionColumn>(column: C, length: u32, positions: &[C::Position]) {
        let live = Mask::lower(length);
        for candidates in [
            Mask::default(),
            live,
            Mask(live.0 & 0xaaaa_aaaa_aaaa_aaaa),
            Mask::lower(5),
            Mask(1 << (length - 1)),
            Mask(1 | 1 << (length - 1)),
        ] {
            for &position in positions {
                for bounds in [
                    (Unbounded, Included(position)),
                    (Unbounded, Excluded(position)),
                    (Included(position), Unbounded),
                    (Excluded(position), Unbounded),
                    (Included(position), Included(position)),
                ] {
                    let expected = candidates.retain(|slot| bounds.contains(&column.get(slot)));
                    assert_eq!(column.retain(candidates, bounds), expected);
                }
            }
            for pair in positions.windows(2) {
                let bounds = (Included(pair[0]), Excluded(pair[1]));
                let expected = candidates.retain(|slot| bounds.contains(&column.get(slot)));
                assert_eq!(column.retain(candidates, bounds), expected);
            }
        }
    }

    #[test]
    fn byte_delta_bounds_match_decoded_positions() {
        let bytes = (0..=u8::MAX).collect::<Vec<_>>();
        for length in [16, 32, 64] {
            for deltas in bytes.chunks_exact(length) {
                for base in [0, 65535, u32::MAX as usize - 255] {
                    check_column(
                        ByteColumn::<false> {
                            base,
                            deltas: deltas.into(),
                        },
                        length as u32,
                        &[
                            0,
                            base,
                            base + 1,
                            base + 127,
                            base + 128,
                            base + 255,
                            base + 256,
                            usize::MAX,
                        ],
                    );
                }
            }
        }
        let bytes = (0..=u16::MAX)
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        for deltas in bytes.chunks_exact(128) {
            check_column(
                ByteColumn::<true> {
                    base: 65535,
                    deltas: deltas.into(),
                },
                64,
                &[
                    0,
                    1,
                    255,
                    256,
                    32767,
                    32768,
                    65534,
                    65535,
                    65536,
                    usize::MAX,
                ],
            );
        }
    }

    #[test]
    fn point_delta_bounds_match_decoded_positions() {
        let bytes = (0..=u16::MAX)
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let base = (300 << 32) | 400;
        let positions = [
            0,
            (44 << 32) | 400,
            (45 << 32) | 144,
            (45 << 32) | 145,
            (299 << 32) | u64::from(u32::MAX),
            (300 << 32) | 399,
            base,
            (300 << 32) | 401,
            (301 << 32) | 399,
            (555 << 32) | 655,
            (555 << 32) | 656,
            556 << 32,
            u64::MAX,
        ];
        for deltas in bytes.chunks_exact(128) {
            check_column(
                PointColumn::<false, true> {
                    base,
                    deltas: deltas.into(),
                },
                64,
                &positions,
            );
            check_column(
                PointColumn::<true, true> {
                    base,
                    deltas: deltas.into(),
                },
                64,
                &positions,
            );
        }
    }
}
