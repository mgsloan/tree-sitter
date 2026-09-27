use crate::{
    ChildIx, Error, FieldId, GrammarKindId, KindId, NamedChildIx, SlotIx, Tree,
    scan::{self, Postorder, Preorder, Scan},
    storage::*,
    traits,
    types::{GroupIx, PackedPoint},
};
use std::{marker::PhantomData, ops::Range, ptr::NonNull};
use tree_sitter::Point;

use fearless_simd::{dispatch, prelude::*, u8x32};

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct RawNode {
    // Every node borrows a live descriptor. Encoding that invariant also lets
    // Option<Node> use null for None without a separate discriminant.
    pub tree: NonNull<TreeData>,
    pub slot: SlotIx,
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Node<'tree> {
    pub(crate) raw: RawNode,
    pub(crate) lifetime: PhantomData<&'tree Tree>,
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
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Node")
            .field("slot", &self.slot().get())
            .field("kind", &self.kind())
            .field("bytes", &self.byte_range())
            .finish()
    }
}

impl Tree {
    pub fn root_node(&self) -> Node<'_> {
        Node {
            raw: RawNode {
                tree: self.0,
                slot: SlotIx::new(self.data().group_end(self.group_count() - 1) - 1),
            },
            lifetime: PhantomData,
        }
    }

    pub fn node_at_slot(&self, slot: SlotIx) -> Option<Node<'_>> {
        (slot.get() < self.slot_count() && slot.get() < self.data().group_end(slot.group().get()))
            .then(|| self.root_node().at(slot))
    }
}

