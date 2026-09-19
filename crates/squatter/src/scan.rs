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
    group_shift: u32,
    symbol_count: u32,
    symbol_shift: u32,
    supertype_count: u32,
    supertype_mask_count: u32,
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
    raw: RawColumns,
    data: &'tree [u8],
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
        let data = unsafe { std::slice::from_raw_parts(raw.data, raw.size as usize) };
        Self { raw, data, root }
    }
    #[inline]
    fn group_size(self) -> u32 {
        1 << self.raw.group_shift
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
        slot - self.word(self.raw.span_base, slot >> self.raw.group_shift)
            - u32::from(self.byte(self.raw.span_delta, slot))
    }
    #[inline]
    fn previous_slot(self, slot: u32) -> Option<u32> {
        let previous = slot.checked_sub(1)?;
        // Slots and subtree boundaries have a live predecessor within a group;
        // only crossing a physical group boundary requires skipping its waste.
        if slot & (self.group_size() - 1) != 0 {
            Some(previous)
        } else {
            Some(previous - u32::from(self.short(self.raw.waste, previous >> self.raw.group_shift)))
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
        let mut remaining = self;
        let mut matches = 0;
        while let Some(slot) = remaining.pop(false) {
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
        self.index << self.columns.raw.group_shift
    }
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
            - u32::from(self.columns.short(self.columns.raw.waste, self.index))
    }
    #[inline]
    fn kind(self, slot: u32) -> u16 {
        let raw = self.columns.raw;
        let symbol =
            u32::from(self.columns.short(raw.symbol, self.first_slot() + slot)) >> raw.symbol_shift;
        if symbol == raw.symbol_count - 2 {
            u16::MAX
        } else if symbol == raw.symbol_count - 1 {
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
                for (index, bytes) in bytes.chunks_exact(32).enumerate() {
                    let low = _mm_loadu_si128(bytes.as_ptr().cast());
                    let high = _mm_loadu_si128(bytes.as_ptr().add(16).cast());
                    let low = _mm_cmpeq_epi16(_mm_srl_epi16(low, shift), target);
                    let high = _mm_cmpeq_epi16(_mm_srl_epi16(high, shift), target);
                    matches |=
                        (_mm_movemask_epi8(_mm_packs_epi16(low, high)) as u64) << (index * 16);
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
    #[inline]
    fn bitmap(self, offset: u32) -> Mask {
        if offset == 0 {
            return Mask::default();
        }
        let mut bits = 0;
        for byte in 0..self.columns.group_size() / 8 {
            bits |=
                u64::from(self.columns.byte(offset, self.first_slot() / 8 + byte)) << (byte * 8);
        }
        Mask(bits).intersection(self.valid_mask())
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
}

/// Internal protocol exposed for generic scan consumers. Implementations are sealed.
pub trait GroupScan<'tree>: sealed::Source {
    fn next_group(&mut self) -> Option<GroupMatches<'tree>>;
    /// Returns the last fragment in reverse extraction order.
    fn next_back_group(&mut self) -> Option<GroupMatches<'tree>>;
    fn count_matches<P: Predicate>(self, predicate: P) -> usize
    where
        Self: Sized,
    {
        count_groups(self, predicate)
    }
}
#[inline(always)]
fn count_groups<'tree, S: GroupScan<'tree>, P: Predicate>(mut source: S, predicate: P) -> usize {
    let mut count = 0;
    while let Some(group) = source.next_group() {
        count += predicate
            .retain_matches(&group.group, group.matches)
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
            source: self.source,
            front: None,
            back: None,
        }
    }
    pub fn groups(self) -> Groups<'tree, S> {
        Groups(self.source, PhantomData)
    }
    /// Count matching nodes with population counts, without constructing handles.
    pub fn count(self) -> usize {
        self.source.count_matches(Identity)
    }
    pub fn rev(self) -> Scan<'tree, Reverse<S>> {
        Scan::new(Reverse(self.source))
    }
    pub fn filter_kind_ids<'kinds>(
        self,
        kinds: &'kinds KindSet,
    ) -> Scan<'tree, Filtered<S, KindIds<'kinds>>> {
        self.filtered(KindIds(kinds))
    }
    /// Zero matches nodes with no field, including the tree root.
    pub fn filter_field_id(self, field: u16) -> Scan<'tree, Filtered<S, FieldId>> {
        self.filtered(FieldId(field))
    }
    pub fn filter_supertype_id(self, supertype: u16) -> Scan<'tree, Filtered<S, SupertypeId>> {
        self.filtered(SupertypeId(supertype))
    }
    pub fn filter_extra(self, value: bool) -> Scan<'tree, Filtered<S, Extra>> {
        self.filtered(Extra(value))
    }
    pub fn filter_missing(self, value: bool) -> Scan<'tree, Filtered<S, Missing>> {
        self.filtered(Missing(value))
    }
    fn filtered<P>(self, predicate: P) -> Scan<'tree, Filtered<S, P>> {
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
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next_group()
    }
}
impl<'tree, S: GroupScan<'tree>> DoubleEndedIterator for Groups<'tree, S> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back_group()
    }
}
impl<'tree, S: GroupScan<'tree>> FusedIterator for Groups<'tree, S> {}

