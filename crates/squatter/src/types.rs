use std::num::NonZeroU16;

macro_rules! integer_type {
    ($(#[$attribute:meta])* $visibility:vis $name:ident(pub $integer:ty)) => {
        $(#[$attribute])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        $visibility struct $name(pub $integer);

        impl $name {
            #[inline]
            pub const fn raw(self) -> $integer {
                self.0
            }
        }
    };
    ($(#[$attribute:meta])* $visibility:vis $name:ident($integer:ty)) => {
        $(#[$attribute])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        $visibility struct $name(pub(crate) $integer);

        impl $name {
            #[inline]
            pub const fn raw(self) -> $integer {
                self.0
            }
        }
    };
}

integer_type!(
    /// A displayed node kind, including aliases, in a particular grammar.
    ///
    /// **Not in Tree-sitter:** it uses `u16` instead.
    ///
    /// ```compile_fail
    /// # fn example(root: tree_squatter::Node<'_>) {
    /// root.all().filter_kind_ids([root.grammar_id()]);
    /// # }
    /// ```
    pub KindId(u16));
integer_type!(
    /// A node kind in the original grammar, ignoring aliases.
    ///
    /// **Not in Tree-sitter:** it uses `u16` instead.
    pub GrammarId(u16));
integer_type!(
    /// An absolute physical slot in a forest's reverse-preorder storage.
    ///
    /// **Not in Tree-sitter**
    #[derive(Default)]
    pub(crate) SlotIx(u32));

integer_type!(
    /// A physical tree index, local to one forest.
    pub TreeIx(u32));
integer_type!(
    /// A region index, local to one forest.
    pub RegionIx(u32));

impl TreeIx {
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }
}

/// A tree index in the upper 32 bits and a forest-global slot in the lower 32 bits.
/// Rebuilding or reordering a forest can change both indices.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct NodeId(u64);

impl NodeId {
    pub(crate) const fn new(tree: TreeIx, slot: SlotIx) -> Self {
        Self(((tree.raw() as u64) << 32) | slot.raw() as u64)
    }

    pub const fn tree(self) -> TreeIx {
        TreeIx::from_raw((self.0 >> 32) as u32)
    }

    pub(crate) const fn slot(self) -> SlotIx {
        SlotIx::from_raw(self.0 as u32)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// A nonzero field identifier in a particular grammar. Absence is `None`.
///
/// **Not in Tree-sitter:** it uses `NonZeroU16` instead.
///
/// ```compile_fail
/// # fn example(root: tree_squatter::Node<'_>) {
/// root.all().filter_field_id(root.kind_id());
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct FieldId(NonZeroU16);

impl FieldId {
    /// Rejects zero; membership in a grammar is not checked.
    #[inline]
    pub const fn from_raw(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[inline]
    pub const fn raw(self) -> u16 {
        self.0.get()
    }
}

impl From<NonZeroU16> for FieldId {
    fn from(value: NonZeroU16) -> Self {
        Self(value)
    }
}

integer_type!(
    /// A physical group index, local to one forest.
    pub GroupIx(u32));
integer_type!(
    /// A physical slot relative to one group.
    ///
    /// ```compile_fail
    /// # fn example(group: tree_squatter::scan::GroupRef<'_>) {
    /// group.node(group.index());
    /// # }
    /// ```
    pub GroupSlotIx(u32));

impl GroupIx {
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }
}
impl GroupSlotIx {
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }
}
integer_type!(#[derive(Default)]
    pub(crate) SlabOffset(u32));

integer_type!(
    /// A compact displayed kind in one prepared language. Zero is reserved;
    /// error IDs follow the concrete kinds. Convert through [`crate::Language`].
    ///
    /// ```compile_fail
    /// # fn example(root: tree_squatter::Node<'_>) {
    /// root.all().filter_kind_ids([root.squatter_kind_id()]);
    /// # }
    /// ```
    #[derive(Default)]
    pub SquatterKindId(u16));
integer_type!(
    /// A compact original grammar symbol in one prepared language, ignoring aliases.
    /// Zero is reserved; convert through [`crate::Language`].
    pub SquatterGrammarId(u16));
integer_type!(pub(crate) PatternIndex(u16));
integer_type!(pub(crate) QueryStringId(u32));
integer_type!(pub(crate) QueryStepIx(u16));
integer_type!(pub(crate) NegatedFieldListIx(u16));
integer_type!(pub(crate) PresenceRequirementIx(u16));

impl QueryStepIx {
    pub(crate) const NONE: Self = Self(u16::MAX);
}

integer_type!(
    /// Row in the upper 32 bits and column in the lower 32 bits.
    pub(crate) PackedPoint(u64));

const _: () = {
    assert!(size_of::<FieldId>() == size_of::<u16>());
    assert!(size_of::<Option<FieldId>>() == size_of::<u16>());
    assert!(align_of::<Option<FieldId>>() == align_of::<u16>());
};

impl std::ops::Add<u32> for SlabOffset {
    type Output = Self;
    fn add(self, bytes: u32) -> Self {
        Self(self.0 + bytes)
    }
}

impl std::ops::Sub<u32> for SlabOffset {
    type Output = Self;
    fn sub(self, bytes: u32) -> Self {
        Self(self.0 - bytes)
    }
}

impl PackedPoint {
    pub(crate) fn from_point_cast(point: tree_sitter::Point) -> Self {
        Self((u64::from(point.row as u32) << 32) | u64::from(point.column as u32))
    }

    pub(crate) fn from_point(point: tree_sitter::Point) -> Option<Self> {
        Some(Self(
            (u64::from(u32::try_from(point.row).ok()?) << 32)
                | u64::from(u32::try_from(point.column).ok()?),
        ))
    }

    pub(crate) fn point(self) -> tree_sitter::Point {
        tree_sitter::Point::new((self.0 >> 32) as usize, (self.0 as u32) as usize)
    }
}

impl From<FieldId> for u16 {
    fn from(value: FieldId) -> Self {
        value.raw()
    }
}

impl std::ops::Add<u64> for PackedPoint {
    type Output = Self;
    fn add(self, delta: u64) -> Self {
        Self(self.0 + delta)
    }
}
impl std::ops::Sub<u64> for PackedPoint {
    type Output = Self;
    fn sub(self, delta: u64) -> Self {
        Self(self.0 - delta)
    }
}
impl PackedPoint {
    pub(crate) fn saturating_add(self, delta: u64) -> Self {
        Self(self.0.saturating_add(delta))
    }
    pub(crate) fn saturating_sub(self, delta: u64) -> Self {
        Self(self.0.saturating_sub(delta))
    }
}

impl SlotIx {
    pub(crate) fn group(self) -> GroupIx {
        GroupIx(self.0 / crate::storage::GROUP_SIZE)
    }
    pub(crate) fn in_group(self) -> GroupSlotIx {
        GroupSlotIx(self.0 % crate::storage::GROUP_SIZE)
    }
}
impl GroupIx {
    pub(crate) fn first_slot(self) -> SlotIx {
        SlotIx(self.0 * crate::storage::GROUP_SIZE)
    }
    pub(crate) fn slot(self, slot: GroupSlotIx) -> SlotIx {
        SlotIx(self.first_slot().raw() + slot.raw())
    }
}

impl From<KindId> for u16 {
    fn from(value: KindId) -> Self {
        value.raw()
    }
}
impl From<GrammarId> for u16 {
    fn from(value: GrammarId) -> Self {
        value.raw()
    }
}
impl From<SlotIx> for u32 {
    fn from(value: SlotIx) -> Self {
        value.raw()
    }
}

impl KindId {
    /// The grammar-independent kind of a visible `ERROR` node.
    pub const ERROR: Self = Self(u16::MAX);

    /// Wrap a raw ID without checking membership in a grammar.
    #[inline]
    pub const fn from_raw(value: u16) -> Self {
        Self(value)
    }
}
impl GrammarId {
    pub(crate) fn from_slice(symbols: &[u16]) -> &[Self] {
        // GrammarId is transparent over u16 and accepts every symbol value.
        unsafe { std::slice::from_raw_parts(symbols.as_ptr().cast(), symbols.len()) }
    }

    /// Wrap a raw ID without checking membership in a grammar.
    #[inline]
    pub const fn from_raw(value: u16) -> Self {
        Self(value)
    }
}

impl SquatterKindId {
    /// Wrap an ID without checking membership in a prepared language.
    #[inline]
    pub const fn from_raw(value: u16) -> Self {
        Self(value)
    }
}

impl SquatterGrammarId {
    /// Wrap an ID without checking membership in a prepared language.
    #[inline]
    pub const fn from_raw(value: u16) -> Self {
        Self(value)
    }
}
impl SlotIx {
    /// Wrap a raw slot without checking whether it holds a node.
    #[inline]
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }
}

integer_type!(
    /// A position or exclusive count among all children.
    /// Counts are exclusive upper bounds, not existing positions. This is distinct
    /// from named-child indices and physical slots.
    ///
    /// **Not in Tree-sitter:** it uses `u32` instead.
    ///
    /// ```compile_fail
    /// # fn example(node: tree_squatter::Node<'_>) {
    /// node.child(tree_squatter::NamedChildIx(0));
    /// # }
    /// ```
    pub ChildIx(pub u32));
impl ChildIx {
    /// Wrap a position or count without checking whether a child exists.
    #[inline]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
}

integer_type!(
    /// A position or exclusive count among named children.
    /// Counts are exclusive upper bounds, not existing positions. This is distinct
    /// from all-child indices and physical slots.
    ///
    /// **Not in Tree-sitter:** it uses `u32` indices and `usize` counts instead.
    pub NamedChildIx(pub u32));
impl NamedChildIx {
    /// Wrap a position or count without checking whether a child exists.
    #[inline]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
}

/// A query-global pattern index.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PatternIx(pub usize);

impl PatternIx {
    pub const fn raw(self) -> usize {
        self.0
    }
}

/// A query-global capture-name index.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct CaptureIx(pub u32);

impl CaptureIx {
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A match identity within one execution.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MatchId(u32);

impl MatchId {
    pub(crate) const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A position within a match’s capture slice.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MatchCaptureIx(u32);

impl MatchCaptureIx {
    pub(crate) const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

integer_type!(pub(crate) CaptureListIx(u32));
integer_type!(#[derive(Default)]
    pub(crate) CaptureStorageIx(u32));
integer_type!(pub(crate) CapturePrefixId(u64));

impl CaptureListIx {
    pub(crate) const NONE: Self = Self(u32::MAX);
}
impl CaptureStorageIx {
    pub(crate) const NONE: Self = Self(u32::MAX);
}

integer_type!(
    /// Identifies the packed slab format for persistence compatibility.
    pub RepresentationId(u64));

integer_type!(pub(crate) ProductionId(u16));
integer_type!(pub(crate) ReductionIx(u32));
integer_type!(pub(crate) SupertypeIx(u16));
integer_type!(#[derive(Default)]
    pub(crate) SupertypeMask(u16));

impl ReductionIx {
    pub(crate) const NONE: Self = Self(u32::MAX);
}

integer_type!(pub(crate) QueryCaptureIx(u16));

impl QueryCaptureIx {
    pub(crate) const NONE: Self = Self(u16::MAX);
}

impl From<QueryCaptureIx> for CaptureIx {
    fn from(value: QueryCaptureIx) -> Self {
        Self(u32::from(value.raw()))
    }
}

impl std::ops::AddAssign<u32> for MatchCaptureIx {
    fn add_assign(&mut self, captures: u32) {
        self.0 += captures;
    }
}

macro_rules! index_arithmetic {
    ($name:ident, $integer:ty) => {
        impl std::ops::Add<$integer> for $name {
            type Output = Self;
            fn add(self, count: $integer) -> Self {
                Self(self.0 + count)
            }
        }
        impl std::ops::Sub<$integer> for $name {
            type Output = Self;
            fn sub(self, count: $integer) -> Self {
                Self(self.0 - count)
            }
        }
        impl std::ops::Sub for $name {
            type Output = $integer;
            fn sub(self, other: Self) -> $integer {
                self.0 - other.0
            }
        }
        impl std::ops::AddAssign<$integer> for $name {
            fn add_assign(&mut self, count: $integer) {
                self.0 += count;
            }
        }
        impl std::ops::SubAssign<$integer> for $name {
            fn sub_assign(&mut self, count: $integer) {
                self.0 -= count;
            }
        }
    };
}

index_arithmetic!(SlotIx, u32);
index_arithmetic!(GroupIx, u32);
index_arithmetic!(TreeIx, u32);
index_arithmetic!(QueryStepIx, u16);

integer_type!(
    /// A physical slot counted from the forest's end in preorder direction.
    #[derive(Default)]
    pub(crate) PreorderIx(u32));
integer_type!(pub(crate) DirectStateIx(u32));

impl PreorderIx {
    pub(crate) fn from_slot(slot: SlotIx, total_slots: u32) -> Self {
        Self(total_slots - 1 - slot.raw())
    }

    pub(crate) fn slot(self, total_slots: u32) -> SlotIx {
        SlotIx(total_slots - 1 - self.0)
    }
}

impl DirectStateIx {
    pub(crate) const NONE: Self = Self(u32::MAX);
}

index_arithmetic!(PreorderIx, u32);
