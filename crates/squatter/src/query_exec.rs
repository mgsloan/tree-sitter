use crate::{
    FieldId, ForestRegion, GrammarId, MatchCaptureIx, Node, NodeId, PatternIx, Query, QueryCapture,
    QueryCursorOptions, QueryCursorState, QueryExecutionError, QueryMatch, QueryScope, RawNode,
    SlotIx, StreamingIterator, TextProvider, TreeIx,
    native::{Pattern, PatternEntry, Step, flags::*},
    query::Scope,
    storage::{ColumnPointer, RegionOrder},
    types::{CaptureIx, GroupIx, MatchId, PackedPoint, PatternIndex, SquatterKindId},
};
use std::{cell::Cell, cmp::Ordering, marker::PhantomData, rc::Rc};
use tree_sitter::Point;

#[cfg(target_arch = "x86_64")]
use fearless_simd::{Level, prelude::*, u8x16};

// Mask encoded IDs before packing column equality into slot bits.
#[cfg(target_arch = "x86_64")]
fearless_simd::kernel!(
    #[inline]
    fn sse2_equal_column(simd: Sse2, bytes: &[u8], value: u16, mask: u16) -> u16 {
        use std::arch::x86_64::*;

        let target = _mm_set1_epi16(value as i16);
        let selected = _mm_set1_epi16(mask as i16);
        let low: __m128i = u8x16::from_slice(simd, &bytes[..16]).into();
        let high: __m128i = u8x16::from_slice(simd, &bytes[16..32]).into();
        let low = _mm_and_si128(low, selected);
        let high = _mm_and_si128(high, selected);
        let equal = _mm_packs_epi16(_mm_cmpeq_epi16(low, target), _mm_cmpeq_epi16(high, target));
        _mm_movemask_epi8(equal) as u16
    }
);

const NONE: u32 = u32::MAX;
const DONE: u16 = u16::MAX;
const SEEKING_IMMEDIATE: u16 = 1;
const HAS_ALTERNATIVES: u16 = 2;
const DEAD: u16 = 4;
const NEEDS_PARENT: u16 = 8;
const SKIPPED_QUANTIFIER: u16 = 16;
const REMOVED: u16 = 32;
const EXHAUSTED: u16 = 64;

#[derive(Clone, Copy)]
#[repr(C)]
struct Capture {
    node: RawNode,
    index: CaptureIx,
}

impl PartialEq for Capture {
    fn eq(&self, other: &Self) -> bool {
        self.node.forest == other.node.forest
            && self.node.id == other.node.id
            && self.index == other.index
    }
}

#[derive(Clone, Copy)]
struct State {
    id: MatchId,
    captures: u32,
    order: u32,
    start_depth: u16,
    step: u16,
    pattern: PatternIndex,
    flags: u16,
    consumed: u32,
}

impl State {
    fn has(self, flag: u16) -> bool {
        self.flags & flag != 0
    }

    fn set(&mut self, flag: u16, value: bool) {
        self.flags = (self.flags & !flag) | if value { flag } else { 0 };
    }

    // Finished states no longer need NFA positions. Reusing those four bytes
    // keeps the hot state record the same size in both queues.
    fn set_capture_byte(&mut self, byte: u32) {
        self.start_depth = byte as u16;
        self.step = (byte >> 16) as u16;
    }

    fn capture_byte(self) -> u32 {
        self.start_depth as u32 | ((self.step as u32) << 16)
    }

    fn precedes(self, other: Self) -> bool {
        !self.has(EXHAUSTED)
            && (other.has(EXHAUSTED)
                || (self.capture_byte(), self.pattern, self.order)
                    < (other.capture_byte(), other.pattern, other.order))
    }
}

const _: () = assert!(size_of::<State>() == 24);

#[derive(Clone, Copy)]
struct CaptureList {
    contents: *const Capture,
    length: u32,
    storage: u32,
    first_byte: u32,
    last_end: u32,
    prefix_size: u32,
    prefix: u64,
    hash: u64,
    set: [u64; 2],
}

impl CaptureList {
    const fn empty() -> Self {
        Self {
            contents: std::ptr::null(),
            length: 0,
            storage: NONE,
            first_byte: 0,
            last_end: NONE,
            prefix_size: 0,
            prefix: 0,
            hash: 0,
            set: [0; 2],
        }
    }

    fn hash_capture(&mut self, capture: Capture) {
        let identity = (capture.node.id.slot().raw() as u64)
            .wrapping_mul(0x9e37_79b1_85eb_ca87)
            .wrapping_add(capture.index.raw() as u64);
        self.hash = self
            .hash
            .wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
            .wrapping_add(identity);
        let bit = (identity >> 57) as usize;
        self.set[bit / 64] |= 1 << (bit % 64);
    }
}

#[derive(Default)]
struct CaptureStorage {
    values: Vec<Capture>,
    references: u32,
    next_free: u32,
}

struct CapturePool {
    lists: Vec<CaptureList>,
    storage: Vec<CaptureStorage>,
    free_list: u32,
    free_storage: u32,
    limit: u32,
    next_prefix: u64,
}

impl CapturePool {
    fn new() -> Self {
        Self {
            lists: Vec::new(),
            storage: Vec::new(),
            free_list: NONE,
            free_storage: NONE,
            limit: NONE,
            next_prefix: 0,
        }
    }

    fn reset(&mut self) {
        let count = self.lists.len();
        for (index, list) in self.lists.iter_mut().enumerate() {
            list.length = NONE;
            list.storage = if index + 1 < count {
                index as u32 + 1
            } else {
                NONE
            };
        }
        self.free_list = if count == 0 { NONE } else { 0 };

        let count = self.storage.len();
        for (index, storage) in self.storage.iter_mut().enumerate() {
            storage.references = 0;
            storage.next_free = if index + 1 < count {
                index as u32 + 1
            } else {
                NONE
            };
        }
        self.free_storage = if count == 0 { NONE } else { 0 };
        self.next_prefix = 0;
    }

    fn list(&self, id: u32) -> &CaptureList {
        const EMPTY: CaptureList = CaptureList::empty();
        if id == NONE {
            &EMPTY
        } else {
            &self.lists[id as usize]
        }
    }

    fn get(&self, id: u32) -> &[Capture] {
        self.values(self.list(id))
    }

    fn values(&self, list: &CaptureList) -> &[Capture] {
        if list.length == 0 {
            &[]
        } else {
            // Copy-on-write prevents shared buffers from reallocating. The
            // unique writer refreshes this pointer after reserving capacity.
            debug_assert_eq!(
                list.contents,
                self.storage[list.storage as usize].values.as_ptr()
            );
            debug_assert!(list.length as usize <= self.storage[list.storage as usize].values.len());
            unsafe { std::slice::from_raw_parts(list.contents, list.length as usize) }
        }
    }

    fn is_empty(&self) -> bool {
        self.free_list == NONE && self.lists.len() >= self.limit as usize
    }

    fn acquire(&mut self) -> u32 {
        if self.free_list != NONE {
            let id = self.free_list;
            self.free_list = self.lists[id as usize].storage;
            self.lists[id as usize] = CaptureList::empty();
            id
        } else if self.lists.len() < self.limit as usize {
            let id = self.lists.len() as u32;
            self.lists.push(CaptureList::empty());
            id
        } else {
            NONE
        }
    }

    fn clear(&mut self, id: u32) {
        let list = &mut self.lists[id as usize];
        if list.storage != NONE {
            let storage = &mut self.storage[list.storage as usize];
            storage.references -= 1;
            if storage.references == 0 {
                // Returned captures remain readable until the next advancement
                // reuses their buffer. Releasing a list must not clear its Vec.
                storage.next_free = self.free_storage;
                self.free_storage = list.storage;
            }
        }
        *list = CaptureList::empty();
    }

    fn release(&mut self, id: u32) {
        if id == NONE {
            return;
        }
        debug_assert_ne!(self.lists[id as usize].length, NONE);
        self.clear(id);
        self.lists[id as usize].length = NONE;
        self.lists[id as usize].storage = self.free_list;
        self.free_list = id;
    }

    fn make_mutable(&mut self, id: u32, additional: usize) {
        let list = self.lists[id as usize];
        if list.storage == NONE || self.storage[list.storage as usize].references > 1 {
            let target = if self.free_storage == NONE {
                let target = self.storage.len() as u32;
                self.storage.push(CaptureStorage::default());
                target
            } else {
                let target = self.free_storage;
                self.free_storage = self.storage[target as usize].next_free;
                target
            };
            let source = if list.length == 0 {
                std::ptr::null()
            } else {
                self.storage[list.storage as usize].values.as_ptr()
            };
            let storage = &mut self.storage[target as usize];
            storage.values.clear();
            storage.values.reserve(list.length as usize + additional);
            if list.length != 0 {
                // A free target cannot be the referenced source. Reallocating
                // its Vec leaves the source buffer and initialized prefix intact.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        source,
                        storage.values.as_mut_ptr(),
                        list.length as usize,
                    );
                    storage.values.set_len(list.length as usize);
                }
            }
            storage.references = 1;
            if list.storage != NONE {
                self.storage[list.storage as usize].references -= 1;
            }
            self.lists[id as usize].storage = target;
        } else {
            self.storage[list.storage as usize]
                .values
                .reserve(additional);
        }
        let list = &mut self.lists[id as usize];
        list.contents = self.storage[list.storage as usize].values.as_ptr();
    }

    fn share(&mut self, target: u32, source: u32) {
        let mut list = self.lists[source as usize];
        if list.length == 0 {
            return;
        }

        // Fingerprints are only useful after branching. Unbranched histories
        // avoid maintaining them; later appends preserve the shared prefix ID.
        if list.prefix == 0 {
            list.hash = 0;
            list.set = [0; 2];
            for capture in self.get(source) {
                list.hash_capture(*capture);
            }
        }
        if list.prefix == 0 || list.prefix_size != list.length {
            self.next_prefix = self.next_prefix.wrapping_add(1);
            if self.next_prefix == 0 {
                for list in &mut self.lists {
                    list.prefix = 0;
                }
                self.next_prefix = 1;
            }
            list.prefix = self.next_prefix;
            list.prefix_size = list.length;
        }
        self.lists[source as usize] = list;
        self.lists[target as usize] = list;
        self.storage[list.storage as usize].references += 1;
    }

    fn append(&mut self, id: u32, node: Node<'_>, step: &Step) -> bool {
        self.make_mutable(id, 3);
        let list = &mut self.lists[id as usize];
        let first = list.length == 0;
        if first {
            list.first_byte = node.start_byte() as u32;
        }
        list.last_end = NONE;

        let storage = &mut self.storage[list.storage as usize];
        for capture_id in step.capture_ids {
            if capture_id == DONE {
                break;
            }
            let capture = Capture {
                node: node.raw,
                index: CaptureIx(capture_id as u32),
            };
            storage.values.push(capture);
            list.length += 1;
            if list.prefix != 0 {
                list.hash_capture(capture);
            }
        }
        first
    }

    fn containment(
        &self,
        left_list: &CaptureList,
        right_list: &CaptureList,
        root: Node<'_>,
    ) -> (bool, bool) {
        let mut contains_right = left_list.length >= right_list.length;
        let mut contains_left = right_list.length >= left_list.length;
        if left_list.storage == right_list.storage {
            return (contains_right, contains_left);
        }

        // Hashes and sets only reject containment. Collisions still undergo
        // exact comparison of node identity and capture order.
        if left_list.prefix != 0 && right_list.prefix != 0 {
            if left_list.length == right_list.length {
                if left_list.hash != right_list.hash {
                    return (false, false);
                }
            } else {
                for word in 0..2 {
                    contains_right &=
                        left_list.set[word] & right_list.set[word] == right_list.set[word];
                    contains_left &=
                        left_list.set[word] & right_list.set[word] == left_list.set[word];
                }
                if !contains_right && !contains_left {
                    return (false, false);
                }
            }
        }

        let left = self.values(left_list);
        let right = self.values(right_list);
        let shared = if left_list.prefix != 0 && left_list.prefix == right_list.prefix {
            left_list.prefix_size.min(right_list.prefix_size) as usize
        } else {
            0
        };
        let (mut left_index, mut right_index) = (shared, shared);
        while left_index < left.len() && right_index < right.len() {
            let first = left[left_index];
            let second = right[right_index];
            if first == second {
                left_index += 1;
                right_index += 1;
                continue;
            }
            if left.len() == right.len() {
                return (false, false);
            }

            let first = root.at(first.node.id.slot());
            let second = root.at(second.node.id.slot());
            let order = first
                .start_byte()
                .cmp(&second.start_byte())
                .then_with(|| second.end_byte().cmp(&first.end_byte()));
            match order {
                Ordering::Less => {
                    contains_left = false;
                    left_index += 1;
                }
                Ordering::Greater => {
                    contains_right = false;
                    right_index += 1;
                }
                Ordering::Equal => {
                    return (false, false);
                }
            }
            if !contains_right && !contains_left {
                return (false, false);
            }
        }
        (
            contains_right && right_index == right.len(),
            contains_left && left_index == left.len(),
        )
    }
}