impl<'tree> Node<'tree> {
    #[inline]
    pub(crate) fn data(self) -> &'tree TreeData {
        // Node construction is restricted to live slots in a retained tree.
        unsafe { self.raw.tree.as_ref() }
    }

    #[inline]
    pub(crate) fn at(self, slot: SlotIx) -> Self {
        Self {
            raw: RawNode { slot, ..self.raw },
            lifetime: PhantomData,
        }
    }

    #[inline]
    pub(crate) fn first_slot(self) -> u32 {
        self.data().first_slot(self.slot().get())
    }

    /// Physical slot in reverse preorder; decreasing slots advance preorder.
    #[inline]
    pub fn slot(self) -> SlotIx {
        self.raw.slot
    }

    pub fn byte_range(self) -> Range<usize> {
        self.start_byte()..self.end_byte()
    }

    pub fn utf8_text(self, source: &[u8]) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&source[self.byte_range()])
    }

    /// This node and its descendants, parents before children, left to right.
    pub fn preorder(self) -> Scan<'tree, Preorder<'tree>> {
        Preorder::scan(self)
    }

    /// This node and its descendants, children left to right before their parent.
    pub fn postorder(self) -> Scan<'tree, Postorder<'tree>> {
        Postorder::scan(self)
    }

    /// Choose the cheaper traversal. Order is unspecified across representations.
    pub fn all(self) -> Scan<'tree, Preorder<'tree>> {
        self.preorder()
    }

    /// Scan this subtree in preorder, matching public kind IDs.
    pub fn descendants_matching_kinds<K: scan::IdSelection>(
        self,
        kinds: K,
    ) -> scan::Nodes<'tree, scan::Filtered<Preorder<'tree>, K::KindPredicate>> {
        self.preorder().filter_kind_ids(kinds).nodes()
    }

    pub fn kind_id(self) -> KindId {
        self.data()
            .tables()
            .decode_kind(self.data().symbol_index(self.slot().get()))
    }

    pub fn grammar_id(self) -> GrammarKindId {
        self.data()
            .tables()
            .decode_grammar_kind(self.data().grammar_index(self.slot().get()))
    }

    pub fn kind(self) -> &'tree str {
        self.data().tables().symbol_name(self.kind_id().get())
    }

    pub fn grammar_name(self) -> &'tree str {
        self.data().tables().symbol_name(self.grammar_id().get())
    }

    #[inline]
    pub fn start_byte(self) -> usize {
        let data = self.data();
        (data.word(data.layout.start_byte_base, self.slot().group().get())
            + data.byte(data.layout.start_byte_delta, self.slot().get()) as u32) as usize
    }

    #[inline]
    pub fn end_byte(self) -> usize {
        let data = self.data();
        (data.word(data.layout.end_byte_base, self.slot().group().get())
            - data.short(data.layout.end_byte_delta, self.slot().get()) as u32) as usize
    }

    pub fn start_position(self) -> Point {
        self.packed_start_point().point()
    }

    pub fn has_points(self) -> bool {
        self.data().has_points()
    }

    pub fn end_position(self) -> Point {
        self.packed_end_point().point()
    }

    #[inline]
    pub(crate) fn packed_start_point(self) -> PackedPoint {
        let data = self.data();
        data.point_data.as_ref().map_or_else(
            || PackedPoint(self.start_byte() as u64),
            |points| points.start(self.slot().get()),
        )
    }

    #[inline]
    pub(crate) fn packed_end_point(self) -> PackedPoint {
        let data = self.data();
        data.point_data.as_ref().map_or_else(
            || PackedPoint(self.end_byte() as u64),
            |points| points.end(self.slot().get()),
        )
    }

    #[inline]
    pub fn is_named(self) -> bool {
        self.data().tables().named(self.kind_id().get())
    }

    pub fn is_extra(self) -> bool {
        self.data().flags() & EXTRAS != 0
            && self.data().bit(self.data().layout.extra, self.slot().get())
    }

    pub fn is_missing(self) -> bool {
        self.data().flags() & MISSING != 0
            && self
                .data()
                .bit(self.data().layout.missing, self.slot().get())
    }

    pub fn is_error(self) -> bool {
        self.kind_id() == KindId::ERROR
    }

    pub fn has_error(self) -> bool {
        self.data().flags() & ERRORS != 0
            && self.data().bit(self.data().layout.error, self.slot().get())
    }

    pub fn has_changes(self) -> bool {
        false
    }

    pub fn field_id(self) -> Option<FieldId> {
        FieldId::new(
            self.data()
                .short(self.data().layout.field, self.slot().get()),
        )
    }

    pub fn field_name(self) -> Option<&'tree str> {
        self.data().tables().field_name(self.field_id()?.get())
    }

    pub fn has_supertype(self, symbol: GrammarKindId) -> bool {
        let data = self.data();
        let tables = data.tables();
        if tables.supertype_count == 0 {
            return false;
        }

        let symbols = tables.supertypes();
        let Ok(index) = symbols.binary_search(&symbol.get()) else {
            return false;
        };
        let value = data.short(data.layout.supertype, self.slot().get()) as usize;
        if tables.supertype_count <= 8 {
            value & (1 << index) != 0
        } else {
            let offset = value * tables.dictionary_words as usize + index / 64;
            unsafe { *tables.supertype_masks.add(offset) & (1 << (index % 64)) != 0 }
        }
    }

    pub fn descendant_count(self) -> usize {
        let first = self.first_slot();
        let waste: u32 = (first / GROUP_SIZE..self.slot().group().get())
            .map(|group| self.data().waste(group))
            .sum();
        (self.slot().get() - first + 1 - waste) as usize
    }

    #[inline]
    pub fn next_preorder(self) -> Option<Self> {
        self.data()
            .previous_slot(self.slot().get())
            .map(|slot| self.at(SlotIx::new(slot)))
    }

    pub fn prev_preorder(self) -> Option<Self> {
        let slot = self.previous_preorder_slot();
        (slot < self.data().groups() * GROUP_SIZE).then(|| self.at(SlotIx::new(slot)))
    }

    fn previous_preorder_slot(self) -> u32 {
        let slot = self.slot().get() + 1;
        if slot < self.data().group_end(self.slot().group().get()) {
            slot
        } else {
            (self.slot().group().get() + 1) * GROUP_SIZE
        }
    }

    fn first_child(self) -> Option<Self> {
        self.next_preorder()
            .filter(|node| node.slot().get() >= self.first_slot())
    }

    pub(crate) fn next_sibling_including_empty(self) -> Option<Self> {
        if self.data().bit(self.data().layout.last, self.slot().get()) {
            None
        } else {
            // Subtree boundaries include leading waste, so the predecessor is live.
            Some(self.at(SlotIx::new(self.first_slot() - 1)))
        }
    }

    pub fn parent(self) -> Option<Self> {
        let data = self.data();
        let mut slot = self.slot().get() + 1;
        while slot < data.groups() * GROUP_SIZE {
            let group = slot / GROUP_SIZE;
            let end = data.group_end(group);
            let maximum = data.word(data.layout.span_max, group) as u64;

            // Reject a whole group if even its largest possible span cannot
            // reach this node. The first enclosing span is the nearest parent.
            if slot as u64 <= self.slot().get() as u64 + maximum {
                while slot < end {
                    if slot as u64
                        <= self.slot().get() as u64 + maximum - data.span_delta(slot) as u64
                    {
                        return Some(self.at(SlotIx::new(slot)));
                    }
                    slot += 1;
                }
            }
            slot = (group + 1) * GROUP_SIZE;
        }
        None
    }

    pub fn children(self) -> Children<'tree> {
        Children {
            next: self.first_child(),
        }
    }

    pub fn named_children(self) -> impl Iterator<Item = Self> {
        self.children().filter(|node| node.is_named())
    }

    pub fn children_by_field_id(self, field: FieldId) -> impl Iterator<Item = Self> {
        self.children()
            .filter(move |node| node.field_id() == Some(field))
    }

    pub fn has_children(self) -> bool {
        self.first_child().is_some()
    }

    pub fn has_named_children(self) -> bool {
        self.named_children().next().is_some()
    }

    pub fn child_count(self) -> ChildIx {
        ChildIx::new(self.children().count() as u32)
    }

    pub fn named_child_count(self) -> NamedChildIx {
        NamedChildIx::new(self.named_children().count() as u32)
    }

    pub fn child(self, index: ChildIx) -> Option<Self> {
        self.children().nth(index.get() as usize)
    }

    pub fn named_child(self, index: NamedChildIx) -> Option<Self> {
        self.named_children().nth(index.get() as usize)
    }

    pub fn child_by_field_id(self, field: FieldId) -> Option<Self> {
        // ERROR productions have no field map, even if descendants contributed
        // inherited fields to enumeration.
        if self.is_error() {
            None
        } else {
            self.children_by_field_id(field).next()
        }
    }

    pub fn child_by_field_name(self, field: &str) -> Option<Self> {
        let tables = self.data().tables();
        let field = (1..=tables.field_count as u16)
            .find(|index| tables.field_name(*index) == Some(field))?;
        self.child_by_field_id(FieldId::new(field)?)
    }

    pub fn child_with_descendant(self, descendant: Self) -> Option<Self> {
        if self.raw.tree != descendant.raw.tree
            || descendant.slot() >= self.slot()
            || descendant.slot().get() < self.first_slot()
        {
            return None;
        }
        self.children()
            .find(|child| child.first_slot() <= descendant.slot().get())
    }

    pub fn next_sibling(self) -> Option<Self> {
        let end = self.end_byte();
        let mut next = self.next_sibling_including_empty();
        while next.is_some_and(|node| node.end_byte() <= end) {
            next = next.unwrap().next_sibling_including_empty();
        }
        next
    }

    pub fn next_named_sibling(self) -> Option<Self> {
        let end = self.end_byte();
        let mut next = self.next_sibling_including_empty();
        while next.is_some_and(|node| node.end_byte() <= end || !node.is_named()) {
            next = next.unwrap().next_sibling_including_empty();
        }
        next
    }

    pub fn prev_sibling(self) -> Option<Self> {
        self.parent()?
            .children()
            .take_while(|node| *node != self)
            .last()
    }

    pub fn prev_named_sibling(self) -> Option<Self> {
        self.parent()?
            .children()
            .take_while(|node| *node != self)
            .filter(|node| node.is_named())
            .last()
    }

    pub fn first_child_for_byte(self, byte: usize) -> Option<Self> {
        self.children().find(|node| node.end_byte() > byte)
    }

    pub fn first_named_child_for_byte(self, byte: usize) -> Option<Self> {
        self.named_children().find(|node| node.end_byte() > byte)
    }

    /// Read the constant-time attributes. Counts are separate operations.
    pub fn attributes(self) -> traits::Attributes<'tree> {
        traits::Attributes {
            kind: self.kind(),
            grammar_name: self.grammar_name(),
            kind_id: self.kind_id(),
            grammar_id: self.grammar_id(),
            start_byte: self.start_byte(),
            end_byte: self.end_byte(),
            start_position: self.start_position(),
            end_position: self.end_position(),
            has_points: self.has_points(),
            is_named: self.is_named(),
            is_extra: self.is_extra(),
            is_missing: self.is_missing(),
            is_error: self.is_error(),
            has_error: self.has_error(),
            has_changes: false,
        }
    }

    pub fn walk(self) -> Result<Cursor<'tree>, Error> {
        Ok(Cursor {
            node: self,
            parents: Vec::new(),
        })
    }

    pub fn descendant_for_byte_range(self, start: usize, end: usize) -> Option<Self> {
        u32::try_from(start).ok()?;
        u32::try_from(end).ok()?;
        self.seek::<false>(start as u64, end as u64, false)
    }

    pub fn named_descendant_for_byte_range(self, start: usize, end: usize) -> Option<Self> {
        u32::try_from(start).ok()?;
        u32::try_from(end).ok()?;
        self.seek::<false>(start as u64, end as u64, true)
    }

    pub fn descendant_for_point_range(self, start: Point, end: Point) -> Option<Self> {
        self.seek::<true>(
            PackedPoint::from_point(start)?.get(),
            PackedPoint::from_point(end)?.get(),
            false,
        )
    }

    pub fn named_descendant_for_point_range(self, start: Point, end: Point) -> Option<Self> {
        self.seek::<true>(
            PackedPoint::from_point(start)?.get(),
            PackedPoint::from_point(end)?.get(),
            true,
        )
    }

    #[inline]
    fn start_key<const POINTS: bool>(self) -> u64 {
        if POINTS {
            self.packed_start_point().get()
        } else {
            self.start_byte() as u64
        }
    }

    #[inline]
    fn end_key<const POINTS: bool>(self) -> u64 {
        if POINTS {
            self.packed_end_point().get()
        } else {
            self.end_byte() as u64
        }
    }

    // Shared empty boundaries require sibling order; the indexed search finds
    // the last qualifying start and cannot distinguish that ordering alone.
    #[inline(never)]
    fn seek_descent<const POINTS: bool>(mut self, start: u64, end: u64, named: bool) -> Self {
        let mut result = self;
        loop {
            let found = self.children().find(|child| {
                let child_start = child.start_key::<POINTS>();
                let child_end = child.end_key::<POINTS>();
                child_start <= start
                    && child_end >= end
                    && if child_start == child_end {
                        child_end >= start
                    } else {
                        child_end > start
                    }
            });
            let Some(child) = found else {
                return result;
            };
            self = child;
            if !named || self.is_named() {
                result = self;
            }
        }
    }

    fn seek<const POINTS: bool>(self, start: u64, end: u64, named: bool) -> Option<Self> {
        if start > end {
            return None;
        }
        let data = self.data();
        if POINTS && !data.has_points() {
            return if start >> 32 == 0 && end >> 32 == 0 {
                self.seek::<false>(start, end, named)
            } else {
                Some(self.seek_descent::<true>(start, end, named))
            };
        }
        if start < self.start_key::<POINTS>() || end > self.end_key::<POINTS>() {
            return Some(self);
        }

        // Starts decrease in physical order. The last live slot has the
        // group's earliest start, including for independently stored points.
        let first = self.first_slot();
        let mut low = first / GROUP_SIZE;
        let mut high = self.slot().group().get();
        while low < high {
            let middle = low + (high - low) / 2;
            let after = if POINTS {
                self.at(SlotIx::new(data.group_end(middle) - 1))
                    .start_key::<true>()
                    > start
            } else {
                data.word(data.layout.start_byte_base, middle) as u64 > start
            };
            if after {
                low = middle + 1;
            } else {
                high = middle;
            }
        }

        let mut slot = (low * GROUP_SIZE).max(first);
        let limit = data.group_end(low).min(self.slot().get() + 1);
        if POINTS {
            let group = scan::GroupRef::new(self).at_group(GroupIx(low));
            slot = (low * GROUP_SIZE + group.first_point_start_before(start, slot % GROUP_SIZE))
                .min(limit);
        } else {
            let base = data.word(data.layout.start_byte_base, low) as u64;
            let mask = start_mask(data, low, (start - base).min(255) as u8) >> (slot % GROUP_SIZE);
            slot = if mask == 0 {
                limit
            } else {
                (slot + mask.trailing_zeros()).min(limit)
            };
        }
        if slot == limit {
            slot = (low + 1) * GROUP_SIZE;
            if slot > self.slot().get() {
                return Some(self);
            }
        }

        let mut candidate = self.at(SlotIx::new(slot));
        if start == end {
            let mut previous = candidate;
            while previous.slot() <= self.slot() && previous.start_key::<POINTS>() == start {
                if previous.end_key::<POINTS>() == start {
                    return Some(self.seek_descent::<POINTS>(start, end, named));
                }
                previous = previous.at(SlotIx::new(previous.previous_preorder_slot()));
            }
        }

        // Long point end scans can skip whole intervening subtrees through
        // their parent spans. Byte end scans use the compact delta columns.
        if POINTS && self.slot().get() - candidate.slot().get() > 512 * GROUP_SIZE {
            while candidate.slot() < self.slot() {
                let candidate_end = candidate.end_key::<true>();
                if candidate_end >= end && candidate_end > start && (!named || candidate.is_named())
                {
                    return Some(candidate);
                }
                candidate = candidate.parent().unwrap_or(self);
            }
            return Some(self);
        }

        if candidate.slot() < self.slot() {
            let candidate_end = candidate.end_key::<POINTS>();
            if candidate_end >= end && candidate_end > start && (!named || candidate.is_named()) {
                return Some(candidate);
            }
            candidate.raw.slot = SlotIx::new(candidate.raw.slot.get() + 1);
        }

        // Earlier preorder siblings end before the range. The first qualifying
        // end after the selected start is an enclosing ancestor.
        while candidate.slot() < self.slot() {
            let group = candidate.slot().group().get();
            let limit = data.group_end(group).min(self.slot().get());
            let view = scan::GroupRef::new(self).at_group(GroupIx(group));
            let mut first = candidate.slot().in_group().get();
            while let Some(offset) = view.first_end_after::<POINTS>(start, end, first) {
                let slot = group * GROUP_SIZE + offset;
                if slot >= limit {
                    break;
                }
                let node = self.at(SlotIx::new(slot));
                if !named || node.is_named() {
                    return Some(node);
                }
                first = offset + 1;
            }
            candidate.raw.slot = SlotIx::new((group + 1) * GROUP_SIZE);
        }
        Some(self)
    }
}

