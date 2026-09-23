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
use crate::{
    FieldId, FieldSet, GrammarKindId, KindId, KindSet, Node, PointData, SlotIx,
    native::GrammarView,
    storage::{ColumnPointer, GROUP_SIZE, Layout, TreeData},
    types::{GroupIx, GroupSlotIx, PackedPoint, RemappedKindId},
};
use std::{
    iter::FusedIterator,
    marker::PhantomData,
    ops::{Bound, Bound::*, Range, RangeBounds},
};
use tree_sitter::Point;

#[cfg(target_arch = "x86_64")]
use fearless_simd::{Level, prelude::*, u8x16};

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
use fearless_simd::u8x32;

// Bias 16-bit deltas so signed comparisons test an unsigned range.
#[cfg(target_arch = "x86_64")]
fearless_simd::kernel!(
    #[inline]
    fn sse2_retain_wide(simd: Sse2, bytes: &[u8], start: u32, length: u32) -> u16 {
        use std::arch::x86_64::*;

        let lower = _mm_set1_epi16((start as u16 ^ 0x8000) as i16);
        let upper = _mm_set1_epi16((length as u16 ^ 0x8000) as i16);
        let equal = _mm_set1_epi16(start as i16);
        let low: __m128i = u8x16::from_slice(simd, &bytes[..16]).into();
        let high: __m128i = u8x16::from_slice(simd, &bytes[16..32]).into();
        let matching = |values| {
            if length == 1 {
                _mm_cmpeq_epi16(values, equal)
            } else {
                _mm_cmpgt_epi16(upper, _mm_sub_epi16(values, lower))
            }
        };
        _mm_movemask_epi8(_mm_packs_epi16(matching(low), matching(high))) as u16
    }
);

// Bias 16-bit deltas so signed AVX2 comparisons test an unsigned range.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
fearless_simd::kernel!(
    #[inline]
    fn avx2_retain_wide(simd: Avx2, bytes: &[u8], start: u32, length: u32) -> u32 {
        use std::arch::x86_64::*;

        let lower = _mm256_set1_epi16((start as u16 ^ 0x8000) as i16);
        let upper = _mm256_set1_epi16((length as u16 ^ 0x8000) as i16);
        let equal = _mm256_set1_epi16(start as i16);
        let low: __m256i = u8x32::from_slice(simd, &bytes[..32]).into();
        let high: __m256i = u8x32::from_slice(simd, &bytes[32..64]).into();
        let matching = |values| {
            if length == 1 {
                _mm256_cmpeq_epi16(values, equal)
            } else {
                _mm256_cmpgt_epi16(upper, _mm256_sub_epi16(values, lower))
            }
        };
        _mm256_movemask_epi8(_mm256_permute4x64_epi64(
            _mm256_packs_epi16(matching(low), matching(high)),
            0xd8,
        )) as u32
    }
);

// Bias byte deltas so signed comparisons test an unsigned range.
#[cfg(target_arch = "x86_64")]
fearless_simd::kernel!(
    #[inline]
    fn sse2_retain_narrow(simd: Sse2, bytes: &[u8], start: u32, length: u32) -> u16 {
        use std::arch::x86_64::*;

        let lower = _mm_set1_epi8((start as u8 ^ 0x80) as i8);
        let upper = _mm_set1_epi8((length as u8 ^ 0x80) as i8);
        let equal = _mm_set1_epi8(start as i8);
        let values: __m128i = u8x16::from_slice(simd, bytes).into();
        let selected = if length == 1 {
            _mm_cmpeq_epi8(values, equal)
        } else {
            _mm_cmpgt_epi8(upper, _mm_sub_epi8(values, lower))
        };
        _mm_movemask_epi8(selected) as u16
    }
);

// Bias byte deltas so signed AVX2 comparisons test an unsigned range.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
fearless_simd::kernel!(
    #[inline]
    fn avx2_retain_narrow(simd: Avx2, bytes: &[u8], start: u32, length: u32) -> u32 {
        use std::arch::x86_64::*;

        let lower = _mm256_set1_epi8((start as u8 ^ 0x80) as i8);
        let upper = _mm256_set1_epi8((length as u8 ^ 0x80) as i8);
        let equal = _mm256_set1_epi8(start as i8);
        let values: __m256i = u8x32::from_slice(simd, bytes).into();
        let selected = if length == 1 {
            _mm256_cmpeq_epi8(values, equal)
        } else {
            _mm256_cmpgt_epi8(upper, _mm256_sub_epi8(values, lower))
        };
        _mm256_movemask_epi8(selected) as u32
    }
);

// Pack zero bit tests into a mask of slots without the supertype.
#[cfg(target_arch = "x86_64")]
fearless_simd::kernel!(
    #[inline]
    fn sse2_absent_supertype(simd: Sse2, bytes: &[u8], bit: u16) -> u16 {
        use std::arch::x86_64::*;

        let bit = _mm_set1_epi16(bit as i16);
        let zero = _mm_setzero_si128();
        let low: __m128i = u8x16::from_slice(simd, &bytes[..16]).into();
        let high: __m128i = u8x16::from_slice(simd, &bytes[16..32]).into();
        let low = _mm_cmpeq_epi16(_mm_and_si128(low, bit), zero);
        let high = _mm_cmpeq_epi16(_mm_and_si128(high, bit), zero);
        _mm_movemask_epi8(_mm_packs_epi16(low, high)) as u16
    }
);

// Shift encoded words before comparing remapped IDs.
#[cfg(target_arch = "x86_64")]
fearless_simd::kernel!(
    #[inline]
    fn sse2_equal_ids(simd: Sse2, bytes: &[u8], shift: u32, target: u16) -> u16 {
        use std::arch::x86_64::*;

        let shift = _mm_cvtsi32_si128(shift as i32);
        let target = _mm_set1_epi16(target as i16);
        let low: __m128i = u8x16::from_slice(simd, &bytes[..16]).into();
        let high: __m128i = u8x16::from_slice(simd, &bytes[16..32]).into();
        let low = _mm_cmpeq_epi16(_mm_srl_epi16(low, shift), target);
        let high = _mm_cmpeq_epi16(_mm_srl_epi16(high, shift), target);
        _mm_movemask_epi8(_mm_packs_epi16(low, high)) as u16
    }
);

// Avx2 tokens require x86-64-v3; +avx2 builds can detect that level at runtime.
// Shift encoded words before comparing remapped IDs across 32 slots.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
fearless_simd::kernel!(
    #[inline]
    fn avx2_equal_ids(simd: Avx2, bytes: &[u8], shift: u32, target: u16) -> u32 {
        use std::arch::x86_64::*;

        let shift = _mm_cvtsi32_si128(shift as i32);
        let target = _mm256_set1_epi16(target as i16);
        let low: __m256i = u8x32::from_slice(simd, &bytes[..32]).into();
        let high: __m256i = u8x32::from_slice(simd, &bytes[32..64]).into();
        let low = _mm256_cmpeq_epi16(_mm256_srl_epi16(low, shift), target);
        let high = _mm256_cmpeq_epi16(_mm256_srl_epi16(high, shift), target);
        _mm256_movemask_epi8(_mm256_permute4x64_epi64(
            _mm256_packs_epi16(low, high),
            0xd8,
        )) as u32
    }
);

// Keep shifted IDs in vectors for repeated target comparisons.
#[cfg(target_arch = "x86_64")]
fearless_simd::kernel!(
    #[inline]
    fn sse2_shifted_ids(
        simd: Sse2,
        bytes: &[u8],
        shift: u32,
    ) -> (std::arch::x86_64::__m128i, std::arch::x86_64::__m128i) {
        use std::arch::x86_64::*;

        let shift = _mm_cvtsi32_si128(shift as i32);
        let low: __m128i = u8x16::from_slice(simd, &bytes[..16]).into();
        let high: __m128i = u8x16::from_slice(simd, &bytes[16..32]).into();
        (_mm_srl_epi16(low, shift), _mm_srl_epi16(high, shift))
    }
);

// Keep shifted IDs in vectors for repeated target comparisons.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
fearless_simd::kernel!(
    #[inline]
    fn avx2_shifted_ids(
        simd: Avx2,
        bytes: &[u8],
        shift: u32,
    ) -> (std::arch::x86_64::__m256i, std::arch::x86_64::__m256i) {
        use std::arch::x86_64::*;

        let shift = _mm_cvtsi32_si128(shift as i32);
        let low: __m256i = u8x32::from_slice(simd, &bytes[..32]).into();
        let high: __m256i = u8x32::from_slice(simd, &bytes[32..64]).into();
        (_mm256_srl_epi16(low, shift), _mm256_srl_epi16(high, shift))
    }
);