#[derive(Clone, Copy)]
struct QueryRange {
    start_byte: u32,
    end_byte: u32,
    start_point: PackedPoint,
    end_point: PackedPoint,
}

impl Default for QueryRange {
    fn default() -> Self {
        Self {
            start_byte: 0,
            end_byte: NONE,
            start_point: PackedPoint(0),
            end_point: PackedPoint(u64::MAX),
        }
    }
}

impl QueryRange {
    fn set_byte_range(&mut self, range: std::ops::Range<usize>) {
        let start = range.start as u32;
        let end = match range.end as u32 {
            0 => NONE,
            end => end,
        };
        if start <= end {
            self.start_byte = start;
            self.end_byte = end;
        }
    }

    fn set_point_range(&mut self, range: std::ops::Range<Point>) {
        let start = PackedPoint::from_point_cast(range.start);
        let mut end = PackedPoint::from_point_cast(range.end);
        if end == PackedPoint(0) {
            end = PackedPoint(u64::MAX);
        }
        if start <= end {
            self.start_point = start;
            self.end_point = end;
        }
    }

    fn unrestricted(self) -> bool {
        self.start_byte == 0
            && self.end_byte == NONE
            && self.start_point == PackedPoint(0)
            && self.end_point == PackedPoint(u64::MAX)
    }

    fn intersects(self, node: Node<'_>) -> bool {
        let empty = node.start_byte() == node.end_byte();
        (node.end_byte() > self.start_byte as usize
            || (empty && node.end_byte() == self.start_byte as usize))
            && node.start_byte() < self.end_byte as usize
            && (node.packed_end_point() > self.start_point
                || (empty && node.packed_end_point() == self.start_point))
            && node.packed_start_point() < self.end_point
    }

    fn contains(self, node: Node<'_>) -> bool {
        node.start_byte() >= self.start_byte as usize
            && node.end_byte() <= self.end_byte as usize
            && node.packed_start_point() >= self.start_point
            && node.packed_end_point() <= self.end_point
    }

    fn precedes(self, node: Node<'_>) -> bool {
        node.end_byte() <= self.start_byte as usize || node.packed_end_point() <= self.start_point
    }

    fn follows(self, node: Node<'_>) -> bool {
        node.start_byte() >= self.end_byte as usize || node.packed_start_point() >= self.end_point
    }
}

struct RegionTrees<'forest> {
    region: ForestRegion<'forest>,
    next: u32,
    end: u32,
}

impl<'forest> RegionTrees<'forest> {
    fn new(region: ForestRegion<'forest>, range: QueryRange) -> Self {
        let trees = region.data().trees.clone();
        let mut next = trees.start.raw();
        let end = trees.end.raw();
        if region.data().order == RegionOrder::NonOverlapping {
            let mut upper = end;
            while next < upper {
                let middle = next + (upper - next) / 2;
                let root = region.forest.tree(TreeIx::from_raw(middle)).root_node();
                let root_end = root.end_byte();
                let before = root_end < range.start_byte as usize
                    || (root_end == range.start_byte as usize && root.start_byte() != root_end);
                if before {
                    next = middle + 1;
                } else {
                    upper = middle;
                }
            }
        }
        Self { region, next, end }
    }

    fn next_root(&mut self, range: QueryRange) -> Option<Node<'forest>> {
        while self.next < self.end {
            let root = self
                .region
                .forest
                .tree(TreeIx::from_raw(self.next))
                .root_node();
            self.next += 1;
            if self.region.data().order != RegionOrder::Unordered
                && root.start_byte() >= range.end_byte as usize
            {
                self.next = self.end;
                return None;
            }
            if range.intersects(root) {
                return Some(root);
            }
        }
        None
    }
}

#[derive(Clone, Copy, Default)]
struct PresenceCache {
    start: u32,
    next: u32,
    found: bool,
    samples: u8,
    rejections: u8,
    cooldown: u8,
}

#[derive(Clone, Copy)]
struct DirectPosition {
    root: u32,
    next: u32,
    end: u32,
}

#[derive(Clone, Copy)]
struct FirstCapture {
    state: usize,
    byte: u32,
    pattern: PatternIndex,
    definite: bool,
}

struct Output {
    id: MatchId,
    pattern: PatternIndex,
    captures: *const Capture,
    count: usize,
    index: usize,
}

#[derive(Clone, Copy, Default)]
struct ComparisonEntry {
    next: usize,
    end: usize,
    count: u32,
    first_byte: u32,
}

#[derive(Clone)]
struct ComparisonBlock {
    bits: [u64; 128],
    valid: u64,
    common: [u64; 2],
    combined: [u64; 2],
    cached_set: [u64; 2],
    cached_candidates: u64,
}

impl ComparisonBlock {
    fn new() -> Self {
        Self {
            bits: [0; 128],
            valid: 0,
            common: [u64::MAX; 2],
            combined: [0; 2],
            cached_set: [0; 2],
            cached_candidates: 0,
        }
    }

    fn candidates(&mut self, set: [u64; 2]) -> u64 {
        if self.cached_set == set {
            return self.cached_candidates;
        }
        self.cached_set = set;
        let mut subsets = self.valid;
        let mut supersets = self.valid;
        for word in 0..2 {
            if self.common[word] & !set[word] != 0 {
                subsets = 0;
            }
            if set[word] & !self.combined[word] != 0 {
                supersets = 0;
            }
            let varying = self.combined[word] ^ self.common[word];
            let mut required = varying & set[word];
            let mut forbidden = varying & !set[word];
            while required != 0 && supersets != 0 {
                supersets &= self.bits[word * 64 + required.trailing_zeros() as usize];
                required &= required - 1;
            }
            while forbidden != 0 && subsets != 0 {
                subsets &= !self.bits[word * 64 + forbidden.trailing_zeros() as usize];
                forbidden &= forbidden - 1;
            }
        }
        self.cached_candidates = subsets | supersets | !self.valid;
        self.cached_candidates
    }
}

/// Reusable query settings and execution storage. Each execution exclusively
/// borrows the cursor; dropping it releases the query, tree, provider, and callback.
/// Finite match limits bound in-progress storage. Discovery and eviction order
/// can retain a different valid subset of results than Tree-sitter.
pub struct QueryCursor {
    // Shared indirection keeps iterator-held references valid across range setters.
    removal: Rc<Cell<Option<MatchId>>>,
    optimized: bool,
    range: QueryRange,
    containing_range: QueryRange,
    max_start_depth: u32,
    pool: CapturePool,
    states: Vec<State>,
    pending: Vec<State>,
    comparison_index: Vec<ComparisonEntry>,
    comparison_heads: Vec<usize>,
    comparison_blocks: Vec<ComparisonBlock>,
    finished: Vec<State>,
    finished_heap_size: usize,
    parents: Vec<NodeId>,
    position: NodeId,
    ascending: bool,
    halted: bool,
    error: Option<QueryExecutionError>,
    exceeded_limit: bool,
    operations: u32,
    dirty_patterns: u64,
    states_need_sort: bool,
    states_max_depth: u32,
    next_state_id: MatchId,
    next_finished_id: u32,
    first_capture: Option<FirstCapture>,
    first_capture_valid: bool,
    presence: Vec<PresenceCache>,
    direct: bool,
    direct_position: u32,
    direct_states: Vec<DirectPosition>,
    direct_free: u32,
    scan_samples: u32,
    scan_sparse_samples: u32,
    scan_cooldown: u32,
}

// Capture pointers are inert outside execution, which retains the tree borrow.
// Starting another execution resets the logical lists before reading captures.
// The removal Rc is never cloned; only a live execution can borrow its cell.
unsafe impl Send for QueryCursor {}

impl Default for QueryCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryCursor {
    pub fn new() -> Self {
        Self {
            removal: Rc::new(Cell::new(None)),
            optimized: true,
            range: QueryRange::default(),
            containing_range: QueryRange::default(),
            max_start_depth: NONE,
            pool: CapturePool::new(),
            states: Vec::with_capacity(8),
            pending: Vec::new(),
            comparison_index: Vec::new(),
            comparison_heads: Vec::new(),
            comparison_blocks: Vec::new(),
            finished: Vec::with_capacity(8),
            finished_heap_size: 0,
            parents: Vec::new(),
            position: NodeId::new(TreeIx::from_raw(0), SlotIx::from_raw(0)),
            ascending: false,
            halted: false,
            error: None,
            exceeded_limit: false,
            operations: 0,
            dirty_patterns: 0,
            states_need_sort: false,
            states_max_depth: 0,
            next_state_id: MatchId::from_raw(0),
            next_finished_id: 0,
            first_capture: None,
            first_capture_valid: false,
            presence: Vec::new(),
            direct: false,
            direct_position: 0,
            direct_states: Vec::new(),
            direct_free: NONE,
            scan_samples: 0,
            scan_sparse_samples: 0,
            scan_cooldown: 0,
        }
    }

    /// Enable execution plans and root seeking when available.
    pub fn set_optimized(&mut self, enabled: bool) {
        self.optimized = enabled;
    }

    /// Set the maximum in-progress capture-list capacity.
    pub fn set_match_limit(&mut self, limit: u32) {
        self.pool.limit = limit;
    }

    /// Whether the most recent execution exceeded its in-progress capacity.
    pub fn did_exceed_match_limit(&self) -> bool {
        self.exceeded_limit
    }

    /// Limit the depth at which patterns can start. `None` removes the limit.
    pub fn set_max_start_depth(&mut self, depth: Option<u32>) -> &mut Self {
        self.max_start_depth = depth.unwrap_or(NONE);
        self
    }

    /// Maximum in-progress capture-list capacity, not a result count.
    pub fn match_limit(&self) -> u32 {
        self.pool.limit
    }

    /// Restrict matches to nodes intersecting this byte range. Zero end is
    /// unbounded. Coordinates narrow to u32; reversed ranges leave it unchanged.
    pub fn set_byte_range(&mut self, range: std::ops::Range<usize>) -> &mut Self {
        self.range.set_byte_range(range);
        self
    }

    /// Restrict matches to nodes intersecting this point range, using the same
    /// narrowing and validation rules as `set_byte_range`.
    /// Without point data, nodes use row zero and byte offsets as columns.
    pub fn set_point_range(&mut self, range: std::ops::Range<Point>) -> &mut Self {
        self.range.set_point_range(range);
        self
    }

    /// Require all matched nodes to be fully contained in this byte range.
    /// Can be combined with the intersecting range set by `set_byte_range`.
    /// Zero end is unbounded. Coordinates narrow to u32; reversed ranges leave
    /// the previous containing range unchanged.
    pub fn set_containing_byte_range(&mut self, range: std::ops::Range<usize>) -> &mut Self {
        self.containing_range.set_byte_range(range);
        self
    }

    /// Require all matched nodes to be fully contained in this point range.
    /// Can be combined with `set_point_range`, using the same narrowing and
    /// validation rules. Without point data, nodes use row zero and byte
    /// offsets as columns. A zero end point is unbounded.
    pub fn set_containing_point_range(&mut self, range: std::ops::Range<Point>) -> &mut Self {
        self.containing_range.set_point_range(range);
        self
    }

    fn start_tree(&mut self, query: &Query, root: Node<'_>) {
        self.removal.set(None);
        self.pool.reset();
        self.states.clear();
        self.pending.clear();
        self.finished.clear();
        self.parents.clear();
        self.direct_states.clear();
        self.presence.clear();
        if self.optimized {
            self.presence
                .resize(query.program.presence.len(), PresenceCache::default());
        }

        self.halted = self.error.is_some();
        self.position = root.id();
        self.ascending = false;
        self.dirty_patterns = 0;
        self.states_need_sort = false;
        self.states_max_depth = 0;
        self.finished_heap_size = 0;
        self.first_capture_valid = false;
        self.scan_samples = 0;
        self.scan_sparse_samples = 0;
        self.scan_cooldown = 0;
        self.direct = self.optimized
            && query.program.direct.is_some()
            && self.max_start_depth == NONE
            && !self.halted;
        self.direct_position =
            root.data().groups() * crate::storage::GROUP_SIZE - 1 - root.slot().raw();
        self.direct_free = NONE;
    }

