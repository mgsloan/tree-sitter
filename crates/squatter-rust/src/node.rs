use crate::{
    Error, Tree,
    scan::{self, Postorder, Preorder, Scan},
    storage::*,
    traits,
};
use std::{marker::PhantomData, ops::Range, ptr::NonNull};
use tree_sitter::Point;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct RawNode {
    // Every node borrows a live descriptor. Encoding that invariant also lets
    // Option<Node> use null for None without a separate discriminant.
    pub tree: NonNull<TreeData>,
    pub slot: u32,
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
            .field("slot", &self.slot())
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
                slot: self.data().group_end(self.group_count() - 1) - 1,
            },
            lifetime: PhantomData,
        }
    }

    pub fn node_at_slot(&self, slot: u32) -> Option<Node<'_>> {
        (slot < self.slot_count() && slot < self.data().group_end(slot / GROUP_SIZE))
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
    pub(crate) fn at(self, slot: u32) -> Self {
        Self {
            raw: RawNode { slot, ..self.raw },
            lifetime: PhantomData,
        }
    }

    #[inline]
    pub(crate) fn first_slot(self) -> u32 {
        self.data().first_slot(self.slot())
    }

    /// Physical slot in reverse preorder; decreasing slots advance preorder.
    #[inline]
    pub fn slot(self) -> u32 {
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

    pub fn kind_id(self) -> u16 {
        self.data()
            .tables()
            .decode_id(self.data().symbol_index(self.slot()))
    }

    pub fn grammar_id(self) -> u16 {
        self.data()
            .tables()
            .decode_id(self.data().grammar_index(self.slot()))
    }

    pub fn kind(self) -> &'tree str {
        self.data().tables().symbol_name(self.kind_id())
    }

    pub fn grammar_name(self) -> &'tree str {
        self.data().tables().symbol_name(self.grammar_id())
    }

    #[inline]
    pub fn start_byte(self) -> usize {
        let data = self.data();
        (data.word(data.layout.start_byte_base, self.slot() / GROUP_SIZE)
            + data.byte(data.layout.start_byte_delta, self.slot()) as u32) as usize
    }

    #[inline]
    pub fn end_byte(self) -> usize {
        let data = self.data();
        (data.word(data.layout.end_byte_base, self.slot() / GROUP_SIZE)
            - data.short(data.layout.end_byte_delta, self.slot()) as u32) as usize
    }

    pub fn start_position(self) -> Point {
        let data = self.data();
        if !data.has_points() {
            return Point::new(0, self.start_byte());
        }

        let base = data.long(data.layout.start_point_base, self.slot() / GROUP_SIZE);
        let delta = data.short(data.layout.start_point, self.slot());
        point_from_key(base + expand_point(delta))
    }

    pub fn end_position(self) -> Point {
        let data = self.data();
        if !data.has_points() {
            return Point::new(0, self.end_byte());
        }

        let base = data.long(data.layout.end_point_base, self.slot() / GROUP_SIZE);
        let delta = data.short(data.layout.end_point, self.slot());
        point_from_key(base - expand_point(delta))
    }

    #[inline]
    pub fn is_named(self) -> bool {
        self.data().tables().named(self.kind_id())
    }

    pub fn is_extra(self) -> bool {
        self.data().flags() & EXTRAS != 0 && self.data().bit(self.data().layout.extra, self.slot())
    }

    pub fn is_missing(self) -> bool {
        self.data().flags() & MISSING != 0
            && self.data().bit(self.data().layout.missing, self.slot())
    }

    pub fn is_error(self) -> bool {
        self.kind_id() == u16::MAX
    }

    /// May be true for an error-free node sharing a block with an erroneous node.
    pub fn has_error(self) -> bool {
        self.data().flags() & ERRORS != 0
            && self
                .data()
                .bit(self.data().layout.error, self.slot() / GROUP_SIZE)
    }

    pub fn has_changes(self) -> bool {
        false
    }

    pub fn field_id(self) -> u16 {
        self.data().short(self.data().layout.field, self.slot())
    }

    pub fn field_name(self) -> Option<&'tree str> {
        self.data().tables().field_name(self.field_id())
    }

    pub fn has_supertype(self, symbol: u16) -> bool {
        let data = self.data();
        let tables = data.tables();
        if tables.supertype_count == 0 {
            return false;
        }

        let symbols = unsafe {
            std::slice::from_raw_parts(tables.supertypes, tables.supertype_count as usize)
        };
        let Ok(index) = symbols.binary_search(&symbol) else {
            return false;
        };
        let value = data.short(data.layout.supertype, self.slot()) as usize;
        if tables.supertype_count <= 8 {
            value & (1 << index) != 0
        } else {
            let offset = value * tables.dictionary_words as usize + index / 64;
            unsafe { *tables.supertype_masks.add(offset) & (1 << (index % 64)) != 0 }
        }
    }

    pub fn descendant_count(self) -> usize {
        let first = self.first_slot();
        let waste: u32 = (first / GROUP_SIZE..self.slot() / GROUP_SIZE)
            .map(|group| self.data().waste(group))
            .sum();
        (self.slot() - first + 1 - waste) as usize
    }

    pub fn next_preorder(self) -> Option<Self> {
        self.data()
            .previous_slot(self.slot())
            .map(|slot| self.at(slot))
    }

    pub fn prev_preorder(self) -> Option<Self> {
        let slot = self.previous_preorder_slot();
        (slot < self.data().groups() * GROUP_SIZE).then(|| self.at(slot))
    }

    fn previous_preorder_slot(self) -> u32 {
        let slot = self.slot() + 1;
        if slot < self.data().group_end(self.slot() / GROUP_SIZE) {
            slot
        } else {
            (self.slot() / GROUP_SIZE + 1) * GROUP_SIZE
        }
    }

    fn first_child(self) -> Option<Self> {
        self.next_preorder()
            .filter(|node| node.slot() >= self.first_slot())
    }

    pub(crate) fn next_sibling_including_empty(self) -> Option<Self> {
        if self.data().bit(self.data().layout.last, self.slot()) {
            None
        } else {
            // Subtree boundaries include leading waste, so the predecessor is live.
            Some(self.at(self.first_slot() - 1))
        }
    }

    pub fn parent(self) -> Option<Self> {
        let data = self.data();
        let mut slot = self.slot() + 1;
        while slot < data.groups() * GROUP_SIZE {
            let group = slot / GROUP_SIZE;
            let end = data.group_end(group);
            let base = data.word(data.layout.span_base, group) as u64;

            // Reject a whole group if even its largest possible span cannot
            // reach this node. The first enclosing span is the nearest parent.
            if slot as u64 <= self.slot() as u64 + base + 255 {
                while slot < end {
                    if slot as u64
                        <= self.slot() as u64
                            + base
                            + data.byte(data.layout.span_delta, slot) as u64
                    {
                        return Some(self.at(slot));
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

    pub fn children_by_field_id(self, field: u16) -> impl Iterator<Item = Self> {
        self.children()
            .take_while(move |_| field != 0)
            .filter(move |node| node.field_id() == field)
    }

    pub fn has_children(self) -> bool {
        self.first_child().is_some()
    }

    pub fn has_named_children(self) -> bool {
        self.named_children().next().is_some()
    }

    pub fn child_count(self) -> usize {
        self.children().count()
    }

    pub fn named_child_count(self) -> usize {
        self.named_children().count()
    }

    pub fn child(self, index: usize) -> Option<Self> {
        self.children().nth(index)
    }

    pub fn named_child(self, index: usize) -> Option<Self> {
        self.named_children().nth(index)
    }

    pub fn child_by_field_id(self, field: u16) -> Option<Self> {
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
        self.child_by_field_id(field)
    }

    pub fn child_with_descendant(self, descendant: Self) -> Option<Self> {
        if self.raw.tree != descendant.raw.tree
            || descendant.slot() >= self.slot()
            || descendant.slot() < self.first_slot()
        {
            return None;
        }
        self.children()
            .find(|child| child.first_slot() <= descendant.slot())
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
        self.seek::<true>(point_key(start)?, point_key(end)?, false)
    }

    pub fn named_descendant_for_point_range(self, start: Point, end: Point) -> Option<Self> {
        self.seek::<true>(point_key(start)?, point_key(end)?, true)
    }

    #[inline]
    fn start_key<const POINTS: bool>(self) -> u64 {
        if POINTS {
            point_key(self.start_position()).unwrap()
        } else {
            self.start_byte() as u64
        }
    }

    #[inline]
    fn end_key<const POINTS: bool>(self) -> u64 {
        if POINTS {
            point_key(self.end_position()).unwrap()
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

        // Group start bases decrease in physical order. Point column minima
        // need not coincide, so a tied row uses the earliest live node's point.
        let first = self.first_slot();
        let mut low = first / GROUP_SIZE;
        let mut high = self.slot() / GROUP_SIZE;
        while low < high {
            let middle = low + (high - low) / 2;
            let after = if POINTS {
                let row = data.long(data.layout.start_point_base, middle) >> 32;
                row > start >> 32
                    || (row == start >> 32
                        && self.at(data.group_end(middle) - 1).start_key::<true>() > start)
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
        let limit = data.group_end(low).min(self.slot() + 1);
        if POINTS {
            let base = data.long(data.layout.start_point_base, low);
            let threshold = point_threshold(
                (start >> 32) as i64 - (base >> 32) as i64,
                start as u32 as i64 - base as u32 as i64,
            );
            while slot < limit
                && threshold.is_none_or(|threshold| {
                    data.short(data.layout.start_point, slot) as u32 > threshold
                })
            {
                slot += 1;
            }
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
            if slot > self.slot() {
                return Some(self);
            }
        }

        let mut candidate = self.at(slot);
        if start == end {
            let mut previous = candidate;
            while previous.slot() <= self.slot() && previous.start_key::<POINTS>() == start {
                if previous.end_key::<POINTS>() == start {
                    return Some(self.seek_descent::<POINTS>(start, end, named));
                }
                previous = previous.at(previous.previous_preorder_slot());
            }
        }

        // Point end scans become expensive over large distances. Parent spans
        // can skip the intervening subtrees; byte end scans remain cheap enough.
        if POINTS && self.slot() - candidate.slot() > 512 * GROUP_SIZE {
            while candidate.slot() < self.slot() {
                let candidate_end = candidate.end_key::<POINTS>();
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
            candidate.raw.slot += 1;
        }

        // Earlier preorder siblings end before the range. The first qualifying
        // end after the selected start is an enclosing ancestor.
        while candidate.slot() < self.slot() {
            let group = candidate.slot() / GROUP_SIZE;
            let limit = data.group_end(group).min(self.slot());
            let threshold = if POINTS {
                let base = data.long(data.layout.end_point_base, group);
                point_threshold(
                    (base >> 32) as i64 - (end >> 32) as i64,
                    base as u32 as i64 - end as u32 as i64 - (start == end) as i64,
                )
            } else {
                let base = data.word(data.layout.end_byte_base, group) as u64;
                (base >= end && base > start).then(|| (base - end).min(base - start - 1) as u32)
            };
            if let Some(threshold) = threshold {
                let offset = if POINTS {
                    data.layout.end_point
                } else {
                    data.layout.end_byte_delta
                };
                while candidate.slot() < limit {
                    if data.short(offset, candidate.slot()) as u32 <= threshold
                        && (!named || candidate.is_named())
                    {
                        return Some(candidate);
                    }
                    candidate.raw.slot += 1;
                }
            }
            candidate.raw.slot = (group + 1) * GROUP_SIZE;
        }
        Some(self)
    }
}

// Row occupies the high byte of a point delta. A negative column difference
// excludes the tied row but still permits all columns of smaller row deltas.
fn point_threshold(rows: i64, columns: i64) -> Option<u32> {
    if rows < 0 {
        None
    } else if rows > 255 {
        Some(65535)
    } else if columns < 0 {
        (rows != 0).then(|| ((rows as u32) << 8) - 1)
    } else {
        Some(((rows as u32) << 8) | columns.min(255) as u32)
    }
}

#[inline]
fn point_key(point: Point) -> Option<u64> {
    Some(((u32::try_from(point.row).ok()? as u64) << 32) | u32::try_from(point.column).ok()? as u64)
}

fn start_mask(data: &TreeData, group: u32, threshold: u8) -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        use std::arch::x86_64::*;
        let target = _mm_set1_epi8(threshold as i8);
        let deltas = data
            .bytes
            .as_ptr()
            .add(data.layout.start_byte_delta as usize + (group * GROUP_SIZE) as usize);
        let mut mask = 0;
        for offset in (0..GROUP_SIZE).step_by(16) {
            let lanes = _mm_loadu_si128(deltas.add(offset as usize).cast());
            let matches = _mm_cmpeq_epi8(_mm_min_epu8(lanes, target), lanes);
            mask |= (_mm_movemask_epi8(matches) as u64) << offset;
        }
        mask
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let mut mask = 0;
        for slot in 0..GROUP_SIZE {
            mask |= ((data.byte(data.layout.start_byte_delta, group * GROUP_SIZE + slot)
                <= threshold) as u64)
                << slot;
        }
        mask
    }
}

#[inline]
fn expand_point(delta: u16) -> u64 {
    ((delta as u64 >> 8) << 32) | (delta as u64 & 255)
}

#[inline]
fn point_from_key(key: u64) -> Point {
    Point::new((key >> 32) as usize, key as u32 as usize)
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
    parents: Vec<u32>,
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