// Seed both comparison masks with the first target.
#[cfg(target_arch = "x86_64")]
fearless_simd::kernel!(
    #[inline]
    fn sse2_first_match(
        _simd: Sse2,
        low: std::arch::x86_64::__m128i,
        high: std::arch::x86_64::__m128i,
        target: u16,
    ) -> (std::arch::x86_64::__m128i, std::arch::x86_64::__m128i) {
        use std::arch::x86_64::*;

        let target = _mm_set1_epi16(target as i16);
        (_mm_cmpeq_epi16(low, target), _mm_cmpeq_epi16(high, target))
    }
);

// Seed both comparison masks with the first target.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
fearless_simd::kernel!(
    #[inline]
    fn avx2_first_match(
        _simd: Avx2,
        low: std::arch::x86_64::__m256i,
        high: std::arch::x86_64::__m256i,
        target: u16,
    ) -> (std::arch::x86_64::__m256i, std::arch::x86_64::__m256i) {
        use std::arch::x86_64::*;

        let target = _mm256_set1_epi16(target as i16);
        (
            _mm256_cmpeq_epi16(low, target),
            _mm256_cmpeq_epi16(high, target),
        )
    }
);

// Accumulate target matches before extracting slot bits.
#[cfg(target_arch = "x86_64")]
fearless_simd::kernel!(
    #[inline]
    fn sse2_match_id(
        _simd: Sse2,
        low: std::arch::x86_64::__m128i,
        high: std::arch::x86_64::__m128i,
        low_matches: std::arch::x86_64::__m128i,
        high_matches: std::arch::x86_64::__m128i,
        target: u16,
    ) -> (std::arch::x86_64::__m128i, std::arch::x86_64::__m128i) {
        use std::arch::x86_64::*;

        let target = _mm_set1_epi16(target as i16);
        (
            _mm_or_si128(low_matches, _mm_cmpeq_epi16(low, target)),
            _mm_or_si128(high_matches, _mm_cmpeq_epi16(high, target)),
        )
    }
);

// Accumulate target matches before extracting slot bits.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
fearless_simd::kernel!(
    #[inline]
    fn avx2_match_id(
        _simd: Avx2,
        low: std::arch::x86_64::__m256i,
        high: std::arch::x86_64::__m256i,
        low_matches: std::arch::x86_64::__m256i,
        high_matches: std::arch::x86_64::__m256i,
        target: u16,
    ) -> (std::arch::x86_64::__m256i, std::arch::x86_64::__m256i) {
        use std::arch::x86_64::*;

        let target = _mm256_set1_epi16(target as i16);
        (
            _mm256_or_si256(low_matches, _mm256_cmpeq_epi16(low, target)),
            _mm256_or_si256(high_matches, _mm256_cmpeq_epi16(high, target)),
        )
    }
);

// Pack two 16-bit comparison vectors into one slot mask.
#[cfg(target_arch = "x86_64")]
fearless_simd::kernel!(
    #[inline]
    fn sse2_short_mask(
        _simd: Sse2,
        low_matches: std::arch::x86_64::__m128i,
        high_matches: std::arch::x86_64::__m128i,
    ) -> u16 {
        use std::arch::x86_64::*;

        _mm_movemask_epi8(_mm_packs_epi16(low_matches, high_matches)) as u16
    }
);

// Restore slot order after lane-local AVX2 packing.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
fearless_simd::kernel!(
    #[inline]
    fn avx2_short_mask(
        _simd: Avx2,
        low: std::arch::x86_64::__m256i,
        high: std::arch::x86_64::__m256i,
    ) -> u32 {
        use std::arch::x86_64::*;

        let packed = _mm256_packs_epi16(low, high);
        _mm256_movemask_epi8(_mm256_permute4x64_epi64(packed, 0xd8)) as u32
    }
);

// Group views borrow the same descriptor as nodes. Keeping layout and grammar
// metadata there avoids copying either into each scan and returned group.
#[derive(Clone, Copy)]
struct Columns<'tree> {
    root: Node<'tree>,
}

impl<'tree> Columns<'tree> {
    fn new(root: Node<'tree>) -> Self {
        Self { root }
    }

    #[inline]
    fn tree(self) -> &'tree TreeData {
        self.root.data()
    }

    #[inline]
    fn layout(self) -> &'tree Layout<ColumnPointer> {
        &self.tree().layout
    }

    #[inline]
    fn tables(self) -> &'tree GrammarView {
        self.tree().tables()
    }

    #[inline]
    fn encode_kind(self, kind: KindId) -> Option<RemappedKindId> {
        self.tables().remap_kind(kind)
    }

    #[inline]
    fn slice(self, column: ColumnPointer, start: usize, length: usize) -> &'tree [u8] {
        // Callers select valid slots from an allocated column in this borrowed tree.
        unsafe { std::slice::from_raw_parts(column.as_ptr().add(start), length) }
    }

    #[inline]
    fn group_size(self) -> u32 {
        GROUP_SIZE
    }

    #[inline]
    fn byte(self, column: ColumnPointer, index: u32) -> u8 {
        self.tree().byte(column, index)
    }

    #[inline]
    fn short(self, column: ColumnPointer, index: u32) -> u16 {
        self.tree().short(column, index)
    }

    #[inline]
    fn word(self, column: ColumnPointer, index: u32) -> u32 {
        self.tree().word(column, index)
    }

    #[inline]
    fn group(self, index: GroupIx) -> GroupRef<'tree> {
        GroupRef {
            columns: self,
            index,
        }
    }

    #[inline]
    fn first_slot(self, slot: u32) -> u32 {
        self.tree().first_slot(slot)
    }

    #[inline]
    fn previous_slot(self, slot: u32) -> Option<u32> {
        self.tree().previous_slot(slot)
    }

    #[inline]
    fn node(self, slot: u32) -> Node<'tree> {
        self.root.at(SlotIx::new(slot))
    }
}