    /// Start a fresh execution, retaining the provider and borrowing this cursor
    /// until dropped. Unavailable optimizations fall back to general execution.
    pub fn execute<'cursor, 'query, 'tree, Provider, Chunk>(
        &'cursor mut self,
        query: &'query Query,
        root: impl Into<QueryScope<'tree>>,
        text_provider: Provider,
    ) -> QueryExecution<'cursor, 'query, 'tree, 'static, Provider, Chunk>
    where
        Provider: TextProvider<Chunk>,
        Chunk: AsRef<[u8]>,
    {
        self.execute_with_options(query, root, text_provider, QueryCursorOptions::new())
    }

    /// Start a fresh execution with a resumable progress callback.
    pub fn execute_with_options<'cursor, 'query, 'tree, 'options, Provider, Chunk>(
        &'cursor mut self,
        query: &'query Query,
        root: impl Into<QueryScope<'tree>>,
        text_provider: Provider,
        options: QueryCursorOptions<'options>,
    ) -> QueryExecution<'cursor, 'query, 'tree, 'options, Provider, Chunk>
    where
        Provider: TextProvider<Chunk>,
        Chunk: AsRef<[u8]>,
    {
        let (root, mut remaining) = match root.into().0 {
            Scope::Node(root) => (root, None),
            Scope::Region(region) => (
                region.trees().next().unwrap().root_node(),
                Some(RegionTrees::new(region, self.range)),
            ),
        };
        self.error = (root.tables().language != query.compiled.view.language)
            .then_some(QueryExecutionError::InvalidExecution);
        self.exceeded_limit = false;
        self.operations = 0;
        self.next_state_id = MatchId::from_raw(0);
        self.next_finished_id = 0;
        let selected = if self.error.is_none() {
            remaining.as_mut().map(|trees| trees.next_root(self.range))
        } else {
            None
        };
        let root = selected.flatten().unwrap_or(root);
        self.start_tree(query, root);
        if selected == Some(None) {
            self.halted = true;
        }

        // Root searches reuse word-wide comparisons across scanned groups.
        let byte_ids = root.data().layout.symbol_width == 1;
        let scan_filter = std::array::from_fn(|index| {
            query
                .program
                .scan_filter
                .matches
                .get(index)
                .map_or((0, 0), |&(value, mask)| {
                    if byte_ids {
                        (
                            u64::from(value as u8) * 0x0101_0101_0101_0101,
                            u64::from(mask as u8) * 0x0101_0101_0101_0101,
                        )
                    } else {
                        (
                            u64::from(value) * 0x0001_0001_0001_0001,
                            u64::from(mask) * 0x0001_0001_0001_0001,
                        )
                    }
                })
        });
        let unrestricted = self.range.unrestricted();
        let containing_unrestricted = self.containing_range.unrestricted();
        QueryExecution {
            cursor: self,
            query,
            steps: query.compiled.steps(),
            entries: query.compiled.entries(),
            patterns: query.compiled.patterns(),
            scan_filter,
            unrestricted,
            containing_unrestricted,
            root_has_error: root.has_error(),
            total_slots: root.data().groups() * crate::storage::GROUP_SIZE,
            root,
            remaining,
            text_provider,
            options,
            text_buffers: Default::default(),
            chunk: PhantomData,
            stopped: false,
            scan_resume: None,
        }
    }
}

/// An execution owns its text provider and callback borrow. Results borrow the
/// current advancement; copied nodes retain only the tree lifetime. A `None`
/// caused by a progress callback is resumable, and does not signal exhaustion.
///
/// A live result prevents cursor reuse:
/// ```compile_fail
/// # use tree_squatter::{Query, QueryCursor, Node};
/// # fn example(cursor: &mut QueryCursor, query: &Query, root: Node<'_>, text: &[u8]) {
/// let mut execution = cursor.execute(query, root, text);
/// let found = execution.next_match().unwrap();
/// cursor.execute(query, root, text);
/// println!("{:?}", found.captures());
/// # }
/// ```
/// Provider and callback borrows last until the execution is dropped:
/// ```compile_fail
/// # use tree_squatter::{Query, QueryCursor, Node};
/// # fn example(cursor: &mut QueryCursor, query: &Query, root: Node<'_>) {
/// let text = vec![b' '; root.end_byte()];
/// let mut execution = cursor.execute(query, root, text.as_slice());
/// drop(text);
/// execution.next_match();
/// # }
/// ```
/// ```compile_fail
/// # use tree_squatter::{Query, QueryCursor, QueryCursorOptions, QueryCursorState, Node};
/// # fn example(cursor: &mut QueryCursor, query: &Query, root: Node<'_>, text: &[u8]) {
/// let mut callback = |_: &QueryCursorState| std::ops::ControlFlow::Continue(());
/// let mut options = QueryCursorOptions::new().progress_callback(&mut callback);
/// let mut execution = cursor.execute_with_options(query, root, text, options.reborrow());
/// options.reborrow();
/// execution.next_match();
/// # }
/// ```
pub struct QueryExecution<'cursor, 'query, 'tree, 'options, Provider, Chunk>
where
    Provider: TextProvider<Chunk>,
    Chunk: AsRef<[u8]>,
{
    cursor: &'cursor mut QueryCursor,
    query: &'query Query,
    // Borrow native records once; hot transitions need no repeated view conversion.
    steps: &'query [Step],
    entries: &'query [PatternEntry],
    patterns: &'query [Pattern],
    scan_filter: [(u64, u64); 8],
    unrestricted: bool,
    containing_unrestricted: bool,
    root_has_error: bool,
    total_slots: u32,
    root: Node<'tree>,
    remaining: Option<RegionTrees<'tree>>,
    text_provider: Provider,
    options: QueryCursorOptions<'options>,
    text_buffers: [Vec<u8>; 2],
    chunk: PhantomData<Chunk>,
    stopped: bool,
    scan_resume: Option<u32>,
}

impl<Provider: TextProvider<Chunk>, Chunk: AsRef<[u8]>> Drop
    for QueryExecution<'_, '_, '_, '_, Provider, Chunk>
{
    fn drop(&mut self) {
        // Retain allocations, but leave no live logical state referring to an
        // input after its guard ends. Raw capture buffers are reset before reuse.
        self.cursor.states.clear();
        self.cursor.finished.clear();
        self.cursor.parents.clear();
        self.cursor.direct_states.clear();
    }
}

impl<'query, 'tree, Provider: TextProvider<Chunk>, Chunk: AsRef<[u8]>>
    QueryExecution<'_, 'query, 'tree, '_, Provider, Chunk>
{
    /// Report a query/node language mismatch. Cancellation is not an error.
    pub fn error(&self) -> Option<QueryExecutionError> {
        self.cursor.error
    }

    /// Advance to the next completed match. After callback cancellation returns
    /// `None`, calling again resumes the same execution.
    pub fn next_match(&mut self) -> Option<QueryMatch<'_, 'tree>> {
        self.next(false).map(|(result, _)| result)
    }

    /// Return a provisional match snapshot and the next capture's index.
    /// Snapshots may gain captures or lose longest-match filtering. Use
    /// `next_match` for completed matches; capture event order is unspecified.
    pub fn next_capture(&mut self) -> Option<(QueryMatch<'_, 'tree>, MatchCaptureIx)> {
        self.next(true)
    }

    fn next(&mut self, capture: bool) -> Option<(QueryMatch<'_, 'tree>, MatchCaptureIx)> {
        if self.cursor.error.is_some() {
            return None;
        }

        if self.stopped {
            self.stopped = false;
            return None;
        }
        if let Some(id) = self.cursor.removal.take() {
            self.remove_match(id);
        }
        loop {
            let output = if capture {
                self.next_capture_output()
            } else {
                self.next_match_output()
            };
            let Some(output) = output else {
                if !self.stopped && self.cursor.halted && self.advance_tree() {
                    continue;
                }
                self.stopped = false;
                return None;
            };
            // Both records have the same C layout. Nodes inherit the execution's
            // retained tree lifetime; the slice expires before any pool reuse.
            let captures = if output.count == 0 {
                &[]
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        output.captures.cast::<QueryCapture<'tree>>(),
                        output.count,
                    )
                }
            };
            let result = QueryMatch {
                id: output.id,
                pattern_index: PatternIx(output.pattern.raw() as usize),
                captures,
                removal: &self.cursor.removal,
            };
            if result.satisfies(self.query, &mut self.text_provider, &mut self.text_buffers) {
                return Some((
                    QueryMatch {
                        id: result.id(),
                        pattern_index: result.pattern_index,
                        captures,
                        removal: &self.cursor.removal,
                    },
                    MatchCaptureIx::from_raw(output.index as u32),
                ));
            }
            if capture {
                self.remove_match(output.id);
            }
        }
    }

    fn advance_tree(&mut self) -> bool {
        let Some(root) = self
            .remaining
            .as_mut()
            .and_then(|trees| trees.next_root(self.cursor.range))
        else {
            return false;
        };
        self.cursor.start_tree(self.query, root);
        self.root = root;
        self.root_has_error = root.has_error();
        self.scan_resume = None;
        true
    }

    fn current(&self) -> Node<'tree> {
        Node::new(self.root.data(), self.cursor.position)
    }

    fn parent(&self) -> Option<Node<'tree>> {
        self.cursor
            .parents
            .last()
            .map(|&id| Node::new(self.root.data(), id))
    }

    fn poll(&mut self) -> bool {
        self.poll_at(self.cursor.position.slot())
    }

    fn poll_at(&mut self, slot: SlotIx) -> bool {
        if self.stopped {
            return true;
        }
        self.cursor.operations += 1;
        if self.cursor.operations < 100 {
            return false;
        }
        self.cursor.operations = 0;
        if let Some(callback) = &mut self.options.progress_callback {
            let state = QueryCursorState {
                current_byte_offset: self.root.at(slot).start_byte(),
            };
            self.stopped = callback(&state).is_break();
        }
        self.stopped
    }

    fn step(&self, index: u16) -> &'query Step {
        debug_assert!((index as usize) < self.steps.len());
        // Only the trusted compiler and its control-flow edges produce indexes.
        unsafe { self.steps.get_unchecked(index as usize) }
    }
}

/// Streaming matches from a query execution. Advancing ends the current result
/// borrow. Callback cancellation returns `None`; advancing again resumes.
pub struct QueryMatches<'cursor, 'query, 'tree, 'options, Provider, Chunk>
where
    Provider: TextProvider<Chunk>,
    Chunk: AsRef<[u8]>,
{
    execution: QueryExecution<'cursor, 'query, 'tree, 'options, Provider, Chunk>,
    current: Option<QueryMatch<'cursor, 'tree>>,
}
impl<Provider: TextProvider<Chunk>, Chunk: AsRef<[u8]>>
    QueryMatches<'_, '_, '_, '_, Provider, Chunk>
{
    /// Update the cursor's persistent byte range for subsequent advancement.
    pub fn set_byte_range(&mut self, range: std::ops::Range<usize>) {
        self.execution.cursor.set_byte_range(range);
        self.execution.unrestricted = self.execution.cursor.range.unrestricted();
        self.execution.cursor.first_capture_valid = false;
    }
    /// Update the cursor's persistent point range for subsequent advancement.
    pub fn set_point_range(&mut self, range: std::ops::Range<Point>) {
        self.execution.cursor.set_point_range(range);
        self.execution.unrestricted = self.execution.cursor.range.unrestricted();
        self.execution.cursor.first_capture_valid = false;
    }
}
impl<'cursor, 'tree, Provider: TextProvider<Chunk>, Chunk: AsRef<[u8]>> StreamingIterator
    for QueryMatches<'cursor, '_, 'tree, '_, Provider, Chunk>
{
    type Item = QueryMatch<'cursor, 'tree>;
    fn advance(&mut self) {
        self.current = None;
        // The captures and removal cell live in the exclusively borrowed cursor.
        // Only get() exposes them, with its shorter &self borrow, and the old
        // item is cleared before advancing can reuse capture storage.
        self.current = self
            .execution
            .next_match()
            .map(|item| unsafe { std::mem::transmute::<_, Self::Item>(item) });
    }
    fn get(&self) -> Option<&Self::Item> {
        self.current.as_ref()
    }
}
impl QueryCursor {
    /// Start a fresh stream of matches, retaining the provider until dropped.
    pub fn matches<'cursor, 'query, 'tree, Provider, Chunk>(
        &'cursor mut self,
        query: &'query Query,
        root: impl Into<QueryScope<'tree>>,
        text_provider: Provider,
    ) -> QueryMatches<'cursor, 'query, 'tree, 'static, Provider, Chunk>
    where
        Provider: TextProvider<Chunk>,
        Chunk: AsRef<[u8]>,
    {
        self.matches_with_options(query, root, text_provider, QueryCursorOptions::new())
    }
    /// Start a fresh stream with a resumable progress callback.
    pub fn matches_with_options<'cursor, 'query, 'tree, 'options, Provider, Chunk>(
        &'cursor mut self,
        query: &'query Query,
        root: impl Into<QueryScope<'tree>>,
        text_provider: Provider,
        options: QueryCursorOptions<'options>,
    ) -> QueryMatches<'cursor, 'query, 'tree, 'options, Provider, Chunk>
    where
        Provider: TextProvider<Chunk>,
        Chunk: AsRef<[u8]>,
    {
        QueryMatches {
            execution: self.execute_with_options(query, root, text_provider, options),
            current: None,
        }
    }
}