fn start_mask(data: &TreeData, group: u32, threshold: u8) -> u64 {
    let deltas = data.column_slice(
        data.layout.start_byte_delta,
        (group * GROUP_SIZE) as usize,
        GROUP_SIZE as usize,
    );
    dispatch!(crate::simd::level(), simd => start_delta_mask(simd, deltas, threshold))
}

#[inline(always)]
fn start_delta_mask<S: Simd>(simd: S, deltas: &[u8], threshold: u8) -> u64 {
    u8x32::from_slice(simd, deltas)
        .simd_le(threshold)
        .to_bitmask()
}

pub struct Children<'tree> {
    next: Option<Node<'tree>>,
}

impl<'tree> Iterator for Children<'tree> {
    type Item = Node<'tree>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.next?;
        self.next = node.next_sibling_including_empty();
        Some(node)
    }
}

impl std::iter::FusedIterator for Children<'_> {}

pub struct Cursor<'tree> {
    node: Node<'tree>,
    parents: Vec<SlotIx>,
}

impl<'tree> Cursor<'tree> {
    pub fn attributes(&mut self) -> traits::Attributes<'tree> {
        self.node.attributes()
    }

    pub fn node(&self) -> Node<'tree> {
        self.node
    }

    pub(crate) fn parent_node(&self) -> Option<Node<'tree>> {
        self.parents.last().map(|slot| self.node.at(*slot))
    }

    /// Start at another node, retaining allocated ancestor storage.
    pub fn reset(&mut self, node: Node<'tree>) {
        self.node = node;
        self.parents.clear();
    }

    pub fn depth(&self) -> u32 {
        self.parents.len() as u32
    }

    pub fn goto_first_child(&mut self) -> bool {
        let Some(child) = self.node.first_child() else {
            return false;
        };
        self.parents.push(self.node.slot());
        self.node = child;
        true
    }

    pub fn goto_last_child(&mut self) -> bool {
        if !self.goto_first_child() {
            return false;
        }
        while self.goto_next_sibling() {}
        true
    }

    pub fn goto_next_sibling(&mut self) -> bool {
        if self.parents.is_empty() {
            return false;
        }
        let Some(next) = self.node.next_sibling_including_empty() else {
            return false;
        };
        self.node = next;
        true
    }

    pub fn goto_parent(&mut self) -> bool {
        let Some(slot) = self.parents.pop() else {
            return false;
        };
        self.node = self.node.at(slot);
        true
    }

    /// Can scan preceding siblings; does not reconstruct the parent.
    pub fn goto_previous_sibling(&mut self) -> bool {
        let previous = self.parent_node().and_then(|parent| {
            parent
                .children()
                .take_while(|node| *node != self.node)
                .last()
        });
        let Some(previous) = previous else {
            return false;
        };
        self.node = previous;
        true
    }

    /// Seek the first child ending after the byte, returning its child index.
    /// Can scan children. Failure leaves the cursor unchanged.
    pub fn goto_first_child_for_byte(&mut self, byte: usize) -> Option<usize> {
        u32::try_from(byte).ok()?;
        self.goto_child_matching(|node| {
            node.end_byte() > byte && node.end_position() > Point::default()
        })
    }

    /// Point counterpart of `goto_first_child_for_byte`.
    pub fn goto_first_child_for_point(&mut self, point: Point) -> Option<usize> {
        u32::try_from(point.row).ok()?;
        u32::try_from(point.column).ok()?;
        self.goto_child_matching(|node| node.end_byte() > 0 && node.end_position() > point)
    }

    fn goto_child_matching(
        &mut self,
        mut matches: impl FnMut(Node<'tree>) -> bool,
    ) -> Option<usize> {
        let (index, child) = self
            .node
            .children()
            .enumerate()
            .find(|(_, node)| matches(*node))?;
        self.parents.push(self.node.slot());
        self.node = child;
        Some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_masks_match_scalar() {
        for level in crate::simd::test_levels() {
            dispatch!(level, simd => check_start_masks(simd));
        }
    }

    #[inline(always)]
    fn check_start_masks<S: Simd>(simd: S) {
        let mut storage = [0u8; 64];
        for offset in 0..32 {
            let bytes = &mut storage[offset..offset + 32];
            for (slot, byte) in bytes.iter_mut().enumerate() {
                *byte = (slot * 8 + offset) as u8;
            }
            for threshold in 0..=u8::MAX {
                let expected = bytes.iter().enumerate().fold(0, |mask, (slot, value)| {
                    mask | (u64::from(*value <= threshold) << slot)
                });
                assert_eq!(start_delta_mask(simd, bytes, threshold), expected);
            }
        }
    }
}
