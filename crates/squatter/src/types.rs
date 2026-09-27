use std::num::NonZeroU16;

macro_rules! integer_type {
    ($(#[$attribute:meta])* $visibility:vis $name:ident($integer:ty)) => {
        $(#[$attribute])*
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        $visibility struct $name(pub(crate) $integer);

        impl $name {
            #[inline]
            pub const fn get(self) -> $integer {
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
    pub GrammarKindId(u16));
integer_type!(
    /// An absolute physical slot in a particular tree's reverse-preorder storage.
    ///
    /// **Not in Tree-sitter**
    pub SlotIx(u32));

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
    pub const fn new(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[inline]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl From<NonZeroU16> for FieldId {
    fn from(value: NonZeroU16) -> Self {
        Self(value)
    }
}

integer_type!(pub(crate) GroupIx(u32));
integer_type!(pub(crate) GroupSlotIx(u32));
integer_type!(pub(crate) SlabOffset(u32));
integer_type!(
    /// Displayed kind with error sentinels moved after the grammar's symbols.
    pub(crate) RemappedKindId(u16));
integer_type!(
    /// Original grammar kind with error sentinels moved after the grammar's symbols.
    pub(crate) RemappedGrammarKindId(u16));
integer_type!(pub(crate) CaptureId(u32));
integer_type!(pub(crate) PatternIndex(u16));
integer_type!(pub(crate) MatchId(u32));
integer_type!(pub(crate) QueryStringId(u32));
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

impl From<u16> for KindId {
    fn from(value: u16) -> Self {
        Self(value)
    }
}
impl From<u16> for GrammarKindId {
    fn from(value: u16) -> Self {
        Self(value)
    }
}
impl From<FieldId> for u16 {
    fn from(value: FieldId) -> Self {
        value.get()
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
        SlotIx(self.first_slot().get() + slot.get())
    }
}

impl From<KindId> for u16 {
    fn from(value: KindId) -> Self {
        value.get()
    }
}
impl From<GrammarKindId> for u16 {
    fn from(value: GrammarKindId) -> Self {
        value.get()
    }
}
impl From<SlotIx> for u32 {
    fn from(value: SlotIx) -> Self {
        value.get()
    }
}

impl KindId {
    /// The grammar-independent kind of a visible `ERROR` node.
    pub const ERROR: Self = Self(u16::MAX);

    /// Wrap a raw ID without checking membership in a grammar.
    #[inline]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }
}
impl GrammarKindId {
    pub(crate) fn from_slice(symbols: &[u16]) -> &[Self] {
        // GrammarKindId is transparent over u16 and accepts every symbol value.
        unsafe { std::slice::from_raw_parts(symbols.as_ptr().cast(), symbols.len()) }
    }

    /// Wrap a raw ID without checking membership in a grammar.
    #[inline]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }
}
impl SlotIx {
    /// Wrap a raw slot without checking whether it holds a node.
    #[inline]
    pub const fn new(value: u32) -> Self {
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
    /// node.child(tree_squatter::NamedChildIx::new(0));
    /// # }
    /// ```
    pub ChildIx(u32));
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
    ///
    /// ```compile_fail
    /// # fn example(node: tree_squatter::Node<'_>) {
    /// node.named_child(tree_squatter::ChildIx::new(0));
    /// # }
    /// ```
    pub NamedChildIx(u32));
impl NamedChildIx {
    /// Wrap a position or count without checking whether a child exists.
    #[inline]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
}