pub struct Nodes<'tree, S> {
    source: S,
    front: Option<GroupNodes<'tree>>,
    back: Option<GroupNodes<'tree>>,
}
impl<'tree, S: GroupScan<'tree>> Iterator for Nodes<'tree, S> {
    type Item = Node<'tree>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(node) = self.front.as_mut().and_then(|group| group.pop(false)) {
                return Some(node);
            }
            self.front = self.source.next_group().map(GroupMatches::nodes);
            if self.front.is_none() {
                return self.back.as_mut().and_then(|group| group.pop(true));
            }
        }
    }
    fn count(self) -> usize {
        let pending = [self.front, self.back]
            .into_iter()
            .flatten()
            .map(|group| group.matches.count_ones() as usize)
            .sum::<usize>();
        pending + Scan::new(self.source).count()
    }
}
impl<'tree, S: GroupScan<'tree>> DoubleEndedIterator for Nodes<'tree, S> {
    fn next_back(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(node) = self.back.as_mut().and_then(|group| group.pop(false)) {
                return Some(node);
            }
            self.back = self.source.next_back_group().map(GroupMatches::nodes);
            if self.back.is_none() {
                return self.front.as_mut().and_then(|group| group.pop(true));
            }
        }
    }
}
impl<'tree, S: GroupScan<'tree>> FusedIterator for Nodes<'tree, S> {}

/// Contiguous physical groups, descending in logical preorder.
pub struct Preorder<'tree> {
    columns: Columns<'tree>,
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
            columns,
            groups: (first >> columns.raw.group_shift)
                ..(root.slot() >> columns.raw.group_shift) + 1,
            slots: first..root.slot() + 1,
        }
    }
    #[inline]
    fn fragment(&self, index: u32, descending: bool) -> GroupMatches<'tree> {
        let group = self.columns.group(index);
        let first = self.slots.start.saturating_sub(group.first_slot());
        let end = (self.slots.end - group.first_slot()).min(group.used());
        GroupMatches {
            group,
            matches: Mask(Mask::lower(end).0 & !Mask::lower(first).0),
            descending,
        }
    }
}
impl sealed::Source for Preorder<'_> {}
impl<'tree> GroupScan<'tree> for Preorder<'tree> {
    #[inline]
    fn next_group(&mut self) -> Option<GroupMatches<'tree>> {
        loop {
            let index = self.groups.next_back()?;
            let fragment = self.fragment(index, true);
            if !fragment.matches.is_empty() {
                return Some(fragment);
            }
        }
    }
    #[inline]
    fn next_back_group(&mut self) -> Option<GroupMatches<'tree>> {
        loop {
            let index = self.groups.next()?;
            let fragment = self.fragment(index, false);
            if !fragment.matches.is_empty() {
                return Some(fragment);
            }
        }
    }
}