#[derive(Clone, Copy, Default)]
struct SymbolIndex {
    enabled: bool,
}
impl SymbolIndex {
    fn new(group: &GroupRef<'_>, mut targets: impl Iterator<Item = RemappedKindId>) -> Self {
        Self {
            enabled: group.columns.tree().presence_cache.is_some() && targets.next().is_some(),
        }
    }
    fn enabled(self) -> bool {
        self.enabled
    }
    fn next_group(
        self,
        group: &GroupRef<'_>,
        targets: impl Iterator<Item = RemappedKindId>,
        groups: Range<u32>,
        reverse: bool,
    ) -> Option<u32> {
        let cache = group.columns.tree().presence_cache.as_ref()?;
        targets
            .filter_map(|target| {
                cache.next_group(
                    groups.clone(),
                    target.get() as usize,
                    group.columns.tree().groups(),
                    reverse,
                )
            })
            .reduce(|previous, candidate| {
                if reverse {
                    previous.min(candidate)
                } else {
                    previous.max(candidate)
                }
            })
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
    fn pop(&mut self, descending: bool) -> Option<GroupSlotIx> {
        if self.is_empty() {
            return None;
        }
        let slot = if descending {
            63 - self.0.leading_zeros()
        } else {
            self.0.trailing_zeros()
        };
        self.0 &= !(1u64 << slot);
        Some(GroupSlotIx(slot))
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
    index: GroupIx,
}
impl<'tree> GroupRef<'tree> {
    #[cfg(any(
        feature = "typed-query-scan",
        feature = "typed-presence-scan",
        feature = "typed-seek"
    ))]
    pub(crate) fn new(root: Node<'tree>) -> Self {
        let columns = Columns::new(root);
        columns.group(root.slot().group())
    }

    #[cfg(any(
        feature = "typed-query-scan",
        feature = "typed-presence-scan",
        feature = "typed-seek"
    ))]
    pub(crate) fn at_group(mut self, index: GroupIx) -> Self {
        debug_assert!(index.get() < self.columns.root.data().groups());
        self.index = index;
        self
    }

    #[cfg(feature = "typed-seek")]
    pub(crate) fn starts_before<const POINTS: bool>(&self, start: u64) -> Mask {
        let candidates = self.valid_mask();
        if POINTS {
            let start = PackedPoint(start);
            PointPositions::<true> { group: self }
                .start()
                .retain(candidates, (Unbounded, Included(start)))
        } else {
            BytePositions(self)
                .start()
                .retain(candidates, (Unbounded, Included(start as usize)))
        }
    }

    #[cfg(feature = "typed-seek")]
    pub(crate) fn ends_after<const POINTS: bool>(&self, start: u64, end: u64) -> Mask {
        let candidates = self.valid_mask();
        // Nonempty nodes ending exactly at the query start cannot contain it.
        if POINTS {
            let (start, end) = (PackedPoint(start), PackedPoint(end));
            let lower = if start == end {
                Excluded(start)
            } else {
                Included(end)
            };
            PointPositions::<true> { group: self }
                .end()
                .retain(candidates, (lower, Unbounded))
        } else {
            let lower = if start == end {
                Excluded(start as usize)
            } else {
                Included(end as usize)
            };
            BytePositions(self)
                .end()
                .retain(candidates, (lower, Unbounded))
        }
    }

    pub fn index(self) -> u32 {
        self.index.get()
    }
    pub fn first_slot(self) -> SlotIx {
        self.index.first_slot()
    }
    #[inline]
    pub fn valid_mask(self) -> Mask {
        Mask::lower(self.used())
    }
    /// Resolve a group-relative physical slot; waste and out-of-group slots fail.
    pub fn node(self, slot: u32) -> Option<Node<'tree>> {
        (slot < self.used()).then(|| self.columns.root.at(self.index.slot(GroupSlotIx(slot))))
    }
    #[inline]
    fn used(self) -> u32 {
        self.columns.group_size() - self.columns.tree().waste(self.index.get())
    }
    #[inline]
    fn kind(self, slot: u32) -> KindId {
        let columns = self.columns;
        let symbol = columns.short(columns.layout().symbol, self.first_slot().get() + slot)
            >> columns.tables().symbol_shift;
        columns.tables().decode_kind(RemappedKindId(symbol))
    }

    #[inline]
    fn equal_ids(&self, column: ColumnPointer, shift: u32, target: u16, candidates: Mask) -> Mask {
        if candidates.0.is_power_of_two() {
            return candidates.retain(|slot| {
                self.columns.short(column, self.first_slot().get() + slot) >> shift == target
            });
        }
        let start = self.first_slot().get() as usize * 2;
        let bytes = self
            .columns
            .slice(column, start, self.columns.group_size() as usize * 2);
        let mut matches = 0;
        #[cfg(target_arch = "x86_64")]
        {
            #[cfg(target_feature = "avx2")]
            if GROUP_SIZE == 32 {
                if let Some(simd) = Level::baseline()
                    .as_avx2()
                    .or_else(|| Level::new().as_avx2())
                {
                    return candidates
                        .intersection(Mask(avx2_equal_ids(simd, bytes, shift, target) as u64));
                }
            }
            let simd = Level::baseline().as_sse2().unwrap();
            let mut chunks = bytes.chunks_exact(32);
            // A 16-slot group needs no loop-carried mask or chunk offset.
            if let Some(first) = chunks.next() {
                matches = sse2_equal_ids(simd, first, shift, target) as u64;
            }
            for (index, bytes) in chunks.enumerate() {
                matches |=
                    (sse2_equal_ids(simd, bytes, shift, target) as u64) << ((index + 1) * 16);
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
    fn equal_id_set<I: crate::Id>(
        &self,
        column: ColumnPointer,
        shift: u32,
        targets: &[I],
        candidates: Mask,
    ) -> Mask {
        if targets.is_empty() {
            return Mask::default();
        }
        if targets.len() == 1 {
            return self.equal_ids(column, shift, targets[0].raw(), candidates);
        }
        if targets.len() == 2 {
            return Mask(
                self.equal_ids(column, shift, targets[0].raw(), candidates)
                    .0
                    | self
                        .equal_ids(column, shift, targets[1].raw(), candidates)
                        .0,
            );
        }
        if candidates.0.is_power_of_two() {
            let slot = self.first_slot().get() + candidates.0.trailing_zeros();
            let value = self.columns.short(column, slot) >> shift;
            return if targets.iter().any(|target| target.raw() == value) {
                candidates
            } else {
                Mask::default()
            };
        }
        #[cfg(target_arch = "x86_64")]
        {
            let start = self.first_slot().get() as usize * 2;
            let bytes = self
                .columns
                .slice(column, start, self.columns.group_size() as usize * 2);
            #[cfg(target_feature = "avx2")]
            if GROUP_SIZE == 32 {
                if let Some(simd) = Level::baseline()
                    .as_avx2()
                    .or_else(|| Level::new().as_avx2())
                {
                    let (low, high) = avx2_shifted_ids(simd, bytes, shift);
                    let (mut low_matches, mut high_matches) =
                        avx2_first_match(simd, low, high, targets[0].raw());
                    for &target in &targets[1..] {
                        (low_matches, high_matches) =
                            avx2_match_id(simd, low, high, low_matches, high_matches, target.raw());
                    }
                    return candidates.intersection(Mask(avx2_short_mask(
                        simd,
                        low_matches,
                        high_matches,
                    ) as u64));
                }
            }
            let mut matches = 0;
            // Each group chunk covers both vector loads; fixed-array callers
            // expose the target count for loop unrolling.
            let simd = Level::baseline().as_sse2().unwrap();
            for (index, bytes) in bytes.chunks_exact(32).enumerate() {
                let (low, high) = sse2_shifted_ids(simd, bytes, shift);
                let (mut low_matches, mut high_matches) =
                    sse2_first_match(simd, low, high, targets[0].raw());
                for &target in &targets[1..] {
                    (low_matches, high_matches) =
                        sse2_match_id(simd, low, high, low_matches, high_matches, target.raw());
                }
                matches |=
                    (sse2_short_mask(simd, low_matches, high_matches) as u64) << (index * 16);
            }
            candidates.intersection(Mask(matches))
        }
        #[cfg(not(target_arch = "x86_64"))]
        candidates.retain(|slot| {
            targets.iter().any(|target| {
                target.raw() == self.columns.short(column, self.first_slot().get() + slot) >> shift
            })
        })
    }
    #[inline]
    fn bitmap(self, column: ColumnPointer) -> u64 {
        let start = self.first_slot().get() / 8;
        let mut bits = 0;
        for byte in 0..self.columns.group_size() / 8 {
            bits |= u64::from(self.columns.byte(column, start + byte)) << (byte * 8);
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
            base: self.group.columns.root.at(self.group.first_slot()),
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
        self.matches
            .pop(self.descending ^ back)
            .map(|slot| self.base.at(self.base.slot().group().slot(slot)))
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
    fn next_matching<P: Predicate>(&mut self, predicate: &mut P) -> Option<Mask> {
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
    /// Fold remaining groups after the iterator's current fragment.
    #[inline]
    fn fold_nodes<B, F>(self, accumulator: B, fold: F) -> B
    where
        F: FnMut(B, Node<'tree>) -> B,
    {
        fold_nodes(self, accumulator, fold)
    }
    #[inline]
    fn count(self) -> usize {
        self.count_matches(Identity)
    }
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        count_groups(self, predicate)
    }
}
#[inline]
fn fold_nodes<'tree, S: GroupScan<'tree>, B, F>(mut source: S, mut accumulator: B, mut fold: F) -> B
where
    F: FnMut(B, Node<'tree>) -> B,
{
    let columns = source.group().columns;
    while let Some(slots) = source.next_slots() {
        let base = source.group().first_slot().get();
        accumulator = slots.fold(accumulator, |accumulator, slot| {
            fold(accumulator, columns.node(base + slot))
        });
    }
    accumulator
}
#[inline(always)]
fn count_groups<'tree, S: GroupScan<'tree>, P: Predicate>(
    mut source: S,
    mut predicate: P,
) -> usize {
    let mut count = 0;
    while let Some(matches) = source.next_matching(&mut predicate) {
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
    #[inline(always)]
    pub fn filter_kind_ids<K: IdSelection>(
        self,
        kinds: K,
    ) -> Scan<'tree, Filtered<S, K::KindPredicate>> {
        let predicate = kinds.into_kind_predicate(self.source.group());
        self.filtered(predicate)
    }
    /// `None` matches nodes with no field, including the tree root.
    pub fn filter_field_id(
        self,
        field: impl Into<Option<FieldId>>,
    ) -> Scan<'tree, Filtered<S, FieldPredicate>> {
        self.filtered(FieldPredicate(field.into()))
    }
    /// Match any selected field ID. `None` includes nodes with no field; an empty
    /// selection matches nothing. Arrays specialize the kernel for their length.
    pub fn filter_field_ids<F: FieldSelection>(
        self,
        fields: F,
    ) -> Scan<'tree, Filtered<S, F::FieldPredicate>> {
        self.filtered(fields.into_field_predicate())
    }
    pub fn filter_supertype_id(
        self,
        supertype: GrammarKindId,
    ) -> Scan<'tree, Filtered<S, SupertypeId>> {
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
    #[inline(always)]
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
    fn fold<B, F>(self, mut accumulator: B, mut fold: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        let columns = self.source.group().columns;
        accumulator = self.slots.fold(accumulator, |accumulator, slot| {
            fold(accumulator, columns.node(self.base + slot))
        });
        self.source.fold_nodes(accumulator, fold)
    }
    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(slot) = self.slots.next() {
                return Some(self.source.group().columns.node(self.base + slot));
            }
            self.slots = self.source.next_slots()?;
            let group = self.source.group();
            self.base = group.first_slot().get();
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
        self.matches.pop(S::DESCENDING).map(GroupSlotIx::get)
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
        let first = columns.first_slot(root.slot().get());
        Self {
            group: columns.group(root.slot().group()),
            groups: (first >> GROUP_SIZE.trailing_zeros())
                ..(root.slot().get() >> GROUP_SIZE.trailing_zeros()) + 1,
            slots: first..root.slot().get() + 1,
        }
    }
    #[inline]
    fn mask(&self) -> Mask {
        let base = self.group.first_slot().get();
        let first = self.slots.start.saturating_sub(base);
        let end = (self.slots.end - base).min(self.group.used());
        Mask(Mask::lower(end).0 & !Mask::lower(first).0)
    }
    #[inline]
    fn next_range<const REVERSE: bool>(&mut self) -> Option<Range<u32>> {
        loop {
            self.group.index = GroupIx(if REVERSE {
                self.groups.next()?
            } else {
                self.groups.next_back()?
            });
            let base = self.group.first_slot().get();
            let first = self.slots.start.saturating_sub(base);
            let end = (self.slots.end - base).min(self.group.used());
            if first < end {
                return Some(first..end);
            }
        }
    }
    #[inline(always)]
    fn next_matching_group<
        const REVERSE: bool,
        const SUBTREES: bool,
        const INDEXED: bool,
        P: Predicate,
    >(
        &mut self,
        predicate: &mut P,
    ) -> Option<Mask> {
        loop {
            // Bitmap jumps can bypass the ancestor that rejects a whole subtree.
            if INDEXED && SUBTREES && predicate.has_group_index() {
                if self.groups.is_empty() {
                    return None;
                }
                self.group.index = GroupIx(self.groups.end - 1);
                if predicate.excludes_subtrees(&self.group) {
                    let span = self.group.columns.word(
                        self.group.columns.layout().span_base,
                        self.group.index.get(),
                    );
                    self.groups.end = if span == 0 {
                        self.group.index.get()
                    } else {
                        (self.group.first_slot().get() - span)
                            .div_ceil(self.group.columns.group_size())
                            .min(self.group.index.get())
                            .max(self.groups.start)
                    };
                    continue;
                }
            }
            self.group.index = GroupIx(if INDEXED {
                let Some(index) = predicate.next_group(&self.group, self.groups.clone(), REVERSE)
                else {
                    self.groups.end = self.groups.start;
                    return None;
                };
                if REVERSE {
                    self.groups.start = index + 1;
                } else {
                    self.groups.end = index;
                }
                index
            } else if REVERSE {
                self.groups.next()?
            } else {
                self.groups.next_back()?
            });
            if SUBTREES && predicate.excludes_subtrees(&self.group) {
                // The last node in preorder occupies the group's first slot.
                // Its descendants end no later, so their whole groups can be skipped.
                let span = self.group.columns.word(
                    self.group.columns.layout().span_base,
                    self.group.index.get(),
                );
                if span != 0 {
                    // The base alone is a conservative span; avoid delta loads
                    // and short jumps when all spans fit in a byte.
                    let end = (self.group.first_slot().get() - span)
                        .div_ceil(self.group.columns.group_size());
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
    #[inline(always)]
    fn count_indexed<const SUBTREES: bool, P: Predicate>(mut self, predicate: &mut P) -> usize {
        let mut count = 0;
        while let Some(matches) = self.next_matching_group::<false, SUBTREES, true, _>(predicate) {
            count += matches.count_ones() as usize;
        }
        count
    }
    // Own the predicate and isolate the loop from index setup so column metadata
    // can stay in registers. The call is paid once per flat scan.
    #[inline(never)]
    fn count_flat<P: Predicate>(mut self, mut predicate: P) -> usize {
        let mut count = 0;
        while let Some(matches) = if predicate.has_subtree_bound() {
            self.next_matching_group::<false, true, false, _>(&mut predicate)
        } else {
            self.next_matching_group::<false, false, false, _>(&mut predicate)
        } {
            count += matches.count_ones() as usize;
        }
        count
    }
    #[inline(always)]
    fn next_indexed<const REVERSE: bool, const SUBTREES: bool, P: Predicate>(
        &mut self,
        predicate: &mut P,
    ) -> Option<Mask> {
        let (index, matches) = indexed_group::<REVERSE, SUBTREES, _>(
            &self.group,
            &mut self.groups,
            self.slots.clone(),
            predicate,
        );
        self.group.index = index;
        matches
    }
}
// Only the remaining group interval is mutable across this call. Flat scan
// kernels can keep their column metadata in registers between matching groups.
#[inline(always)]
fn indexed_group<const REVERSE: bool, const SUBTREES: bool, P: Predicate>(
    group: &GroupRef<'_>,
    groups: &mut Range<u32>,
    slots: Range<u32>,
    predicate: &mut P,
) -> (GroupIx, Option<Mask>) {
    let mut source = Preorder {
        group: *group,
        groups: groups.clone(),
        slots,
    };
    let matches = source.next_matching_group::<REVERSE, SUBTREES, true, _>(predicate);
    *groups = source.groups;
    (source.group.index, matches)
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
                let group = self.group.columns.group(GroupIx(index));
                let base = group.first_slot().get();
                let first = self.slots.start.saturating_sub(base);
                let end = (self.slots.end - base).min(group.used());
                end.saturating_sub(first) as usize
            })
            .sum()
    }
    #[inline(always)]
    fn count_matches<P: Predicate>(self, mut predicate: P) -> usize {
        if !predicate.has_group_index() {
            predicate.count_flat(self)
        } else if predicate.has_subtree_bound() {
            self.count_indexed::<true, _>(&mut predicate)
        } else {
            self.count_indexed::<false, _>(&mut predicate)
        }
    }
    #[inline]
    fn group(&self) -> &GroupRef<'tree> {
        &self.group
    }
    #[inline(always)]
    fn next_matching<P: Predicate>(&mut self, predicate: &mut P) -> Option<Mask> {
        match (predicate.has_group_index(), predicate.has_subtree_bound()) {
            (true, true) => self.next_indexed::<false, true, _>(predicate),
            (true, false) => self.next_indexed::<false, false, _>(predicate),
            (false, true) => self.next_matching_group::<false, true, false, _>(predicate),
            (false, false) => self.next_matching_group::<false, false, false, _>(predicate),
        }
    }
    #[inline(always)]
    fn next_mask(&mut self) -> Option<Mask> {
        loop {
            self.group.index = GroupIx(self.groups.next_back()?);
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
    fn next_matching<P: Predicate>(&mut self, predicate: &mut P) -> Option<Mask> {
        if predicate.has_group_index() {
            self.0
                .next_matching_group::<true, false, true, _>(predicate)
        } else {
            self.0
                .next_matching_group::<true, false, false, _>(predicate)
        }
    }
    #[inline(always)]
    fn next_mask(&mut self) -> Option<Mask> {
        loop {
            self.0.group.index = GroupIx(self.0.groups.next()?);
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
            self.next = Some(columns.root.slot().get());
            self.first = columns.first_slot(columns.root.slot().get());
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
            group: columns.group(columns.root.slot().group()),
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
        self.group.index = GroupIx(slot >> GROUP_SIZE.trailing_zeros());
        Some(Mask(1u64 << SlotIx::new(slot).in_group().get()))
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
            columns.root.slot().get()
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
        self.group.index = GroupIx(slot >> GROUP_SIZE.trailing_zeros());
        Some(Mask(1u64 << SlotIx::new(slot).in_group().get()))
    }
}

/// Byte-offset coordinates.
pub struct Bytes;

/// Row/column coordinates, using `(0, byte_offset)` when points are not stored.
pub struct Points {
    stored: bool,
}

struct BytePositions<'group, 'tree>(&'group GroupRef<'tree>);
struct PointPositions<'group, 'tree, const STORED: bool> {
    group: &'group GroupRef<'tree>,
}
struct AbsolutePointColumn<'tree, const END: bool> {
    points: &'tree PointData,
    first_slot: u32,
    count: u32,
}
impl<const END: bool> PositionColumn for AbsolutePointColumn<'_, END> {
    type Position = PackedPoint;
    fn minimum(&self) -> PackedPoint {
        (0..self.count)
            .map(|slot| self.get(slot))
            .min()
            .unwrap_or(PackedPoint(0))
    }
    fn maximum(&self) -> PackedPoint {
        (0..self.count)
            .map(|slot| self.get(slot))
            .max()
            .unwrap_or(PackedPoint(0))
    }
    fn get(&self, slot: u32) -> PackedPoint {
        if END {
            self.points.end(self.first_slot + slot)
        } else {
            self.points.start(self.first_slot + slot)
        }
    }
}
struct ByteColumn<'tree, const END: bool> {
    base: usize,
    deltas: ColumnDeltas<'tree>,
}
struct PointColumn<'tree, const END: bool, const STORED: bool> {
    base: PackedPoint,
    deltas: ColumnDeltas<'tree>,
}

#[derive(Clone, Copy)]
struct ColumnDeltas<'tree> {
    pointer: ColumnPointer,
    length: usize,
    borrow: PhantomData<&'tree [u8]>,
}
impl<'tree> ColumnDeltas<'tree> {
    #[inline]
    fn slice(self) -> &'tree [u8] {
        // The group borrows a live tree and spans allocated column slots.
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.length) }
    }
}