/// Streaming captures from a query execution. Advancing ends the current result
/// borrow. Callback cancellation returns `None`; advancing again resumes.
/// Capture events are provisional snapshots: order and multiplicity can differ
/// from Tree-sitter, and snapshots can grow or lose longest-match filtering.
/// Completed-match captures remain covered. Use `matches` for completed results.
///
/// ```compile_fail
/// # use tree_squatter::{Query, QueryCursor, Node, StreamingIterator};
/// # fn example(cursor: &mut QueryCursor, query: &Query, root: Node<'_>, text: &[u8]) {
/// let mut captures = cursor.captures(query, root, text);
/// let (found, _) = captures.next().unwrap();
/// let borrowed = found.captures();
/// captures.next();
/// println!("{borrowed:?}");
/// # }
/// ```
pub struct QueryCaptures<'cursor, 'query, 'tree, 'options, Provider, Chunk>
where
    Provider: TextProvider<Chunk>,
    Chunk: AsRef<[u8]>,
{
    execution: QueryExecution<'cursor, 'query, 'tree, 'options, Provider, Chunk>,
    current: Option<(QueryMatch<'cursor, 'tree>, MatchCaptureIx)>,
}
impl<Provider: TextProvider<Chunk>, Chunk: AsRef<[u8]>>
    QueryCaptures<'_, '_, '_, '_, Provider, Chunk>
{
    /// Update the cursor's persistent byte range for subsequent advancement.
    pub fn set_byte_range(&mut self, range: std::ops::Range<usize>) {
        self.execution.cursor.set_byte_range(range);
        self.execution.unrestricted = self.execution.cursor.range.unrestricted();
        self.execution.cursor.first_capture_valid = false;
    }
    /// Update the cursor's persistent point range for subsequent advancement.
    pub fn set_point_range(&mut self, range: std::ops::Range<Point>) {
        self.execution.cursor.set_point_range(range);
        self.execution.unrestricted = self.execution.cursor.range.unrestricted();
        self.execution.cursor.first_capture_valid = false;
    }
}
impl<'cursor, 'tree, Provider: TextProvider<Chunk>, Chunk: AsRef<[u8]>> StreamingIterator
    for QueryCaptures<'cursor, '_, 'tree, '_, Provider, Chunk>
{
    type Item = (QueryMatch<'cursor, 'tree>, MatchCaptureIx);
    fn advance(&mut self) {
        self.current = None;
        // The captures and removal cell live in the exclusively borrowed cursor.
        // Only get() exposes them, with its shorter &self borrow, and the old
        // item is cleared before advancing can reuse capture storage.
        self.current = self
            .execution
            .next_capture()
            .map(|item| unsafe { std::mem::transmute::<_, Self::Item>(item) });
    }
    fn get(&self) -> Option<&Self::Item> {
        self.current.as_ref()
    }
}
impl QueryCursor {
    /// Start a fresh stream of captures, retaining the provider until dropped.
    pub fn captures<'cursor, 'query, 'tree, Provider, Chunk>(
        &'cursor mut self,
        query: &'query Query,
        root: impl Into<QueryScope<'tree>>,
        text_provider: Provider,
    ) -> QueryCaptures<'cursor, 'query, 'tree, 'static, Provider, Chunk>
    where
        Provider: TextProvider<Chunk>,
        Chunk: AsRef<[u8]>,
    {
        self.captures_with_options(query, root, text_provider, QueryCursorOptions::new())
    }
    /// Start a fresh stream with a resumable progress callback.
    pub fn captures_with_options<'cursor, 'query, 'tree, 'options, Provider, Chunk>(
        &'cursor mut self,
        query: &'query Query,
        root: impl Into<QueryScope<'tree>>,
        text_provider: Provider,
        options: QueryCursorOptions<'options>,
    ) -> QueryCaptures<'cursor, 'query, 'tree, 'options, Provider, Chunk>
    where
        Provider: TextProvider<Chunk>,
        Chunk: AsRef<[u8]>,
    {
        QueryCaptures {
            execution: self.execute_with_options(query, root, text_provider, options),
            current: None,
        }
    }
}