/// Topology traversal over reverse-preorder storage; currently singleton fragments.
pub struct Postorder<'tree> {
    columns: Columns<'tree>,
    front: ForwardPostorder,
    back: ReversePostorder,
    last_front: Option<u32>,
    last_back: Option<u32>,
    finished: bool,
}
#[derive(Default)]
struct ForwardPostorder {
    ancestors: Vec<(u32, u32)>,
    next: Option<u32>,
    first: u32,
    started: bool,
}
impl ForwardPostorder {
    #[inline]
    fn next(&mut self, columns: Columns<'_>) -> Option<u32> {
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
#[derive(Default)]
struct ReversePostorder {
    pending: Vec<u32>,
    started: bool,
}
impl ReversePostorder {
    #[inline]
    fn next(&mut self, columns: Columns<'_>) -> Option<u32> {
        if !self.started {
            self.started = true;
            self.pending.push(columns.root.slot());
        }
        let slot = self.pending.pop()?;
        let first = columns.first_slot(slot);
        if first != slot {
            let mut child = columns.previous_slot(slot);
            while let Some(current) = child.filter(|&current| current >= first) {
                self.pending.push(current);
                child = columns.previous_slot(columns.first_slot(current));
            }
        }
        Some(slot)
    }
}
impl<'tree> Postorder<'tree> {
    pub(crate) fn scan(root: Node<'tree>) -> Scan<'tree, Self> {
        Scan::new(Self {
            columns: Columns::new(root),
            front: ForwardPostorder::default(),
            back: ReversePostorder::default(),
            last_front: None,
            last_back: None,
            finished: false,
        })
    }
    #[inline]
    fn take(&mut self, reverse: bool) -> Option<GroupMatches<'tree>> {
        if self.finished {
            return None;
        }
        let slot = if reverse {
            self.back.next(self.columns)
        } else {
            self.front.next(self.columns)
        };
        let (own, other) = if reverse {
            (&mut self.last_back, self.last_front)
        } else {
            (&mut self.last_front, self.last_back)
        };
        if slot.is_none() || slot == other {
            self.finished = true;
            return None;
        }
        *own = slot;
        let slot = slot.unwrap();
        Some(GroupMatches {
            group: self.columns.group(slot >> self.columns.raw.group_shift),
            matches: Mask(1u64 << (slot & (self.columns.group_size() - 1))),
            descending: !reverse,
        })
    }
}
impl sealed::Source for Postorder<'_> {}
impl<'tree> GroupScan<'tree> for Postorder<'tree> {
    #[inline]
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        // Pure predicates and a scalar count do not observe traversal order.
        // A partially consumed scan must retain its remaining topology instead.
        if !self.front.started && !self.back.started {
            Preorder::new(self.columns).count_matches(predicate)
        } else {
            count_groups(self, predicate)
        }
    }

    #[inline]
    fn next_group(&mut self) -> Option<GroupMatches<'tree>> {
        self.take(false)
    }
    #[inline]
    fn next_back_group(&mut self) -> Option<GroupMatches<'tree>> {
        self.take(true)
    }
}

pub struct Reverse<S>(S);
impl<S: sealed::Source> sealed::Source for Reverse<S> {}
impl<'tree, S: GroupScan<'tree>> GroupScan<'tree> for Reverse<S> {
    #[inline]
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        self.0.count_matches(predicate)
    }

    #[inline]
    fn next_group(&mut self) -> Option<GroupMatches<'tree>> {
        self.0.next_back_group()
    }
    #[inline]
    fn next_back_group(&mut self) -> Option<GroupMatches<'tree>> {
        self.0.next_group()
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
            let start = self.columns.word(self.columns.raw.start_byte_base, middle) as usize;
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
impl<S: UnrestrictedScan> UnrestrictedScan for Reverse<S> {
    fn restrict_bytes(&mut self, range: &Range<usize>) {
        self.0.restrict_bytes(range);
    }
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
    let start_base = columns.word(columns.raw.start_byte_base, group.index) as usize;
    let end_base = columns.word(columns.raw.end_byte_base, group.index) as usize;
    if start_base >= range.end || end_base <= range.start {
        return Mask::default();
    }
    let all_start = start_base.saturating_add(255) < range.end;
    let all_end = end_base.saturating_sub(65535) > range.start;
    candidates.retain(|slot| {
        let slot = group.first_slot() + slot;
        let start = start_base + usize::from(columns.byte(columns.raw.start_byte_delta, slot));
        let end = end_base - usize::from(columns.short(columns.raw.end_byte_delta, slot));
        start < end && (all_start || start < range.end) && (all_end || end > range.start)
    })
}

impl<'tree, S: GroupScan<'tree>> GroupScan<'tree> for OverlappingBytes<S> {
    #[inline]
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        if self.range.is_empty() {
            return 0;
        }
        self.source
            .count_matches(And(ByteRange(self.range), predicate))
    }

    #[inline]
    fn next_group(&mut self) -> Option<GroupMatches<'tree>> {
        if self.range.is_empty() {
            return None;
        }
        loop {
            let mut group = self.source.next_group()?;
            group.matches = overlapping_matches(&group.group, group.matches, &self.range);
            if !group.matches.is_empty() {
                return Some(group);
            }
        }
    }
    #[inline]
    fn next_back_group(&mut self) -> Option<GroupMatches<'tree>> {
        if self.range.is_empty() {
            return None;
        }
        loop {
            let mut group = self.source.next_back_group()?;
            group.matches = overlapping_matches(&group.group, group.matches, &self.range);
            if !group.matches.is_empty() {
                return Some(group);
            }
        }
    }
}