#[inline]
fn column_deltas<'tree>(
    group: &GroupRef<'tree>,
    column: ColumnPointer,
    width: usize,
) -> ColumnDeltas<'tree> {
    ColumnDeltas {
        pointer: column.add(group.first_slot().get() as usize * width),
        length: group.columns.group_size() as usize * width,
        borrow: PhantomData,
    }
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
fn point_cutoff(base: PackedPoint, position: PackedPoint, inclusive: bool) -> u32 {
    let (base, position) = (base.get(), position.get());
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
            let mut matches = 0;
            let length = bounds.end - bounds.start;
            #[cfg(target_feature = "avx2")]
            if GROUP_SIZE == 32 && deltas.len() == (if WIDE { 64 } else { 32 }) {
                if let Some(simd) = Level::baseline()
                    .as_avx2()
                    .or_else(|| Level::new().as_avx2())
                {
                    matches = if WIDE {
                        avx2_retain_wide(simd, deltas, bounds.start, length) as u64
                    } else {
                        avx2_retain_narrow(simd, deltas, bounds.start, length) as u64
                    };
                    return candidates.intersection(Mask(matches));
                }
            }
            // Bias the wrapped delta-minus-lower by the sign bit so signed SIMD
            // comparisons implement an unsigned interval test. Checked chunks
            // cover complete groups; candidate clipping excludes waste lanes.
            let simd = Level::baseline().as_sse2().unwrap();
            if WIDE {
                for (index, bytes) in deltas.chunks_exact(32).enumerate() {
                    matches |= (sse2_retain_wide(simd, bytes, bounds.start, length) as u64)
                        << (index * 16);
                }
            } else {
                for (index, bytes) in deltas.chunks_exact(16).enumerate() {
                    matches |= (sse2_retain_narrow(simd, bytes, bounds.start, length) as u64)
                        << (index * 16);
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
    type Position = PackedPoint;
    #[inline]
    fn minimum(&self) -> PackedPoint {
        if END {
            self.base
                .saturating_sub(if STORED { (255 << 32) | 255 } else { 65535 })
        } else {
            self.base
        }
    }
    #[inline]
    fn maximum(&self) -> PackedPoint {
        if END {
            self.base
        } else {
            self.base
                .saturating_add(if STORED { (255 << 32) | 255 } else { 255 })
        }
    }
    #[inline]
    fn get(&self, slot: u32) -> PackedPoint {
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
    fn retain(&self, candidates: Mask, bounds: (Bound<PackedPoint>, Bound<PackedPoint>)) -> Mask {
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
                byte_cutoff(base.get(), position.get(), inclusive, limit)
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
impl<C: PositionColumn<Position = PackedPoint>> PositionColumn for UnpackedColumn<C> {
    type Position = Point;
    #[inline]
    fn minimum(&self) -> Point {
        self.0.minimum().point()
    }
    #[inline]
    fn maximum(&self) -> Point {
        self.0.maximum().point()
    }
    #[inline]
    fn get(&self, slot: u32) -> Point {
        self.0.get(slot).point()
    }
}
impl<P: Positions<Position = PackedPoint>> Positions for UnpackedPositions<P> {
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
                .word(group.columns.layout().start_byte_base, group.index.get())
                as usize,
            deltas: column_deltas(group, group.columns.layout().start_byte_delta, 1),
        }
    }
    #[inline]
    fn end(&self) -> Self::End {
        let group = self.0;
        ByteColumn {
            base: group
                .columns
                .word(group.columns.layout().end_byte_base, group.index.get())
                as usize,
            deltas: column_deltas(group, group.columns.layout().end_byte_delta, 2),
        }
    }
}
impl<'tree> Positions for PointPositions<'_, 'tree, false> {
    type Position = PackedPoint;
    type Start = PointColumn<'tree, false, false>;
    type End = PointColumn<'tree, true, false>;
    #[inline]
    fn start(&self) -> Self::Start {
        let column = BytePositions(self.group).start();
        PointColumn {
            base: PackedPoint(column.base as u64),
            deltas: column.deltas,
        }
    }
    #[inline]
    fn end(&self) -> Self::End {
        let column = BytePositions(self.group).end();
        PointColumn {
            base: PackedPoint(column.base as u64),
            deltas: column.deltas,
        }
    }
}
impl<'tree> Positions for PointPositions<'_, 'tree, true> {
    type Position = PackedPoint;
    type Start = AbsolutePointColumn<'tree, false>;
    type End = AbsolutePointColumn<'tree, true>;
    fn start(&self) -> Self::Start {
        AbsolutePointColumn {
            points: self.group.columns.tree().point_data.as_ref().unwrap(),
            first_slot: self.group.first_slot().get(),
            count: self.group.used(),
        }
    }
    fn end(&self) -> Self::End {
        AbsolutePointColumn {
            points: self.group.columns.tree().point_data.as_ref().unwrap(),
            first_slot: self.group.first_slot().get(),
            count: self.group.used(),
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
            .word(group.columns.layout().start_byte_base, group.index.get()) as usize
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
        Self {
            stored: group.columns.tree().has_points(),
        }
    }
    #[inline(always)]
    fn start_minimum(&self, group: &GroupRef<'_>) -> Point {
        if !self.stored {
            Point::new(0, Bytes.start_minimum(group))
        } else {
            // Row and column bases are independent minima. Only the earliest
            // live node gives an actual start position ordered across groups.
            PointPositions::<true> { group }
                .start()
                .get(group.used() - 1)
                .point()
        }
    }
    #[inline]
    fn end_before(&self, group: &GroupRef<'_>, bound: Bound<Point>) -> bool {
        let end = if !self.stored {
            PackedPoint(BytePositions(group).end().maximum() as u64)
        } else {
            PointPositions::<true> { group }.end().maximum()
        };
        match bound {
            Included(limit) | Excluded(limit) => {
                if let Some(limit) = PackedPoint::from_point(limit) {
                    before_bound(
                        end,
                        match bound {
                            Included(_) => Included(limit),
                            _ => Excluded(limit),
                        },
                    )
                } else {
                    before_bound(end.point(), bound)
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
        if !self.stored {
            retain_points(relation, PointPositions::<false> { group }, candidates)
        } else {
            retain_points(relation, PointPositions::<true> { group }, candidates)
        }
    }
}

#[inline(always)]
fn retain_points<R: Relation<Point>, P: Positions<Position = PackedPoint>>(
    relation: &R,
    positions: P,
    candidates: impl FnOnce() -> Mask,
) -> Mask {
    if let Some(packed) = relation.try_map(PackedPoint::from_point) {
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
            let start = coordinates.start_minimum(&self.group.columns.group(GroupIx(middle)));
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
                let start = coordinates.start_minimum(&self.group.columns.group(GroupIx(middle)));
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
    #[inline(always)]
    fn count_matches<P: Predicate>(self, predicate: P) -> usize {
        if self.selection.relation.is_empty() {
            return 0;
        }
        self.source.count_matches(And(self.selection, predicate))
    }
    #[inline(always)]
    fn next_mask(&mut self) -> Option<Mask> {
        if self.selection.relation.is_empty() {
            return None;
        }
        self.source.next_matching(&mut self.selection)
    }
    #[inline(always)]
    fn next_matching<P: Predicate>(&mut self, predicate: &mut P) -> Option<Mask> {
        if self.selection.relation.is_empty() {
            return None;
        }
        self.source
            .next_matching(&mut And(&mut self.selection, predicate))
    }
}

pub trait Predicate: sealed::Predicate {
    #[inline]
    fn fold_nodes<'tree, S: GroupScan<'tree>, B, F>(self, source: S, accumulator: B, fold: F) -> B
    where
        Self: Sized,
        F: FnMut(B, Node<'tree>) -> B,
    {
        fold_nodes(
            Filtered {
                source,
                predicate: self,
            },
            accumulator,
            fold,
        )
    }
    /// Comparison state without group-index traversal.
    #[inline(always)]
    fn flat(&self) -> impl Predicate {
        self
    }

    #[inline(always)]
    fn into_flat(self) -> impl Predicate
    where
        Self: Sized,
    {
        self
    }

    #[inline(always)]
    fn count_flat(self, source: Preorder<'_>) -> usize
    where
        Self: Sized,
    {
        source.count_flat(self.into_flat())
    }

    #[inline(always)]
    fn has_group_index(&self) -> bool {
        false
    }
    /// The nearest possible match in the remaining physical group interval.
    #[inline(always)]
    fn next_group(
        &mut self,
        _group: &GroupRef<'_>,
        groups: Range<u32>,
        reverse: bool,
    ) -> Option<u32> {
        if groups.is_empty() {
            None
        } else {
            Some(if reverse {
                groups.start
            } else {
                groups.end - 1
            })
        }
    }
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

// Shared predicate views carry comparison and subtree bounds.
// Preparation and indexed traversal use the owning predicate.
impl<P: Predicate + ?Sized> sealed::Predicate for &P {}
impl<P: Predicate + ?Sized> Predicate for &P {
    #[inline(always)]
    fn has_subtree_bound(&self) -> bool {
        P::has_subtree_bound(self)
    }

    #[inline(always)]
    fn excludes_subtrees(&self, group: &GroupRef<'_>) -> bool {
        P::excludes_subtrees(self, group)
    }

    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        P::retain_matches(self, group, candidates)
    }

    #[inline(always)]
    fn retain_group(&self, group: &GroupRef<'_>, candidates: impl FnOnce() -> Mask) -> Mask {
        P::retain_group(self, group, candidates)
    }
}

impl<P: Predicate> sealed::Predicate for &mut P {}
impl<P: Predicate> Predicate for &mut P {
    #[inline(always)]
    fn flat(&self) -> impl Predicate {
        P::flat(self)
    }

    #[inline(always)]
    fn has_group_index(&self) -> bool {
        P::has_group_index(self)
    }
    #[inline(always)]
    fn next_group(
        &mut self,
        group: &GroupRef<'_>,
        groups: Range<u32>,
        reverse: bool,
    ) -> Option<u32> {
        P::next_group(self, group, groups, reverse)
    }
    #[inline(always)]
    fn has_subtree_bound(&self) -> bool {
        P::has_subtree_bound(self)
    }
    #[inline(always)]
    fn excludes_subtrees(&self, group: &GroupRef<'_>) -> bool {
        P::excludes_subtrees(self, group)
    }
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        P::retain_matches(self, group, candidates)
    }
    #[inline(always)]
    fn retain_group(&self, group: &GroupRef<'_>, candidates: impl FnOnce() -> Mask) -> Mask {
        P::retain_group(self, group, candidates)
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
    fn flat(&self) -> impl Predicate {
        And(self.0.flat(), self.1.flat())
    }

    #[inline(always)]
    fn into_flat(self) -> impl Predicate {
        And(self.0.into_flat(), self.1.into_flat())
    }

    #[inline(always)]
    fn has_group_index(&self) -> bool {
        self.0.has_group_index() || self.1.has_group_index()
    }
    #[inline(always)]
    fn next_group(
        &mut self,
        group: &GroupRef<'_>,
        groups: Range<u32>,
        reverse: bool,
    ) -> Option<u32> {
        if self.0.has_group_index() {
            self.0.next_group(group, groups, reverse)
        } else {
            self.1.next_group(group, groups, reverse)
        }
    }
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
    fn fold_nodes<B, F>(self, accumulator: B, fold: F) -> B
    where
        F: FnMut(B, Node<'tree>) -> B,
    {
        self.predicate.fold_nodes(self.source, accumulator, fold)
    }

    #[inline(always)]
    fn count(self) -> usize {
        // The default adds Identity, hiding standalone count specializations
        // behind And even when this is the only filter.
        self.source.count_matches(self.predicate)
    }

    #[inline(always)]
    fn count_matches<Q: Predicate>(self, predicate: Q) -> usize {
        self.source.count_matches(And(self.predicate, predicate))
    }
    #[inline(always)]
    fn next_mask(&mut self) -> Option<Mask> {
        if self.predicate.has_group_index() {
            return self.source.next_matching(&mut self.predicate);
        }

        let predicate = self.predicate.flat();
        loop {
            let candidates = self.source.next_mask()?;
            let matches = predicate.retain_matches(self.source.group(), candidates);
            if !matches.is_empty() {
                return Some(matches);
            }
        }
    }
    #[inline(always)]
    fn next_matching<Q: Predicate>(&mut self, predicate: &mut Q) -> Option<Mask> {
        if predicate.has_group_index() {
            return self
                .source
                .next_matching(&mut And(&mut self.predicate, predicate));
        }

        let predicate = predicate.flat();
        loop {
            let candidates = self.next_mask()?;
            let matches = predicate.retain_matches(self.group(), candidates);
            if !matches.is_empty() {
                return Some(matches);
            }
        }
    }
}

/// Kind selections accepted by scans. Array lengths specialize the scan.
pub trait IdSelection: sealed::IdSelection {
    type KindPredicate: Predicate;
    fn into_kind_predicate(self, group: &GroupRef<'_>) -> Self::KindPredicate;
    fn contains_id(&self, id: KindId) -> bool;
    fn is_empty(&self) -> bool;
}
impl sealed::IdSelection for &KindSet {}
impl<'ids> IdSelection for &'ids KindSet {
    type KindPredicate = KindIds<'ids>;
    fn into_kind_predicate(self, _: &GroupRef<'_>) -> Self::KindPredicate {
        KindIds {
            strategy: KindStrategy::Multiple(self),
            index: SymbolIndex::default(),
        }
    }
    fn contains_id(&self, id: KindId) -> bool {
        self.contains(id)
    }
    fn is_empty(&self) -> bool {
        KindSet::is_empty(self)
    }
}
impl<const N: usize> sealed::IdSelection for [KindId; N] {}
impl<const N: usize> IdSelection for [KindId; N] {
    type KindPredicate = ArrayKindIds<N>;
    #[inline(always)]
    fn into_kind_predicate(self, group: &GroupRef<'_>) -> Self::KindPredicate {
        let encode = |kind| group.columns.encode_kind(kind);
        let first = self.iter().copied().find_map(encode);
        // Repeating a valid target preserves membership without an invalid sentinel.
        let ids = self.map(|kind| encode(kind).or(first).unwrap_or_default());
        ArrayKindIds {
            values: ArrayKindValues {
                ids,
                empty: first.is_none(),
            },
            index: SymbolIndex::default(),
        }
    }
    fn contains_id(&self, id: KindId) -> bool {
        self.contains(&id)
    }
    fn is_empty(&self) -> bool {
        N == 0
    }
}
impl<const N: usize> sealed::IdSelection for &[KindId; N] {}
impl<const N: usize> IdSelection for &[KindId; N] {
    type KindPredicate = ArrayKindIds<N>;
    #[inline(always)]
    fn into_kind_predicate(self, group: &GroupRef<'_>) -> Self::KindPredicate {
        (*self).into_kind_predicate(group)
    }
    fn contains_id(&self, id: KindId) -> bool {
        self.contains(&id)
    }
    fn is_empty(&self) -> bool {
        N == 0
    }
}

/// Field selections accepted by scans; `None` selects nodes with no field.
pub trait FieldSelection: sealed::IdSelection {
    type FieldPredicate: Predicate;
    fn into_field_predicate(self) -> Self::FieldPredicate;
}
impl sealed::IdSelection for &FieldSet {}
impl<'ids> FieldSelection for &'ids FieldSet {
    type FieldPredicate = FieldIds<'ids>;
    fn into_field_predicate(self) -> Self::FieldPredicate {
        FieldIds(self)
    }
}
impl<const N: usize> sealed::IdSelection for [Option<FieldId>; N] {}
impl<const N: usize> FieldSelection for [Option<FieldId>; N] {
    type FieldPredicate = ArrayFieldIds<N>;
    fn into_field_predicate(self) -> Self::FieldPredicate {
        ArrayFieldIds(self)
    }
}
impl<const N: usize> sealed::IdSelection for [FieldId; N] {}
impl<const N: usize> FieldSelection for [FieldId; N] {
    type FieldPredicate = ArrayFieldIds<N>;
    fn into_field_predicate(self) -> Self::FieldPredicate {
        ArrayFieldIds(self.map(Some))
    }
}
impl<const N: usize> sealed::IdSelection for &[FieldId; N] {}
impl<const N: usize> FieldSelection for &[FieldId; N] {
    type FieldPredicate = ArrayFieldIds<N>;
    fn into_field_predicate(self) -> Self::FieldPredicate {
        (*self).into_field_predicate()
    }
}
impl<const N: usize> sealed::IdSelection for &[Option<FieldId>; N] {}
impl<const N: usize> FieldSelection for &[Option<FieldId>; N] {
    type FieldPredicate = ArrayFieldIds<N>;
    fn into_field_predicate(self) -> Self::FieldPredicate {
        (*self).into_field_predicate()
    }
}

pub struct ArrayKindIds<const N: usize> {
    values: ArrayKindValues<N>,
    index: SymbolIndex,
}

// Flat loops own only their encoded comparison values.
#[derive(Clone, Copy)]
struct ArrayKindValues<const N: usize> {
    ids: [RemappedKindId; N],
    empty: bool,
}

// Cache column parameters so singleton scans do not reread the grammar between groups.
struct KindPredicate {
    target: RemappedKindId,
    column: ColumnPointer,
    shift: u32,
}

impl KindPredicate {
    #[inline(always)]
    fn new(columns: Columns<'_>, target: RemappedKindId) -> Self {
        Self {
            target,
            column: columns.layout().symbol,
            shift: columns.tables().symbol_shift as u32,
        }
    }
}

impl sealed::Predicate for KindPredicate {}
impl Predicate for KindPredicate {
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        group.equal_ids(self.column, self.shift, self.target.get(), candidates)
    }
}

impl<const N: usize> sealed::Predicate for ArrayKindValues<N> {}
impl<const N: usize> Predicate for ArrayKindValues<N> {
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        if self.empty {
            return Mask::default();
        }

        group.equal_id_set(
            group.columns.layout().symbol,
            group.columns.tables().symbol_shift as u32,
            &self.ids,
            candidates,
        )
    }
}

impl<const N: usize> sealed::Predicate for ArrayKindIds<N> {}
impl<const N: usize> Predicate for ArrayKindIds<N> {
    #[inline(always)]
    fn count_flat(self, source: Preorder<'_>) -> usize {
        if self.values.empty {
            0
        } else if N == 1 {
            let predicate = KindPredicate::new(source.group.columns, self.values.ids[0]);
            source.count_flat(predicate)
        } else {
            source.count_flat(self.values)
        }
    }

    #[inline(always)]
    fn flat(&self) -> impl Predicate {
        // Give the loop its own encoded IDs so advancing the source cannot
        // obscure their independence from the source position.
        self.values
    }

    #[inline(always)]
    fn into_flat(self) -> impl Predicate {
        self.values
    }

    #[inline(always)]
    fn has_group_index(&self) -> bool {
        self.index.enabled()
    }
    #[inline(always)]
    fn next_group(
        &mut self,
        group: &GroupRef<'_>,
        groups: Range<u32>,
        reverse: bool,
    ) -> Option<u32> {
        self.index
            .next_group(group, self.values.ids.iter().copied(), groups, reverse)
    }
    #[inline(always)]
    fn prepare(&mut self, group: &GroupRef<'_>) {
        if !self.values.empty {
            self.index = SymbolIndex::new(group, self.values.ids.iter().copied());
        }
    }
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        self.values.retain_matches(group, candidates)
    }
}

pub struct KindIds<'kinds> {
    strategy: KindStrategy<'kinds>,
    index: SymbolIndex,
}

enum KindStrategy<'kinds> {
    Empty,
    Single(KindPredicate),
    // Retain the public-ID set for sparse candidate masks.
    Small {
        ids: [RemappedKindId; 16],
        length: u8,
        kinds: &'kinds KindSet,
    },
    Multiple(&'kinds KindSet),
}
impl KindStrategy<'_> {
    fn targets<'scan>(
        &'scan self,
        columns: Columns<'scan>,
    ) -> impl Iterator<Item = RemappedKindId> + 'scan {
        let (encoded, public): (&[RemappedKindId], &[KindId]) = match self {
            Self::Empty => (&[], &[]),
            Self::Single(single) => (std::slice::from_ref(&single.target), &[]),
            Self::Small { ids, length, .. } => (&ids[..usize::from(*length)], &[]),
            Self::Multiple(kinds) => (&[], &kinds.ids),
        };
        encoded.iter().copied().chain(
            public
                .iter()
                .copied()
                .filter_map(move |kind| columns.encode_kind(kind)),
        )
    }
}
impl sealed::Predicate for KindIds<'_> {}
impl Predicate for KindIds<'_> {
    #[inline]
    fn fold_nodes<'tree, S: GroupScan<'tree>, B, F>(self, source: S, accumulator: B, fold: F) -> B
    where
        F: FnMut(B, Node<'tree>) -> B,
    {
        if self.has_group_index() {
            return fold_nodes(
                Filtered {
                    source,
                    predicate: self,
                },
                accumulator,
                fold,
            );
        }
        // Keep strategy dispatch outside the group loop for flat consumers.
        match self.strategy {
            KindStrategy::Empty => accumulator,
            KindStrategy::Single(single) => single.fold_nodes(source, accumulator, fold),
            KindStrategy::Small { ids, length: 2, .. } => ArrayKindValues {
                ids: [ids[0], ids[1]],
                empty: false,
            }
            .fold_nodes(source, accumulator, fold),
            KindStrategy::Small {
                ids, length: 3..=4, ..
            } => ArrayKindValues {
                ids: [ids[0], ids[1], ids[2], ids[3]],
                empty: false,
            }
            .fold_nodes(source, accumulator, fold),
            strategy => strategy.fold_nodes(source, accumulator, fold),
        }
    }

    // Select the small count kernel once, outside the group loop.
    #[inline(always)]
    fn count_flat(self, source: Preorder<'_>) -> usize {
        match self.strategy {
            KindStrategy::Empty => 0,
            KindStrategy::Single(single) => source.count_flat(single),
            KindStrategy::Small { ids, length: 2, .. } => source.count_flat(ArrayKindValues {
                ids: [ids[0], ids[1]],
                empty: false,
            }),
            KindStrategy::Small {
                ids, length: 3..=4, ..
            } => source.count_flat(ArrayKindValues {
                ids: [ids[0], ids[1], ids[2], ids[3]],
                empty: false,
            }),
            strategy => source.count_flat(strategy),
        }
    }

    #[inline(always)]
    fn flat(&self) -> impl Predicate {
        &self.strategy
    }

    #[inline(always)]
    fn into_flat(self) -> impl Predicate {
        self.strategy
    }

    #[inline(always)]
    fn has_group_index(&self) -> bool {
        self.index.enabled()
    }
    #[inline(always)]
    fn next_group(
        &mut self,
        group: &GroupRef<'_>,
        groups: Range<u32>,
        reverse: bool,
    ) -> Option<u32> {
        self.index
            .next_group(group, self.strategy.targets(group.columns), groups, reverse)
    }
    #[inline(always)]
    fn prepare(&mut self, group: &GroupRef<'_>) {
        let KindStrategy::Multiple(kinds) = self.strategy else {
            return;
        };
        let columns = group.columns;
        let single = |target| KindStrategy::Single(KindPredicate::new(columns, target));

        self.strategy = match kinds.ids.as_slice() {
            [] => KindStrategy::Empty,
            &[kind] => columns
                .encode_kind(kind)
                .map_or(KindStrategy::Empty, single),
            targets if targets.len() <= 16 => {
                let mut ids = [RemappedKindId(0); 16];
                let mut length = 0;
                for target in targets
                    .iter()
                    .copied()
                    .filter_map(|kind| columns.encode_kind(kind))
                {
                    ids[length] = target;
                    length += 1;
                }
                // Three targets use the four-ID kernel without adding a match.
                if length == 3 {
                    ids[3] = ids[0];
                }
                match length {
                    0 => KindStrategy::Empty,
                    1 => single(ids[0]),
                    _ => KindStrategy::Small {
                        ids,
                        length: length as u8,
                        kinds,
                    },
                }
            }
            _ => KindStrategy::Multiple(kinds),
        };
        self.index = SymbolIndex::new(group, self.strategy.targets(group.columns));
    }

    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        self.strategy.retain_matches(group, candidates)
    }
}

impl sealed::Predicate for KindStrategy<'_> {}
impl Predicate for KindStrategy<'_> {
    // Inlining lets node consumers discard unused group metadata.
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        #[cfg(target_arch = "x86_64")]
        let layout = group.columns.layout();
        match self {
            KindStrategy::Empty => Mask::default(),
            KindStrategy::Single(single) => single.retain_matches(group, candidates),
            #[cfg(target_arch = "x86_64")]
            KindStrategy::Small { ids, length, kinds } => match *length {
                2 => group.equal_id_set(
                    layout.symbol,
                    group.columns.tables().symbol_shift as u32,
                    &ids[..2],
                    candidates,
                ),
                3..=4 => group.equal_id_set(
                    layout.symbol,
                    group.columns.tables().symbol_shift as u32,
                    &ids[..4],
                    candidates,
                ),
                _ => retain_small_kind_set(group, candidates, &ids[..usize::from(*length)], kinds),
            },
            #[cfg(not(target_arch = "x86_64"))]
            KindStrategy::Small { kinds, .. } => retain_kind_set(group, candidates, kinds),
            KindStrategy::Multiple(kinds) => retain_kind_set(group, candidates, kinds),
        }
    }
}
// Keep variable-length SIMD out of the singleton scan's register allocation.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn retain_small_kind_set(
    group: &GroupRef<'_>,
    candidates: Mask,
    ids: &[RemappedKindId],
    kinds: &KindSet,
) -> Mask {
    if ids.len() > 4 && candidates.at_most::<4>() {
        return candidates.retain(|slot| kinds.contains(group.kind(slot)));
    }
    let layout = group.columns.layout();
    group.equal_id_set(
        layout.symbol,
        group.columns.tables().symbol_shift as u32,
        ids,
        candidates,
    )
}
// Isolate the scalar membership loop from SIMD and index traversal state.
#[inline(never)]
fn retain_kind_set(group: &GroupRef<'_>, candidates: Mask, kinds: &KindSet) -> Mask {
    if candidates.at_most::<4>() {
        return candidates.retain(|slot| kinds.contains(group.kind(slot)));
    }
    let layout = group.columns.layout();
    let start = group.first_slot().get() as usize * 2;
    let bytes = group
        .columns
        .slice(layout.symbol, start, group.used() as usize * 2);
    let mut matches = 0;
    for (slot, bytes) in bytes.chunks_exact(2).enumerate() {
        let symbol = u32::from(u16::from_le_bytes([bytes[0], bytes[1]]))
            >> group.columns.tables().symbol_shift as u32;
        let kind = group
            .columns
            .tables()
            .decode_kind(RemappedKindId(symbol as u16));
        matches |= u64::from(kinds.contains(kind)) << slot;
    }
    candidates.intersection(Mask(matches))
}
pub struct ArrayFieldIds<const N: usize>([Option<FieldId>; N]);
impl<const N: usize> sealed::Predicate for ArrayFieldIds<N> {}
impl<const N: usize> Predicate for ArrayFieldIds<N> {
    #[inline(always)]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        group.equal_id_set(group.columns.layout().field, 0, &self.0, candidates)
    }
}
pub struct FieldIds<'ids>(&'ids FieldSet);
impl sealed::Predicate for FieldIds<'_> {}
impl Predicate for FieldIds<'_> {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let column = group.columns.layout().field;
        match self.0.ids.as_slice() {
            [] => Mask::default(),
            &[field] => group.equal_ids(column, 0, field.map_or(0, FieldId::get), candidates),
            fields if fields.len() <= 4 => {
                fields.iter().fold(Mask::default(), |matches, &field| {
                    Mask(
                        matches.0
                            | group
                                .equal_ids(column, 0, field.map_or(0, FieldId::get), candidates)
                                .0,
                    )
                })
            }
            _ => candidates.retain(|slot| {
                self.0.contains(FieldId::new(
                    group.columns.short(column, group.first_slot().get() + slot),
                ))
            }),
        }
    }
}
pub struct FieldPredicate(Option<FieldId>);
impl sealed::Predicate for FieldPredicate {}
impl Predicate for FieldPredicate {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        group.equal_ids(
            group.columns.layout().field,
            0,
            self.0.map_or(0, FieldId::get),
            candidates,
        )
    }
}
pub struct Extra(bool);
impl sealed::Predicate for Extra {}
impl Predicate for Extra {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let flags = if group.columns.tree().flags() & crate::storage::EXTRAS != 0 {
            group.bitmap(group.columns.layout().extra)
        } else {
            0
        };
        Mask(candidates.0 & if self.0 { flags } else { !flags })
    }
}
pub struct Missing(bool);
impl sealed::Predicate for Missing {}
impl Predicate for Missing {
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let flags = if group.columns.tree().flags() & crate::storage::MISSING != 0 {
            group.bitmap(group.columns.layout().missing)
        } else {
            0
        };
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
            let mut absent = 0;
            // Each checked chunk covers both SSE2 loads. Candidate clipping
            // excludes waste and slots outside the subtree after mask extraction.
            let simd = Level::baseline().as_sse2().unwrap();
            for (index, bytes) in masks.chunks_exact(32).enumerate() {
                absent |= (sse2_absent_supertype(simd, bytes, bit) as u64) << (index * 16);
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
    symbol: GrammarKindId,
    index: Option<usize>,
}
impl sealed::Predicate for SupertypeId {}
impl Predicate for SupertypeId {
    #[inline]
    fn prepare(&mut self, group: &GroupRef<'_>) {
        self.index = group
            .columns
            .tables()
            .supertypes()
            .binary_search(&self.symbol.get())
            .ok();
    }
    #[inline]
    fn retain_matches(&self, group: &GroupRef<'_>, candidates: Mask) -> Mask {
        let Some(index) = self.index else {
            return Mask::default();
        };
        let columns = &group.columns;
        if columns.tables().supertypes().len() <= 8 {
            return retain_supertype_masks(
                column_deltas(group, columns.layout().supertype, 2).slice(),
                candidates,
                1 << index,
            );
        }
        let words = columns.tables().supertypes().len().div_ceil(64);
        candidates.retain(|slot| {
            let value = columns.short(columns.layout().supertype, group.first_slot().get() + slot);
            columns.tables().supertype_masks()[usize::from(value) * words + index / 64]
                & (1u64 << (index % 64))
                != 0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_masks_match_indexed_masks_without_preparing_traversal() {
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let source = format!("[{}0]", "0,".repeat(4096));
        let native = parser.parse(&source, None).unwrap();
        let grammar = crate::Grammar::new(&language).unwrap();
        let tree = crate::Tree::pack(&grammar, &native).unwrap();
        let root = tree.root_node();
        let columns = Columns::new(root);
        let group = columns.group(GroupIx(0));

        // Preparation may use the sidecar to skip groups. Both paths must
        // produce the same exact mask within each group.
        let query = [root.kind_id()].into_kind_predicate(&group);
        assert!(!query.has_group_index());
        let mut indexed = [root.kind_id()].into_kind_predicate(&group);
        indexed.prepare(&group);
        assert!(indexed.has_group_index());

        for index in 0..tree.group_count() {
            let group = columns.group(GroupIx(index));
            let expected = group
                .valid_mask()
                .retain(|slot| group.kind(slot) == root.kind_id());
            assert_eq!(query.retain_matches(&group, group.valid_mask()), expected);
            assert_eq!(
                indexed.retain_group(&group, || group.valid_mask()),
                expected
            );
        }
    }

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
                pointer: ColumnPointer(data.as_ptr().cast_mut()),
                length: data.len(),
                borrow: PhantomData,
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
        ]
        .map(PackedPoint);
        let base = PackedPoint(base);
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