const _: () = {
    assert!(size_of::<Capture>() == size_of::<QueryCapture<'static>>());
    assert!(align_of::<Capture>() == align_of::<QueryCapture<'static>>());
    assert!(
        std::mem::offset_of!(Capture, index) == std::mem::offset_of!(QueryCapture<'static>, index)
    );
};

impl QueryCursor {
    fn sift_up(&mut self, mut index: usize) {
        while index != 0 {
            let parent = (index - 1) / 2;
            if !self.finished[index].precedes(self.finished[parent]) {
                break;
            }
            self.finished.swap(index, parent);
            index = parent;
        }
    }

    fn sift_down(&mut self, mut index: usize) {
        loop {
            let left = index * 2 + 1;
            if left >= self.finished.len() {
                break;
            }
            let right = left + 1;
            let smallest = if right < self.finished.len()
                && self.finished[right].precedes(self.finished[left])
            {
                right
            } else {
                left
            };
            if !self.finished[smallest].precedes(self.finished[index]) {
                break;
            }
            self.finished.swap(smallest, index);
            index = smallest;
        }
    }

    fn erase_finished(&mut self, index: usize) {
        if self.finished_heap_size == 0 {
            self.finished.remove(index);
        } else {
            self.finished.swap_remove(index);
            if index < self.finished.len() {
                if index != 0 && self.finished[index].precedes(self.finished[(index - 1) / 2]) {
                    self.sift_up(index);
                } else {
                    self.sift_down(index);
                }
            }
            self.finished_heap_size = self.finished.len();
        }
    }
}

impl<'query, 'tree, Provider: TextProvider<Chunk>, Chunk: AsRef<[u8]>>
    QueryExecution<'_, 'query, 'tree, '_, Provider, Chunk>
{
    fn update_key(&self, state: &mut State) {
        let captures = self.cursor.pool.get(state.captures);
        state.set(EXHAUSTED, state.consumed as usize >= captures.len());
        if !state.has(EXHAUSTED) {
            let byte = if state.consumed == 0 {
                self.cursor.pool.list(state.captures).first_byte
            } else {
                self.root
                    .at(captures[state.consumed as usize].node.id.slot())
                    .start_byte() as u32
            };
            state.set_capture_byte(byte);
        }
    }

    fn finish(&mut self, mut state: State) {
        state.order = self.cursor.next_finished_id;
        self.cursor.next_finished_id = self.cursor.next_finished_id.wrapping_add(1);
        self.cursor.finished.push(state);
    }

    fn heapify(&mut self) {
        // Completed-match iteration needs discovery order only. Decode capture
        // positions lazily when capture streaming first needs the heap.
        while self.cursor.finished_heap_size < self.cursor.finished.len() {
            let index = self.cursor.finished_heap_size;
            let mut state = self.cursor.finished[index];
            self.update_key(&mut state);
            self.cursor.finished[index] = state;
            self.cursor.sift_up(index);
            self.cursor.finished_heap_size += 1;
        }
    }

    fn snapshot(&mut self, state: &mut State) -> Output {
        if state.id == MatchId::from_raw(NONE) {
            state.id = self.cursor.next_state_id;
            self.cursor.next_state_id =
                MatchId::from_raw(self.cursor.next_state_id.raw().wrapping_add(1));
        }
        let captures = self.cursor.pool.get(state.captures);
        Output {
            id: state.id,
            pattern: state.pattern,
            captures: captures.as_ptr(),
            count: captures.len(),
            index: state.consumed as usize,
        }
    }

    fn next_match_output(&mut self) -> Option<Output> {
        if self.cursor.finished.is_empty() && !self.advance(false) {
            return None;
        }
        let index = if self.cursor.finished_heap_size == 0 {
            0
        } else {
            self.heapify();
            self.cursor
                .finished
                .iter()
                .enumerate()
                .min_by_key(|(_, state)| state.order)
                .unwrap()
                .0
        };
        let mut state = self.cursor.finished[index];
        let output = self.snapshot(&mut state);
        self.cursor.pool.release(state.captures);
        self.cursor.erase_finished(index);
        Some(output)
    }

    fn first_in_progress(&mut self, eviction: bool) -> Option<FirstCapture> {
        let mut result: Option<FirstCapture> = None;
        for index in 0..self.cursor.states.len() {
            self.poll();
            let mut state = self.cursor.states[index];
            if state.has(DEAD) {
                continue;
            }
            let captures = self.cursor.pool.get(state.captures);
            while (state.consumed as usize) < captures.len()
                && self.cursor.range.precedes(
                    self.root
                        .at(captures[state.consumed as usize].node.id.slot()),
                )
            {
                state.consumed += 1;
            }
            self.cursor.states[index].consumed = state.consumed;
            let Some(capture) = captures.get(state.consumed as usize) else {
                continue;
            };
            let byte = self.root.at(capture.node.id.slot()).start_byte() as u32;
            if result.is_none_or(|first| (byte, state.pattern) < (first.byte, first.pattern)) {
                let step = self.step(state.step);
                if eviction && step.has(ROOT_PATTERN_GUARANTEED) {
                    continue;
                }
                result = Some(FirstCapture {
                    state: index,
                    byte,
                    pattern: state.pattern,
                    definite: step.has(ROOT_PATTERN_GUARANTEED) && !step.has(IS_IMMEDIATE),
                });
            }
        }
        result
    }

    fn next_capture_output(&mut self) -> Option<Output> {
        loop {
            self.heapify();
            if !self.cursor.first_capture_valid {
                self.cursor.first_capture = self.first_in_progress(false);
                self.cursor.first_capture_valid = true;
            }
            let unfinished = self.cursor.first_capture;
            let mut finished = false;

            while let Some(mut state) = self.cursor.finished.first().copied() {
                let captures = self.cursor.pool.get(state.captures);
                let Some(capture) = captures.get(state.consumed as usize) else {
                    self.cursor.pool.release(state.captures);
                    self.cursor.erase_finished(0);
                    continue;
                };
                let node = self.root.at(capture.node.id.slot());
                if self.cursor.range.precedes(node) || self.cursor.range.follows(node) {
                    state.consumed += 1;
                    self.update_key(&mut state);
                    self.cursor.finished[0] = state;
                    self.cursor.sift_down(0);
                    continue;
                }
                finished = unfinished.is_none_or(|first| {
                    (node.start_byte() as u32, state.pattern) < (first.byte, first.pattern)
                });
                break;
            }

            if finished {
                let mut state = self.cursor.finished[0];
                let output = self.snapshot(&mut state);
                state.consumed += 1;
                self.update_key(&mut state);
                self.cursor.finished[0] = state;
                self.cursor.sift_down(0);
                return Some(output);
            } else if let Some(first) = unfinished.filter(|first| first.definite) {
                let mut state = self.cursor.states[first.state];
                let output = self.snapshot(&mut state);
                state.consumed += 1;
                self.cursor.states[first.state] = state;
                self.cursor.first_capture_valid = false;
                return Some(output);
            }

            if self.cursor.pool.is_empty() {
                if let Some(first) = unfinished {
                    let state = self.cursor.states.remove(first.state);
                    self.cursor.pool.release(state.captures);
                    if self.cursor.direct {
                        self.release_direct(state.order);
                    }
                    self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
                }
            }
            if !self.advance(true) && (self.stopped || self.cursor.finished.is_empty()) {
                return None;
            }
        }
    }

    /// Suppress every remaining result with this execution-local match ID.
    pub fn remove_match(&mut self, id: MatchId) {
        if self.cursor.finished_heap_size != 0 {
            self.heapify();
        }
        while let Some(index) = self.cursor.finished.iter().position(|state| state.id == id) {
            self.cursor
                .pool
                .release(self.cursor.finished[index].captures);
            self.cursor.erase_finished(index);
        }
        while let Some(index) = self.cursor.states.iter().position(|state| state.id == id) {
            let state = self.cursor.states.remove(index);
            self.cursor.pool.release(state.captures);
            if self.cursor.direct {
                self.release_direct(state.order);
            }
            self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
            self.cursor.first_capture_valid = false;
        }
    }

    fn prepare_capture(&mut self, state: &mut State, preserve: usize) -> bool {
        if state.captures != NONE {
            return true;
        }
        state.captures = self.cursor.pool.acquire();
        if state.captures != NONE {
            return true;
        }

        self.cursor.exceeded_limit = true;
        let Some(first) = self
            .first_in_progress(true)
            .filter(|first| first.state != preserve)
        else {
            return false;
        };
        let other = &mut self.cursor.states[first.state];
        state.captures = other.captures;
        other.captures = NONE;
        other.flags |= DEAD;
        self.cursor.dirty_patterns |= 1 << (other.pattern.raw() % 64);
        self.cursor.states_need_sort = true;
        self.cursor.pool.clear(state.captures);
        true
    }

    fn capture(&mut self, state: &mut State, node: Node<'tree>, step: &Step) {
        if state.has(DEAD) {
            return;
        }
        if !self.prepare_capture(state, usize::MAX) {
            state.flags |= DEAD;
            return;
        }
        self.cursor.states_need_sort |= self.cursor.pool.append(state.captures, node, step);
    }

    fn copy_state(&mut self, index: usize) -> bool {
        let original = self.cursor.states[index];
        let mut copy = original;
        copy.captures = NONE;
        if original.captures != NONE {
            if !self.prepare_capture(&mut copy, index) {
                return false;
            }
            self.cursor.pool.share(copy.captures, original.captures);
        }
        self.cursor.states.insert(index + 1, copy);
        true
    }

    fn add_state(&mut self, entry: PatternEntry) -> usize {
        let step = self.step(entry.step_index);
        let depth = (self.cursor.parents.len() as u32).wrapping_sub(step.depth as u32);
        let mut index = self.cursor.states.len();
        while index > 0 {
            let previous = self.cursor.states[index - 1];
            if (previous.start_depth as u32) < depth {
                break;
            }
            if previous.start_depth as u32 == depth {
                if previous.pattern == entry.pattern_index && previous.step == entry.step_index {
                    return index - 1;
                }
                if previous.pattern <= entry.pattern_index {
                    break;
                }
            }
            index -= 1;
        }

        self.cursor.dirty_patterns |= 1 << (entry.pattern_index.raw() % 64);
        self.cursor.states_need_sort = true;
        self.cursor.states.insert(
            index,
            State {
                id: MatchId::from_raw(NONE),
                captures: NONE,
                order: NONE,
                start_depth: depth as u16,
                step: entry.step_index,
                pattern: entry.pattern_index,
                consumed: 0,
                flags: SEEKING_IMMEDIATE | if step.depth == 1 { NEEDS_PARENT } else { 0 },
            },
        );
        index
    }

    fn stage_remaining(&mut self, index: usize) {
        if self.cursor.states.len() - index > 32 && self.cursor.pending.is_empty() {
            self.cursor
                .pending
                .extend_from_slice(&self.cursor.states[index + 1..]);
            self.cursor.states.truncate(index + 1);
        }
    }

    fn goto_first_child(&mut self) -> bool {
        let Some(child) = self.current().child(crate::ChildIx::new(0)) else {
            return false;
        };
        self.cursor.parents.push(self.cursor.position);
        self.cursor.position = child.id();
        true
    }

    fn goto_next_sibling(&mut self) -> bool {
        if self.cursor.parents.is_empty() {
            return false;
        }
        let Some(next) = self.current().next_sibling_including_empty() else {
            return false;
        };
        self.cursor.position = next.id();
        true
    }

    fn goto_parent(&mut self) -> bool {
        let Some(parent) = self.cursor.parents.pop() else {
            return false;
        };
        self.cursor.position = parent;
        true
    }

    fn total_slots(&self) -> u32 {
        self.total_slots
    }

    fn normalize_position(&self, position: u32) -> u32 {
        let total = self.total_slots();
        let limit = self.node_end(self.root);
        if position >= limit {
            return limit;
        }
        let slot = total - 1 - position;
        let end = self
            .root
            .data()
            .group_end(slot / crate::storage::GROUP_SIZE);
        if slot >= end {
            (total - end).min(limit)
        } else {
            position
        }
    }

    fn position_node(&self, position: u32) -> Node<'tree> {
        self.root
            .at(SlotIx::from_raw(self.total_slots() - 1 - position))
    }

    fn node_end(&self, node: Node<'tree>) -> u32 {
        self.total_slots() - node.first_slot()
    }

    fn find_symbols(&mut self, start: u32, end: u32) -> u32 {
        let targets = &self.query.program.scan_targets;
        // Fixed cardinalities keep comparison counts visible to the scan
        // compiler. Larger unions retain the packed-word control kernel.
        match targets.as_slice() {
            &[first] => return self.find_symbols_simd(start, end, [SquatterKindId(first)]),
            &[first, second] => {
                return self.find_symbols_simd(
                    start,
                    end,
                    [SquatterKindId(first), SquatterKindId(second)],
                );
            }
            &[first, second, third] => {
                return self.find_symbols_simd(
                    start,
                    end,
                    [
                        SquatterKindId(first),
                        SquatterKindId(second),
                        SquatterKindId(third),
                    ],
                );
            }
            &[first, second, third, fourth] => {
                return self.find_symbols_simd(
                    start,
                    end,
                    [
                        SquatterKindId(first),
                        SquatterKindId(second),
                        SquatterKindId(third),
                        SquatterKindId(fourth),
                    ],
                );
            }
            _ => {}
        }
        self.find_symbols_control(start, end)
    }

    fn find_symbols_simd<const N: usize>(
        &mut self,
        mut start: u32,
        end: u32,
        targets: [SquatterKindId; N],
    ) -> u32 {
        use crate::scan::GroupRef;
        use crate::storage::GROUP_SIZE;

        let total = self.total_slots();
        let groups = GroupRef::new(self.root);

        while start < end {
            if self.poll_at(SlotIx::from_raw(total - 1 - start)) {
                return start;
            }
            let index = (total - 1 - start) / GROUP_SIZE;
            let group_end = ((start / GROUP_SIZE + 1) * GROUP_SIZE).min(end);
            if self.root.presence().is_none_or(|cache| {
                targets
                    .iter()
                    .any(|symbol| cache.has(index, symbol.raw() as usize))
            }) {
                let group = groups.at_group(GroupIx(index));
                let base = group.first_slot().raw();
                let mut hits = group.equal_kind_ids(&targets, group.valid_mask()).bits();
                let first = total - group_end - base;
                let last = total - start - base;
                hits &= u64::MAX << first;
                if last < 64 {
                    hits &= (1 << last) - 1;
                }
                if hits != 0 {
                    return total - 1 - (base + 63 - hits.leading_zeros());
                }
            }
            start = self.normalize_position(group_end);
        }
        end
    }

    fn find_symbols_control(&mut self, mut start: u32, end: u32) -> u32 {
        use crate::storage::GROUP_SIZE;
        let query = self.query;
        let data = self.root.data();
        let filter = &query.program.scan_filter.matches;
        let targets = &query.program.scan_targets;
        let total = self.total_slots();
        let width = data.layout.symbol_width;
        let lanes = 8 / width;
        let low_bits = if width == 1 {
            0x7f7f_7f7f_7f7f_7f7f
        } else {
            0x7fff_7fff_7fff_7fff
        };

        while start < end {
            if self.poll_at(SlotIx::from_raw(total - 1 - start)) {
                return start;
            }
            let group = (total - 1 - start) / GROUP_SIZE;
            let group_end = ((start / GROUP_SIZE + 1) * GROUP_SIZE).min(end);
            if !targets.is_empty()
                && targets.len() <= 4
                && self.root.presence().is_some_and(|cache| {
                    !targets
                        .iter()
                        .any(|symbol| cache.has(group, *symbol as usize))
                })
            {
                start = self.normalize_position(group_end);
                continue;
            }

            if filter.is_empty() {
                while start < group_end {
                    let symbol = data.symbol_index(total - 1 - start).raw();
                    if query.program.scan_symbols[symbol as usize / 64] & (1 << (symbol % 64)) != 0
                    {
                        return start;
                    }
                    start = self.normalize_position(start + 1);
                }
            } else {
                // Lane high bits identify exact matches without carries leaking
                // between adjacent byte or u16 IDs.
                let low = total - group_end;
                let high = total - start;
                for word_index in (low / lanes..=(high - 1) / lanes).rev() {
                    let word = data.long(data.layout.symbol, word_index);
                    let mut hits = 0;
                    for &(value, mask) in &self.scan_filter[..filter.len()] {
                        let difference = (word ^ value) & mask;
                        hits |= !(((difference & low_bits).wrapping_add(low_bits)) | difference)
                            & !low_bits;
                    }
                    while hits != 0 {
                        let bit = 63 - hits.leading_zeros();
                        let physical = word_index * lanes + bit / (8 * width);
                        if physical >= low && physical < high {
                            return total - 1 - physical;
                        }
                        hits &= !(1 << bit);
                    }
                }
                start = self.normalize_position(group_end);
            }
        }
        end
    }

    fn scan_seek(&mut self) -> bool {
        let current_position = self.total_slots() - 1 - self.cursor.position.slot().raw();
        let end = self.node_end(self.root);
        let mut start = self.scan_resume.take().unwrap_or(current_position);
        let target = 'search: loop {
            let target = self.find_symbols(start, end);
            if self.stopped {
                self.scan_resume = Some(target);
                return false;
            }
            if self.cursor.halted {
                return false;
            }
            if target == end {
                self.cursor.halted = true;
                return false;
            }

            // With no partial states, skipped enter/exit events cannot affect a
            // match. Restore only the ancestor path needed by the next root.
            while self.total_slots() - 1 - self.cursor.position.slot().raw() != target {
                let node = self.current();
                if target < self.node_end(node) {
                    // A symbol hit must not re-enter a subtree that ordinary
                    // traversal would skip because of the query range.
                    if (!self.unrestricted
                        && (!self.cursor.range.intersects(node)
                            || self
                                .parent()
                                .is_some_and(|parent| !self.cursor.range.intersects(parent))))
                        || (!self.containing_unrestricted
                            && !self.cursor.containing_range.intersects(node))
                    {
                        start = self.node_end(node);
                        continue 'search;
                    }
                    if self.goto_first_child() {
                        continue;
                    }
                }
                while !self.goto_next_sibling() {
                    assert!(self.goto_parent());
                }
            }
            break target;
        };

        self.cursor.scan_sparse_samples += (target - current_position >= 2) as u32;
        self.cursor.scan_samples += 1;
        if self.cursor.scan_samples == 32 {
            if self.cursor.scan_sparse_samples == 0 {
                self.cursor.scan_cooldown = 256;
            }
            self.cursor.scan_samples = 0;
            self.cursor.scan_sparse_samples = 0;
        }

        true
    }

    fn presence_matches(&mut self, requirement: u16, root: Node<'tree>) -> bool {
        if !self.cursor.optimized {
            return true;
        }
        let index = requirement as usize - 1;
        let mut cache = self.cursor.presence[index];
        let requirement = self.query.program.presence[index];
        if cache.samples == 32 {
            if cache.rejections < 8 {
                cache.cooldown = 128;
            }
            cache.samples = 0;
            cache.rejections = 0;
        }
        if cache.cooldown != 0 {
            cache.cooldown -= 1;
            self.cursor.presence[index] = cache;
            return true;
        }
        cache.samples += 1;

        let data = root.data();
        let group_size = crate::storage::GROUP_SIZE;
        if requirement.symbol != 0 {
            if let Some(presence) = root.presence() {
                let slot = root.slot().raw();
                let group = slot / group_size;
                let maximum = data.word(data.layout.span_max, group);
                // The maximum covers this subtree without loading its span delta.
                let first = slot
                    .saturating_sub(maximum)
                    .max(root.tree_data().slots.start.raw());
                let groups = first / group_size..group + 1;
                let found = presence
                    .find_matching_group(groups, requirement.symbol as usize, false)
                    .is_some();
                if !found {
                    cache.rejections += 1;
                    self.cursor.presence[index] = cache;
                    return false;
                }
            }
        }

        let mut begin = self.normalize_position(self.total_slots() - root.slot().raw());
        let limit = self.node_end(root);
        if begin >= cache.start && begin <= cache.next {
            if cache.next >= limit {
                cache.rejections += 1;
                self.cursor.presence[index] = cache;
                return false;
            }
            if cache.found {
                self.cursor.presence[index] = cache;
                return true;
            }
            begin = cache.next;
        } else {
            cache.start = begin;
        }

        // Only a completed scan proves absence. Hitting the budget preserves
        // the ordinary matcher, and the known-empty prefix can be reused later.
        let scanned_end = begin + (limit - begin).min(256);
        let mut position = begin;
        while position < scanned_end {
            let group = position / group_size;
            let group_start = group * group_size;
            let end = (group_start + group_size).min(scanned_end);
            let physical_group = data.groups() - 1 - group;
            let mut hits = {
                let mut hits = u64::MAX;
                if requirement.symbol != 0 {
                    hits = equal_column(
                        data,
                        data.layout.symbol,
                        physical_group,
                        requirement.symbol,
                        u16::MAX,
                        data.layout.symbol_width,
                    );
                }
                if requirement.field != 0 {
                    hits &= equal_column(
                        data,
                        data.layout.field,
                        physical_group,
                        requirement.field,
                        u16::MAX,
                        2,
                    );
                }
                hits
            };
            // Physical slots run opposite to preorder, so the highest hit comes first.
            hits &= u64::MAX << (group_size - (end - group_start));
            hits &= u64::MAX >> (64 - group_size + (position - group_start));
            if hits != 0 {
                cache.next = group_start + (hits.leading_zeros() - (64 - group_size));
                cache.found = true;
                self.cursor.presence[index] = cache;
                return true;
            }
            position = end;
        }
        cache.next = scanned_end;
        cache.found = false;
        let found = scanned_end < limit;
        if !found {
            cache.rejections += 1;
        }
        self.cursor.presence[index] = cache;
        found
    }

    fn acquire_direct(&mut self, position: DirectPosition) -> u32 {
        if self.cursor.direct_free == NONE {
            let index = self.cursor.direct_states.len() as u32;
            self.cursor.direct_states.push(position);
            index
        } else {
            let index = self.cursor.direct_free;
            self.cursor.direct_free = self.cursor.direct_states[index as usize].root;
            self.cursor.direct_states[index as usize] = position;
            index
        }
    }

    fn release_direct(&mut self, index: u32) {
        self.cursor.direct_states[index as usize].root = self.cursor.direct_free;
        self.cursor.direct_free = index;
    }

    fn named_child_position(&self, start: u32, end: u32) -> u32 {
        let mut position = self.normalize_position(start);
        while position < end {
            let node = self.position_node(position);
            if node.is_named() {
                return position;
            }
            position = self.normalize_position(self.node_end(node));
        }
        end
    }

    fn direct_roots(&self, node: Node<'tree>) -> u64 {
        let plan = self.query.program.direct.as_ref().unwrap();
        let roots = plan.roots[node.data().symbol_index(node.slot().raw()).raw() as usize];
        if roots == 0 || (self.unrestricted && self.containing_unrestricted) {
            return roots;
        }
        if (!self.unrestricted && !self.cursor.range.intersects(node))
            || (!self.containing_unrestricted && !self.cursor.containing_range.contains(node))
        {
            return 0;
        }

        // An empty node can overlap the range start while its enclosing
        // nonempty subtree ends there and would never be entered.
        let mut ancestor = node;
        while ancestor.start_byte() == ancestor.end_byte() && ancestor != self.root {
            ancestor = ancestor.parent().unwrap();
            if (!self.unrestricted && !self.cursor.range.intersects(ancestor))
                || (!self.containing_unrestricted
                    && !self.cursor.containing_range.intersects(ancestor))
            {
                return 0;
            }
        }
        roots
    }

    fn advance_direct(&mut self, stop_on_definite: bool) -> bool {
        if self.cursor.halted {
            return false;
        }
        let query = self.query;
        let plan = query.program.direct.as_ref().unwrap();
        let root_end = self.node_end(self.root);

        loop {
            let next = self
                .cursor
                .states
                .iter()
                .map(|state| self.cursor.direct_states[state.order as usize].next)
                .min()
                .unwrap_or(root_end);
            let mut position = self.normalize_position(self.cursor.direct_position);
            while position < next {
                if self.poll_at(SlotIx::from_raw(self.total_slots - 1 - position)) {
                    self.cursor.direct_position = position;
                    return false;
                }
                if !query.program.scan_filter.matches.is_empty() {
                    position = self.find_symbols(position, next);
                    if self.stopped {
                        self.cursor.direct_position = position;
                        return false;
                    }
                    if position == next {
                        break;
                    }
                }
                let node = self.position_node(position);
                if self.direct_roots(node) != 0 {
                    break;
                }
                position = self.normalize_position(position + 1);
            }
            if self.cursor.halted {
                return false;
            }

            let mut index = 0;
            while index < self.cursor.states.len() {
                let state = self.cursor.states[index];
                if self.cursor.direct_states[state.order as usize].end <= position {
                    self.cursor.pool.release(state.captures);
                    self.release_direct(state.order);
                    self.cursor.states.remove(index);
                } else {
                    index += 1;
                }
            }
            if position == root_end {
                self.cursor.halted = true;
                return false;
            }
            if self.poll_at(SlotIx::from_raw(self.total_slots - 1 - position)) {
                self.cursor.direct_position = position;
                return false;
            }

            self.cursor.direct_position = self.normalize_position(position + 1);
            let node = self.position_node(position);
            // Shared capture bookkeeping polls the node currently being processed.
            self.cursor.position = node.id();
            let symbol = node.data().symbol_index(node.slot().raw()).raw();
            let mut roots = self.direct_roots(node);
            while roots != 0 {
                let pattern = roots.trailing_zeros() as usize;
                roots &= roots - 1;
                let order = self.acquire_direct(DirectPosition {
                    root: position,
                    next: position,
                    end: self.node_end(node),
                });
                self.cursor.states.push(State {
                    id: MatchId::from_raw(NONE),
                    captures: NONE,
                    order,
                    start_depth: 0,
                    step: plan.start_steps[pattern],
                    pattern: PatternIndex(pattern as u16),
                    consumed: 0,
                    flags: 0,
                });
            }

            let mut did_match = false;
            let mut index = 0;
            while index < self.cursor.states.len() {
                self.poll_at(node.slot());
                let mut state = self.cursor.states[index];
                let current = self.cursor.direct_states[state.order as usize];
                if state.has(DEAD) {
                    self.cursor.pool.release(state.captures);
                    self.release_direct(state.order);
                    self.cursor.states.remove(index);
                    continue;
                }
                if current.next != position {
                    index += 1;
                    continue;
                }

                let operation = plan.steps[state.step as usize];
                let step = self.step(state.step);
                let sibling = self.node_end(node);
                let symbol_matches =
                    symbol.wrapping_sub(operation.symbol_start) <= operation.symbol_span;
                let matches = symbol_matches
                    && (operation.field == 0
                        || FieldId::from_raw(operation.field) == node.field_id())
                    && (!operation.last_named_child
                        || self.named_child_position(sibling, current.end) == current.end);
                if !matches {
                    self.cursor.pool.release(state.captures);
                    self.release_direct(state.order);
                    self.cursor.states.remove(index);
                    continue;
                }
                if step.capture_ids[0] != DONE {
                    self.capture(&mut state, node, step);
                }
                if state.has(DEAD) {
                    self.release_direct(state.order);
                    self.cursor.states.remove(index);
                    continue;
                }

                state.step = if plan.local_patterns & (1 << state.pattern.raw()) != 0 {
                    plan.end_steps[state.pattern.raw() as usize]
                } else {
                    state.step + 1
                };
                let next_step = self.step(state.step);
                did_match |= stop_on_definite && next_step.has(ROOT_PATTERN_GUARANTEED);
                if next_step.depth == DONE {
                    self.release_direct(state.order);
                    self.finish(state);
                    self.cursor.states.remove(index);
                    did_match = true;
                } else {
                    let start = match plan.steps[state.step as usize].relation {
                        crate::query_plan::Relation::FirstNamedChild => position + 1,
                        _ => sibling,
                    };
                    self.cursor.direct_states[state.order as usize].next =
                        self.named_child_position(start, current.end);
                    self.cursor.states[index] = state;
                    index += 1;
                }
            }
            if did_match {
                return true;
            }
            if self.stopped {
                return false;
            }
        }
    }

    fn later_siblings(&self, cached: &mut Option<(bool, bool)>, named: bool) -> bool {
        let (later, later_named) = *cached.get_or_insert_with(|| {
            let mut later = false;
            if !self.cursor.parents.is_empty() {
                let mut next = self.current().next_sibling_including_empty();
                while let Some(node) = next {
                    later = true;
                    if node.is_named() {
                        return (true, true);
                    }
                    next = node.next_sibling_including_empty();
                }
            }
            (later, false)
        });
        if named { later_named } else { later }
    }

    fn later_field(&self, field: u16) -> bool {
        if self.cursor.parents.is_empty() {
            return false;
        }
        let mut next = self.current().next_sibling_including_empty();
        while let Some(node) = next {
            if node.field_id() == FieldId::from_raw(field) {
                return true;
            }
            next = node.next_sibling_including_empty();
        }
        false
    }

    fn fallible_step(&self, index: u16) -> bool {
        let step = self.step(index);
        let mut next = index + 1;
        while self.step(next).has(IS_PASS_THROUGH) {
            next += 1;
        }
        let next = self.step(next);
        next.depth != DONE
            && (next.depth > step.depth || (next.depth == step.depth && next.has(IS_IMMEDIATE)))
            && (!next.has(PARENT_PATTERN_GUARANTEED) || step.symbol == 0)
    }

    fn advance(&mut self, stop_on_definite: bool) -> bool {
        if self.stopped {
            return false;
        }
        self.cursor.first_capture_valid = false;
        if self.cursor.direct {
            return self.advance_direct(stop_on_definite);
        }
        let mut did_match = false;

        loop {
            if self.cursor.halted {
                while let Some(state) = self.cursor.states.pop() {
                    self.cursor.pool.release(state.captures);
                }
            }
            if did_match || self.cursor.halted {
                return did_match;
            }
            if self.poll() {
                return false;
            }

            if !self.cursor.ascending && self.cursor.scan_cooldown != 0 {
                self.cursor.scan_cooldown -= 1;
            }
            if !self.cursor.ascending
                && self.cursor.scan_cooldown == 0
                && self.cursor.states.is_empty()
                && self.cursor.optimized
                && !self.query.program.scan_symbols.is_empty()
                && self.cursor.max_start_depth == NONE
                && !self.scan_seek()
            {
                return false;
            }

            let depth = self.cursor.parents.len() as u32;
            if self.cursor.ascending {
                // States waiting at shallower depths cannot change when a
                // deeper node exits. The bound is refreshed during compaction.
                if depth <= self.cursor.states_max_depth {
                    let mut retained = 0;
                    for index in 0..self.cursor.states.len() {
                        let state = self.cursor.states[index];
                        let step = self.step(state.step);
                        if step.depth == DONE && (state.start_depth as u32 > depth || depth == 0) {
                            self.finish(state);
                            self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
                            did_match = true;
                        } else if step.depth != DONE
                            && state.start_depth as u32 + step.depth as u32 > depth
                        {
                            self.cursor.pool.release(state.captures);
                            self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
                        } else {
                            if retained != index {
                                self.cursor.states[retained] = state;
                            }
                            retained += 1;
                        }
                    }
                    self.cursor.states.truncate(retained);
                }

                if self.goto_next_sibling() {
                    self.cursor.ascending = false;
                } else if !self.goto_parent() {
                    self.cursor.halted = true;
                }
            } else {
                let node = self.current();
                let unrestricted = self.unrestricted;
                let parent_intersects = unrestricted
                    || self
                        .parent()
                        .is_none_or(|parent| self.cursor.range.intersects(parent));
                let intersects =
                    unrestricted || (parent_intersects && self.cursor.range.intersects(node));
                if self.containing_unrestricted || self.cursor.containing_range.contains(node) {
                    did_match |= self.enter(node, intersects, parent_intersects, stop_on_definite);
                }

                let descend = (intersects && depth < self.cursor.max_start_depth)
                    || self.cursor.states.iter().any(|state| {
                        let step = self.step(state.step);
                        step.depth != DONE && state.start_depth as u32 + step.depth as u32 > depth
                    });
                if descend
                    && (self.containing_unrestricted
                        || self.cursor.containing_range.intersects(node))
                    && self.goto_first_child()
                {
                    continue;
                }
                self.cursor.ascending = true;
            }
        }
    }

    fn enter(
        &mut self,
        node: Node<'tree>,
        intersects: bool,
        parent_intersects: bool,
        stop_on_definite: bool,
    ) -> bool {
        let query = self.query;
        let depth = self.cursor.parents.len() as u32;
        let symbol = node.data().symbol_index(node.slot().raw());
        let named = node.tables().named_index(symbol);
        let symbol = symbol.raw();
        let is_error = symbol as u32 == query.compiled.view.symbol_count;
        let field = if query.program.needs_fields && depth != 0 {
            node.field_id().map_or(0, FieldId::raw)
        } else {
            0
        };
        let mut siblings = None;
        let mut later_field = None;
        let mut first_updated = if depth > self.cursor.states_max_depth {
            self.cursor.states.len()
        } else {
            0
        };
        let patterns = query
            .program
            .pattern_map
            .get(symbol as usize)
            .copied()
            .unwrap_or(crate::native::Range {
                offset: 0,
                length: 0,
            });
        let entries = self.entries;
        let start_depth = if patterns.length == 0 {
            0
        } else {
            depth.wrapping_sub(
                self.step(entries[patterns.offset as usize].step_index)
                    .depth as u32,
            )
        };
        let mut wildcard = 0;
        let wildcard_count = if is_error {
            0
        } else {
            query.compiled.view.wildcard_root_pattern_count as usize
        };
        let mut concrete = patterns.offset as usize;

        // Merge the two pattern-ordered slices so starting concrete patterns
        // does not shift wildcard states that were just inserted.
        while wildcard < wildcard_count || concrete < patterns.end() {
            let is_wildcard = wildcard < wildcard_count
                && (concrete == patterns.end()
                    || entries[wildcard].pattern_index <= entries[concrete].pattern_index);
            let entry = if is_wildcard {
                let entry = entries[wildcard];
                wildcard += 1;
                entry
            } else {
                let entry = entries[concrete];
                concrete += 1;
                entry
            };
            let step = self.step(entry.step_index);
            let candidate_depth = if is_wildcard {
                depth.wrapping_sub(step.depth as u32)
            } else {
                start_depth
            };
            let in_range = if entry.flags & 1 != 0 {
                intersects
            } else {
                parent_intersects
                    && (!self.root_has_error
                        || self.parent().is_none_or(|parent| !parent.is_error()))
            };
            if in_range
                && (step.field == 0 || step.field == field)
                && (!is_wildcard || step.supertype_symbol == 0 || query.program.needs_supertypes)
                && candidate_depth <= self.cursor.max_start_depth
            {
                if entry.presence_requirement != 0
                    && !self.presence_matches(entry.presence_requirement, node)
                {
                    continue;
                }
                first_updated = first_updated.min(self.add_state(entry));
            }
        }

        let mut did_match = false;
        let mut index = first_updated;
        let mut pending_index = 0;
        // A stop requested inside state work takes effect after this node's
        // transition, so resumption cannot replay a partly applied transition.
        while index < self.cursor.states.len() || pending_index < self.cursor.pending.len() {
            self.poll();
            if index == self.cursor.states.len() {
                self.cursor.states.push(self.cursor.pending[pending_index]);
                pending_index += 1;
            }
            let mut state = self.cursor.states[index];
            let step = self.step(state.step);
            if state.start_depth as u32 + step.depth as u32 != depth {
                index += 1;
                continue;
            }

            let symbol_matches = if step.symbol == 0 {
                if step.has(IS_MISSING) {
                    node.is_missing()
                } else {
                    !is_error && (!step.has(IS_NAMED) || named)
                }
            } else {
                symbol == step.symbol && (!step.has(IS_MISSING) || node.is_missing())
            };
            if self.cursor.optimized
                && step.has(IS_LOCAL)
                && state.has(SEEKING_IMMEDIATE)
                && symbol_matches
            {
                self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
                if step.capture_ids[0] != DONE {
                    self.capture(&mut state, node, step);
                }
                state.step = (self.patterns[state.pattern.raw() as usize].steps.end() - 1) as u16;
                state.flags &= !(SEEKING_IMMEDIATE | SKIPPED_QUANTIFIER);
                did_match |= stop_on_definite && self.step(state.step).has(ROOT_PATTERN_GUARANTEED);
                self.cursor.states[index] = state;
                index += 1;
                continue;
            }

            let mut matches = symbol_matches;
            let mut later_can_match =
                !((step.has(IS_IMMEDIATE) && named && !state.has(SKIPPED_QUANTIFIER))
                    || state.has(SEEKING_IMMEDIATE))
                    && self.later_siblings(&mut siblings, false);
            if step.has(IS_LAST_CHILD) && self.later_siblings(&mut siblings, true) {
                matches = false;
            }
            if step.supertype_symbol != 0
                && !node.has_supertype(GrammarId::from_raw(step.supertype_symbol))
            {
                matches = false;
            }
            if step.field != 0 {
                if step.field == field && later_can_match {
                    if !*later_field.get_or_insert_with(|| self.later_field(field)) {
                        later_can_match = false;
                    }
                } else if step.field != field {
                    matches = false;
                }
            }
            if step.negated_field_list_id != 0 {
                let fields = unsafe { query.compiled.view.negated_fields.as_slice() };
                for field in fields[step.negated_field_list_id as usize..]
                    .iter()
                    .copied()
                    .take_while(|field| *field != 0)
                {
                    if node
                        .child_by_field_id(FieldId::from_raw(field).unwrap())
                        .is_some()
                    {
                        matches = false;
                        break;
                    }
                }
            }

            if !matches {
                if !later_can_match {
                    self.cursor.pool.release(state.captures);
                    self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
                    self.stage_remaining(index);
                    self.cursor.states.remove(index);
                } else {
                    index += 1;
                }
                continue;
            }
            self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
            self.stage_remaining(index);
            let mut copies = 0;
            if later_can_match
                && (step.has(CONTAINS_CAPTURES) || self.fallible_step(state.step))
                && self.copy_state(index)
            {
                copies += 1;
            }

            if state.has(NEEDS_PARENT) {
                if let Some(parent) = self.parent() {
                    state.flags &= !NEEDS_PARENT;
                    let mut skipped = state.step - 1;
                    while self.step(skipped).has(IS_DEAD_END | IS_PASS_THROUGH)
                        || self.step(skipped).depth > 0
                    {
                        skipped -= 1;
                    }
                    let parent_step = self.step(skipped);
                    if parent_step.capture_ids[0] != DONE {
                        self.capture(&mut state, parent, parent_step);
                    }
                } else {
                    state.flags |= DEAD;
                }
            }
            if step.capture_ids[0] != DONE {
                self.capture(&mut state, node, step);
            }
            if state.has(DEAD) {
                self.cursor.pool.release(state.captures);
                self.cursor.states.remove(index);
                index += copies;
                continue;
            }

            state.step += 1;
            let next_step = self.step(state.step);
            state.set(
                SEEKING_IMMEDIATE,
                step.symbol == 0 && !step.has(IS_NAMED) && next_step.has(IS_IMMEDIATE),
            );
            state.flags &= !SKIPPED_QUANTIFIER;
            self.cursor.states[index] = state;
            did_match |= stop_on_definite && next_step.has(ROOT_PATTERN_GUARANTEED);

            let mut branch = index;
            let mut branch_end = index + 1;
            while branch < branch_end {
                self.poll();
                let mut child = self.cursor.states[branch];
                let step = self.step(child.step);
                if step.alternative_index == DONE {
                    branch += 1;
                    continue;
                }
                if step.has(IS_DEAD_END) {
                    self.cursor.states[branch].step = step.alternative_index;
                    continue;
                }
                if step.has(IS_PASS_THROUGH) {
                    child.step += 1;
                    self.cursor.states[branch] = child;
                }
                if !(step.has(ALTERNATIVE_IS_SKIP)
                    && step.has(IS_LAST_CHILD)
                    && self.later_siblings(&mut siblings, true))
                    && self.copy_state(branch)
                {
                    branch_end += 1;
                    copies += 1;
                    let mut copy = self.cursor.states[branch + 1];
                    copy.step = step.alternative_index;
                    if step.has(IS_PASS_THROUGH) {
                        copy.flags |= SEEKING_IMMEDIATE;
                    }
                    if step.has(ALTERNATIVE_IS_SKIP) {
                        if !step.has(IS_IMMEDIATE) {
                            copy.set(SKIPPED_QUANTIFIER, self.step(copy.step).depth == step.depth);
                        } else if self.step(child.step - 1).depth < step.depth {
                            copy.flags |= SEEKING_IMMEDIATE;
                        }
                    }
                    self.cursor.states[branch + 1] = copy;
                }
                if !step.has(IS_PASS_THROUGH) {
                    branch += 1;
                }
            }
            index += 1 + copies;
        }
        self.cursor.pending.clear();
        did_match | self.deduplicate()
    }

    fn sort_states(&mut self) {
        let pool = &self.cursor.pool;
        let states = &mut self.cursor.states;
        let precedes = |left: State, right: State| {
            if left.start_depth != right.start_depth {
                return left.start_depth < right.start_depth;
            }
            if left.pattern != right.pattern {
                return left.pattern < right.pattern;
            }

            // Different depth/pattern groups need no capture lookup.
            let left = pool.list(left.captures);
            let right = pool.list(right.captures);
            (left.length != 0, left.first_byte) < (right.length != 0, right.first_byte)
        };
        // Enter events leave the array nearly sorted. Stable insertion avoids
        // allocating scratch and keeps discovery order among equal captures.
        for index in 1..states.len() {
            let state = states[index];
            if !precedes(state, states[index - 1]) {
                continue;
            }
            let mut position = index;
            while position != 0 && precedes(state, states[position - 1]) {
                states[position] = states[position - 1];
                position -= 1;
            }
            states[position] = state;
        }
    }

    fn needs_comparison_blocks(&self) -> bool {
        if self.cursor.states.len() < 256 {
            return false;
        }
        let mut previous = None;
        let mut run = 0;
        for state in &self.cursor.states {
            let captures = self.cursor.pool.list(state.captures);
            if captures.prefix == 0 {
                run = 0;
                continue;
            }
            let key = (state.start_depth, state.pattern, captures.first_byte);
            if previous != Some(key) {
                previous = Some(key);
                run = 0;
            }
            run += 1;
            if run == 256 {
                return true;
            }
        }
        false
    }

    fn index_captures(&mut self) {
        self.cursor.comparison_index.clear();
        self.cursor.comparison_blocks.clear();
        let count = self.cursor.states.len();
        if count < 64 {
            return;
        }
        self.cursor
            .comparison_index
            .resize(count, ComparisonEntry::default());
        if self.needs_comparison_blocks() {
            self.cursor
                .comparison_blocks
                .resize_with(count.div_ceil(64), ComparisonBlock::new);
        }
        let buckets = count.next_power_of_two().clamp(256, 65536);
        self.cursor.comparison_heads.resize(buckets, count);
        self.cursor.comparison_heads.fill(count);

        // Indexes remain stable until deduplication compacts tombstones. Hash
        // collisions merely add candidates; exact containment decides removal.
        for index in (0..count).rev() {
            let state = self.cursor.states[index];
            let captures = self.cursor.pool.list(state.captures);
            let mut entry = ComparisonEntry {
                next: count,
                end: index + 1,
                count: captures.length,
                first_byte: captures.first_byte,
            };
            if captures.prefix != 0 {
                if !self.cursor.comparison_blocks.is_empty() {
                    let block = &mut self.cursor.comparison_blocks[index / 64];
                    let slot = 1 << (index % 64);
                    let previous = block.valid;
                    block.valid |= slot;
                    for word in 0..2 {
                        let mut differing = captures.set[word] & !block.common[word];
                        // Common bits need no bitmap until a history omits one.
                        // Restore all earlier slots when that first happens.
                        if previous != 0 {
                            let mut missing = block.common[word] & !captures.set[word];
                            while missing != 0 {
                                block.bits[word * 64 + missing.trailing_zeros() as usize] =
                                    previous;
                                missing &= missing - 1;
                            }
                        }
                        block.common[word] &= captures.set[word];
                        block.combined[word] |= captures.set[word];
                        while differing != 0 {
                            block.bits[word * 64 + differing.trailing_zeros() as usize] |= slot;
                            differing &= differing - 1;
                        }
                    }
                }

                let bucket = (captures.hash ^ (captures.hash >> 32)) as usize & (buckets - 1);
                entry.next = self.cursor.comparison_heads[bucket];
                self.cursor.comparison_heads[bucket] = index;
                if index + 1 < count {
                    let next = self.cursor.states[index + 1];
                    let next_captures = self.cursor.pool.list(next.captures);
                    if state.pattern == next.pattern
                        && state.start_depth == next.start_depth
                        && next_captures.prefix != 0
                        && captures.length == next_captures.length
                    {
                        entry.end = self.cursor.comparison_index[index + 1].end;
                    }
                }
            }
            self.cursor.comparison_index[index] = entry;
        }
    }

    fn unique_start(&self, index: usize) -> bool {
        let states = &self.cursor.states;
        if states.len() - index < 5 {
            return false;
        }
        let first = states[index];
        let same_group = |state: &State| {
            state.start_depth == first.start_depth && state.pattern == first.pattern
        };
        if !same_group(&states[index + 4]) {
            return false;
        }
        let mut capture_id = None;
        for state in states[index..].iter().take_while(|state| same_group(state)) {
            if state.has(DEAD) {
                continue;
            }
            let Some(capture) = self.cursor.pool.get(state.captures).first() else {
                continue;
            };
            if let Some(expected) = capture_id {
                if capture.index != expected {
                    return false;
                }
            } else {
                capture_id = Some(capture.index);
                let quantifiers = unsafe {
                    self.query.compiled.view.capture_quantifiers.as_slice()
                        [state.pattern.raw() as usize]
                        .as_slice()
                };
                if !matches!(quantifiers.get(capture.index.raw() as usize), Some(1 | 2)) {
                    return false;
                }
            }
        }
        capture_id.is_some()
    }

    fn last_capture_end(&mut self, id: u32) -> u32 {
        let captures = self.cursor.pool.list(id);
        if captures.last_end != NONE {
            return captures.last_end;
        }
        let slot = self.cursor.pool.get(id).last().unwrap().node.id.slot();
        let end = self.root.at(slot).end_byte() as u32;
        self.cursor.pool.lists[id as usize].last_end = end;
        end
    }

    // Keep the large comparison pass out of the per-node matcher's register set.
    #[inline(never)]
    fn deduplicate(&mut self) -> bool {
        let dirty = self.cursor.dirty_patterns;
        if dirty == 0 {
            return false;
        }
        self.cursor.dirty_patterns = 0;
        for state in &mut self.cursor.states {
            if dirty & (1 << (state.pattern.raw() % 64)) != 0 {
                state.flags &= !HAS_ALTERNATIVES;
            }
        }
        if self.cursor.states_need_sort {
            self.sort_states();
            self.cursor.states_need_sort = false;
        }
        self.index_captures();
        if self.cursor.comparison_index.is_empty() {
            self.compare_states::<false>(dirty)
        } else {
            self.compare_states::<true>(dirty)
        }
    }

    // Small state sets do not use indexes. Select that path once per pass so
    // their inner loop carries no hash-bucket or capture-set bitmap state.
    fn compare_states<const INDEXED: bool>(&mut self, dirty: u64) -> bool {
        let mut group = None;
        let mut unique_start = false;
        let mut did_match = false;
        for index in 0..self.cursor.states.len() {
            self.poll();
            let mut state = self.cursor.states[index];
            if state.has(REMOVED) || dirty & (1 << (state.pattern.raw() % 64)) == 0 {
                continue;
            }
            if state.has(DEAD) {
                self.cursor.pool.release(state.captures);
                self.cursor.states[index].flags |= REMOVED;
                self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
                continue;
            }
            if group != Some((state.start_depth, state.pattern)) {
                group = Some((state.start_depth, state.pattern));
                unique_start = self.unique_start(index);
            }

            let captures = *self.cursor.pool.list(state.captures);
            let mut next_bucket = if INDEXED {
                self.cursor.comparison_index[index].next
            } else {
                self.cursor.states.len()
            };
            let mut comparison_block = usize::MAX;
            let mut candidates = 0;
            let mut other_index = index + 1;
            while other_index < self.cursor.states.len() {
                self.poll();
                let mut other = self.cursor.states[other_index];
                if other.has(REMOVED) {
                    other_index += 1;
                    continue;
                }
                if other.start_depth != state.start_depth || other.pattern != state.pattern {
                    break;
                }
                let other_entry = if INDEXED {
                    Some(self.cursor.comparison_index[other_index])
                } else {
                    None
                };
                let (other_count, other_start) = if let Some(entry) = other_entry {
                    (entry.count, entry.first_byte)
                } else {
                    let captures = self.cursor.pool.list(other.captures);
                    (captures.length, captures.first_byte)
                };
                if captures.length != 0
                    && other_count != 0
                    && ((unique_start && other_start > captures.first_byte)
                        || other_start >= self.last_capture_end(state.captures))
                {
                    break;
                }

                if INDEXED
                    && !self.cursor.comparison_blocks.is_empty()
                    && captures.prefix != 0
                    && (self.query.program.repeated_captures || captures.length != other_count)
                {
                    if comparison_block != other_index / 64 {
                        comparison_block = other_index / 64;
                        candidates = self.cursor.comparison_blocks[comparison_block]
                            .candidates(captures.set);
                    }
                    let remaining = candidates & (u64::MAX << (other_index % 64));
                    let next = (comparison_block * 64 + remaining.trailing_zeros() as usize)
                        .min(self.cursor.states.len());
                    if next > other_index {
                        other_index = next;
                        continue;
                    }
                }

                let other_captures = self.cursor.pool.list(other.captures);
                if let Some(entry) = other_entry {
                    if captures.prefix != 0
                        && other_captures.prefix != 0
                        && captures.length == other_captures.length
                        && captures.hash != other_captures.hash
                    {
                        while next_bucket <= other_index {
                            next_bucket = self.cursor.comparison_index[next_bucket].next;
                        }
                        other_index = entry.end.min(next_bucket);
                        continue;
                    }
                }

                let (contains_other, contains_state) = self.cursor.pool.containment(
                    self.cursor.pool.list(state.captures),
                    other_captures,
                    self.root,
                );
                if contains_other {
                    if state.step == other.step
                        && (other.has(SEEKING_IMMEDIATE) || !state.has(SEEKING_IMMEDIATE))
                    {
                        self.cursor.pool.release(other.captures);
                        self.cursor.states[other_index].flags |= REMOVED;
                        self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
                        other_index += 1;
                        continue;
                    }
                    other.flags |= HAS_ALTERNATIVES;
                    self.cursor.states[other_index].flags = other.flags;
                }
                if contains_state {
                    if state.step == other.step
                        && (state.has(SEEKING_IMMEDIATE) || !other.has(SEEKING_IMMEDIATE))
                    {
                        self.cursor.pool.release(state.captures);
                        state.flags |= REMOVED;
                        self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
                        break;
                    }
                    state.flags |= HAS_ALTERNATIVES;
                }
                other_index += 1;
            }

            if !state.has(REMOVED)
                && self.step(state.step).depth == DONE
                && !state.has(HAS_ALTERNATIVES)
            {
                self.finish(state);
                state.flags |= REMOVED;
                self.cursor.dirty_patterns |= 1 << (state.pattern.raw() % 64);
                did_match = true;
            }
            self.cursor.states[index].flags = state.flags;
        }

        self.cursor.states_max_depth = 0;
        let mut retained = 0;
        for index in 0..self.cursor.states.len() {
            let state = self.cursor.states[index];
            if state.has(REMOVED) {
                continue;
            }
            let step_depth = self.step(state.step).depth;
            let depth = state.start_depth as u32
                + if step_depth == DONE {
                    0
                } else {
                    step_depth as u32
                };
            self.cursor.states_max_depth = self.cursor.states_max_depth.max(depth);
            if retained != index {
                self.cursor.states[retained] = state;
            }
            retained += 1;
        }
        self.cursor.states.truncate(retained);
        did_match
    }
}

fn equal_column(
    data: &crate::storage::ForestData,
    address: ColumnPointer,
    group: u32,
    value: u16,
    mask: u16,
    width: u32,
) -> u64 {
    use crate::storage::GROUP_SIZE;
    if width == 1 {
        let bytes = data.column_slice(address, (group * GROUP_SIZE) as usize, GROUP_SIZE as usize);
        return crate::scan::equal_byte_ids(bytes, &[SquatterKindId(value)])
            & (u64::MAX >> (64 - GROUP_SIZE + data.waste(group)));
    }
    let mut matches = 0;
    #[cfg(target_arch = "x86_64")]
    {
        let column = data.column_slice(
            address,
            (group * GROUP_SIZE) as usize * 2,
            GROUP_SIZE as usize * 2,
        );
        let simd = Level::baseline().as_sse2().unwrap();
        for (index, bytes) in column.chunks_exact(32).enumerate() {
            matches |= (sse2_equal_column(simd, bytes, value, mask) as u64) << (index * 16);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    for lane in 0..GROUP_SIZE {
        matches |=
            ((data.short(address, group * GROUP_SIZE + lane) & mask == value) as u64) << lane;
    }
    matches & (u64::MAX >> (64 - GROUP_SIZE + data.waste(group)))
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    use crate::{Forest, Language, PackOptions};

    fn collect_plan_results(
        cursor: &mut QueryCursor,
        query: &Query,
        root: Node<'_>,
        source: &[u8],
        captures: bool,
        direct: bool,
    ) -> Vec<(PatternIx, Vec<(u32, CaptureIx)>)> {
        let mut execution = cursor.execute(query, root, source);
        assert_eq!(execution.cursor.direct, direct);
        let mut results = Vec::new();
        loop {
            let next = if captures {
                execution
                    .next_capture()
                    .map(|(result, index)| (result, Some(index)))
            } else {
                execution.next_match().map(|result| (result, None))
            };
            let Some((result, index)) = next else { break };
            let captures = index.map_or(result.captures(), |index| {
                &result.captures()[index.raw() as usize..index.raw() as usize + 1]
            });
            results.push((
                result.pattern_index,
                captures
                    .iter()
                    .map(|capture| (capture.node.slot().raw(), capture.index))
                    .collect(),
            ));
            assert!(results.len() < 10_000);
        }
        assert_eq!(execution.error(), None);
        results.sort();
        results
    }

    #[test]
    fn error_plans_match_general_execution() {
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let grammar = Language::new(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();

        for source in ["x", "[\n1, ?,\n2]", "{\"key\":}", "[1,"] {
            let tree = Forest::parse(&grammar, &mut parser, source).unwrap();
            assert!(tree.root_node().has_error());
            for pattern in [
                "(ERROR) @error",
                "[(ERROR) (number)] @value",
                "(ERROR) @error (number) @number",
                "(_) @node",
                "(_ . (_) @first)",
                "(_ . (_) @first . (_) @second .)",
                "(ERROR . (_) @first)",
                "(_ . (ERROR) @first)",
            ] {
                let query = Query::new(&grammar, pattern).unwrap();
                for root in tree.root_node().preorder().nodes() {
                    for range in [0..0, 0..1, 1..source.len(), source.len()..source.len() + 1] {
                        for captures in [false, true] {
                            let collect = |optimized| {
                                let mut cursor = QueryCursor::new();
                                cursor.set_optimized(optimized);
                                cursor.set_byte_range(range.clone());
                                let results = collect_plan_results(
                                    &mut cursor,
                                    &query,
                                    root,
                                    source.as_bytes(),
                                    captures,
                                    optimized,
                                );
                                assert!(results.len() < 100);
                                results
                            };
                            assert_eq!(
                                collect(true),
                                collect(false),
                                "{pattern}, {source:?}, {root:?}, {range:?}, captures={captures}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn bounded_plans_match_general_execution() {
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let grammar = Language::new(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let separator = format!(",\n{}", " ".repeat(300));
        let source = format!(
            "[{}]",
            ["[1,2,3],{\"a\":[4,5,6]},true,null,false,\"text\""; 4].join(&separator)
        );
        let native = parser.parse(&source, None).unwrap();
        assert!(!native.root_node().has_error());
        let point = |offset: usize| {
            let prefix = &source[..offset.min(source.len())];
            Point::new(
                prefix.bytes().filter(|byte| *byte == b'\n').count(),
                prefix.rsplit('\n').next().unwrap().len() + offset.saturating_sub(source.len()),
            )
        };

        for symbol_presence in [false, true] {
            let tree = Forest::pack_with_options(
                &grammar,
                &native,
                PackOptions {
                    symbol_presence: &|_| symbol_presence,
                    ..Default::default()
                },
            )
            .unwrap();
            assert!((0..tree.group_count()).any(|group| tree.data().waste(group) != 0));
            let selected = tree
                .root_node()
                .preorder()
                .nodes()
                .filter(|node| node.kind() == "array")
                .nth(3)
                .unwrap();
            let start = selected.start_byte();
            let end = selected.end_byte();
            for (pattern, direct) in [
                ("(number) @value", true),
                ("[(number) (string) (true) (null)] @value", true),
                ("[(number) (string) (true) (null) (false)] @value", true),
                (
                    "(array . (number) @first . (number) @second . (number) @third .) @array",
                    true,
                ),
                ("(array (number)+ @values) @array", false),
                ("((number) @first (number) @second)", false),
                ("(number)+ @values", false),
            ] {
                let query = Query::new(&grammar, pattern).unwrap();
                assert!(!query.program.scan_symbols.is_empty());
                for root in [tree.root_node(), selected] {
                    for range in [
                        0..0,
                        0..1,
                        start..end,
                        start + 2..start + 3,
                        start + 3..start + 3,
                        end..end + 1,
                        source.len()..source.len() + 1,
                    ] {
                        for (points, containing) in
                            [(false, false), (true, false), (false, true), (true, true)]
                        {
                            for captures in [false, true] {
                                let collect = |optimized| {
                                    let mut cursor = QueryCursor::new();
                                    cursor.set_optimized(optimized);
                                    if containing && points {
                                        cursor.set_containing_point_range(
                                            point(range.start)..point(range.end),
                                        );
                                    } else if containing {
                                        cursor.set_containing_byte_range(range.clone());
                                    } else if points {
                                        cursor
                                            .set_point_range(point(range.start)..point(range.end));
                                    } else {
                                        cursor.set_byte_range(range.clone());
                                    }
                                    let mut results = collect_plan_results(
                                        &mut cursor,
                                        &query,
                                        root,
                                        source.as_bytes(),
                                        captures,
                                        optimized && direct,
                                    );
                                    if captures {
                                        results.dedup();
                                    }
                                    results
                                };
                                assert_eq!(
                                    collect(true),
                                    collect(false),
                                    "{pattern}, {range:?}, points={points}, containing={containing}, captures={captures}, root={root:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn root_search_respects_ranges_and_group_waste() {
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let grammar = Language::new(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let separator = format!(",{}", " ".repeat(300));
        let source = format!("[{}]", ["1,\"text\",true"; 8].join(&separator));
        let native = parser.parse(&source, None).unwrap();
        assert!(!native.root_node().has_error());

        for symbol_presence in [false, true] {
            let tree = Forest::pack_with_options(
                &grammar,
                &native,
                PackOptions {
                    symbol_presence: &|_| symbol_presence,
                    ..Default::default()
                },
            )
            .unwrap();
            assert!((0..tree.group_count()).any(|group| tree.data().waste(group) != 0));
            for pattern in [
                "(number) @value",
                "[(number) (string)] @value",
                "[(number) (string) (true)] @value",
                "[(number) (string) (true) (null)] @value",
                "(null) @value",
            ] {
                let query = Query::new(&grammar, pattern).unwrap();
                let mut cursor = QueryCursor::new();
                let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
                let total = execution.total_slots();
                for start in 0..=total {
                    let start = execution.normalize_position(start);
                    for end in start..=total {
                        let expected = (start..end)
                            .find(|&position| {
                                execution.normalize_position(position) == position
                                    && query.program.scan_targets.contains(
                                        &tree.data().symbol_index(total - 1 - position).raw(),
                                    )
                            })
                            .unwrap_or(end);
                        assert_eq!(execution.find_symbols(start, end), expected, "{pattern}");
                        assert_eq!(execution.find_symbols_control(start, end), expected);
                    }
                }
            }
        }
    }
}