pub trait Predicate: sealed::Predicate {
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
    #[inline]
    fn count_matches<Q: Predicate>(self, predicate: Q) -> usize {
        self.source.count_matches(And(self.predicate, predicate))
    }

    #[inline]
    fn next_group(&mut self) -> Option<GroupMatches<'tree>> {
        loop {
            let mut group = self.source.next_group()?;
            group.matches = self.predicate.retain_matches(&group.group, group.matches);
            if !group.matches.is_empty() {
                return Some(group);
            }
        }
    }
    #[inline]
    fn next_back_group(&mut self) -> Option<GroupMatches<'tree>> {
        loop {
            let mut group = self.source.next_back_group()?;
            group.matches = self.predicate.retain_matches(&group.group, group.matches);
            if !group.matches.is_empty() {
                return Some(group);
            }
        }
    }
}

pub struct KindIds<'kinds>(&'kinds KindSet);
impl sealed::Predicate for KindIds<'_> {}
impl Predicate for KindIds<'_> {
    // Inlining lets node consumers discard unused group metadata.
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        if self.0.is_empty() {
            return Mask::default();
        }
        if let [kind] = self.0.ids.as_slice() {
            let raw = group.columns.raw;
            let target = match *kind {
                u16::MAX => (raw.symbol_count - 2) as u16,
                value if value == u16::MAX - 1 => (raw.symbol_count - 1) as u16,
                value if u32::from(value) < raw.symbol_count - 2 => value,
                _ => return Mask::default(),
            };
            return group.equal_ids(raw.symbol, raw.symbol_shift, target, candidates);
        }
        if candidates.0.is_power_of_two() {
            return candidates.retain(|slot| self.0.contains(group.kind(slot)));
        }
        let raw = group.columns.raw;
        let start = raw.symbol as usize + group.first_slot() as usize * 2;
        let bytes = &group.columns.data[start..start + group.used() as usize * 2];
        let mut matches = 0;
        for (slot, bytes) in bytes.chunks_exact(2).enumerate() {
            let symbol = u32::from(u16::from_le_bytes([bytes[0], bytes[1]])) >> raw.symbol_shift;
            let kind = if symbol == raw.symbol_count - 2 {
                u16::MAX
            } else if symbol == raw.symbol_count - 1 {
                u16::MAX - 1
            } else {
                symbol as u16
            };
            matches |= u64::from(self.0.contains(kind)) << slot;
        }
        candidates.intersection(Mask(matches))
    }
}
pub struct FieldId(u16);
impl sealed::Predicate for FieldId {}
impl Predicate for FieldId {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        group.equal_ids(group.columns.raw.field, 0, self.0, candidates)
    }
}
pub struct Extra(bool);
impl sealed::Predicate for Extra {}
impl Predicate for Extra {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let flags = group.bitmap(group.columns.raw.extra).0;
        Mask(candidates.0 & if self.0 { flags } else { !flags })
    }
}
pub struct Missing(bool);
impl sealed::Predicate for Missing {}
impl Predicate for Missing {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let flags = group.bitmap(group.columns.raw.missing).0;
        Mask(candidates.0 & if self.0 { flags } else { !flags })
    }
}
pub struct SupertypeId(u16);
impl sealed::Predicate for SupertypeId {}
impl Predicate for SupertypeId {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let raw = group.columns.raw;
        if raw.supertype_count == 0 {
            return Mask::default();
        }
        // Native grammar arrays are immutable and live as long as columns.root.
        let supertypes =
            unsafe { std::slice::from_raw_parts(raw.supertypes, raw.supertype_count as usize) };
        let Ok(index) = supertypes.binary_search(&self.0) else {
            return Mask::default();
        };
        let words = (raw.supertype_count as usize).div_ceil(64);
        let masks = if raw.supertype_count > 8 {
            unsafe {
                std::slice::from_raw_parts(
                    raw.supertype_masks,
                    raw.supertype_mask_count as usize * words,
                )
            }
        } else {
            &[]
        };
        candidates.retain(|slot| {
            let value = group
                .columns
                .short(raw.supertype, group.first_slot() + slot);
            if raw.supertype_count <= 8 {
                value & (1 << index) != 0
            } else {
                masks[usize::from(value) * words + index / 64] & (1u64 << (index % 64)) != 0
            }
        })
    }
}
