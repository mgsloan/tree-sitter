use crate::types::{GroupIx, SlotIx, SquatterKindId};
use crate::{
    Error, Forest, ForestRegion,
    storage::{GROUP_SIZE, Slab, StableSlab, slab_format},
    types::PackedPoint,
};
use smallvec::SmallVec;
use std::{
    ops::ControlFlow,
    ptr::{self, NonNull},
};

const PRESENCE_FORMAT: u32 = slab_format(0xfe, 0);
const ABSENT_PRESENCE_FORMAT: u32 = slab_format(0xfc, 0);
const POINT_FORMAT: u32 = slab_format(0xfd, 0);
const HEADER_BYTES: usize = 16;

/// Failure to build, load, validate, or attach separate tree data.
///
/// **Not in Tree-sitter**
#[derive(Debug)]
pub enum SideDataError {
    InvalidTarget,
    Core(Error),
}

impl From<Error> for SideDataError {
    fn from(error: Error) -> Self {
        Self::Core(error)
    }
}
impl From<SideDataError> for Error {
    fn from(error: SideDataError) -> Self {
        match error {
            SideDataError::Core(error) => error,
            _ => Error::InvalidArgument,
        }
    }
}

impl std::fmt::Display for SideDataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for SideDataError {}

struct Sidecar(Slab);

impl Sidecar {
    fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        self.0.bytes_mut()
    }
    fn word(&self, byte: usize) -> u64 {
        u64::from_le_bytes(self.bytes()[byte..byte + 8].try_into().unwrap())
    }
    fn put_word(&mut self, byte: usize, value: u64) {
        self.bytes_mut()[byte..byte + 8].copy_from_slice(&value.to_le_bytes());
    }
    fn header(
        &mut self,
        offset: usize,
        format: u32,
        groups: u32,
        dimension: u32,
    ) -> Result<(), SideDataError> {
        let bytes = &mut self.bytes_mut()[offset..offset + HEADER_BYTES];
        unsafe { write_header(bytes.as_mut_ptr(), format, groups, dimension) }
            .map_err(SideDataError::from)
    }
}

unsafe fn write_header(
    destination: *mut u8,
    format: u32,
    groups: u32,
    dimension: u32,
) -> Result<(), Error> {
    let slots = groups.checked_mul(GROUP_SIZE).ok_or(Error::Overflow)?;
    for (index, word) in [format, groups, slots, dimension].into_iter().enumerate() {
        unsafe {
            destination
                .add(index * 4)
                .cast::<u32>()
                .write_unaligned(word.to_le());
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Header {
    format: u32,
    groups: u32,
    dimension: u32,
}

fn header(bytes: &[u8]) -> Result<Header, SideDataError> {
    if bytes.len() < HEADER_BYTES {
        return Err(SideDataError::InvalidTarget);
    }
    let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let groups = word(4);
    if groups.checked_mul(GROUP_SIZE) != Some(word(8)) {
        return Err(SideDataError::InvalidTarget);
    }
    Ok(Header {
        format: word(0),
        groups,
        dimension: word(12),
    })
}

fn presence_length(groups: u32, symbols: u32, present: bool) -> Result<usize, SideDataError> {
    let words = if present {
        (groups as usize).div_ceil(64)
    } else {
        0
    };
    words
        .checked_mul(symbols as usize)
        .and_then(|words| words.checked_mul(8))
        .and_then(|bytes| bytes.checked_add(HEADER_BYTES))
        .ok_or(Error::Overflow.into())
}

fn presence_records(bytes: &[u8]) -> Result<Vec<(usize, Header)>, SideDataError> {
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let record = header(&bytes[offset..])?;
        if !matches!(record.format, PRESENCE_FORMAT | ABSENT_PRESENCE_FORMAT)
            || record.groups == 0
            || record.dimension < 2
            || record.dimension > 65536
        {
            return Err(SideDataError::InvalidTarget);
        }
        let length = presence_length(
            record.groups,
            record.dimension,
            record.format == PRESENCE_FORMAT,
        )?;
        let end = offset
            .checked_add(length)
            .filter(|&end| end <= bytes.len())
            .ok_or(SideDataError::InvalidTarget)?;
        records.try_reserve(1).map_err(|_| Error::Allocation)?;
        records.push((offset, record));
        offset = end;
    }
    Ok(records)
}

/// Separately owned symbol membership data for selected forest regions.
/// Coverage changes scan cost, never results.
pub struct PresenceCache(Sidecar);

impl PresenceCache {
    pub fn build(forest: &Forest) -> Result<Self, SideDataError> {
        Self::build_selected(forest, |_| true)
    }

    /// Evaluates the predicate once per region, in physical order.
    pub fn build_selected(
        forest: &Forest,
        select: impl Fn(ForestRegion<'_>) -> bool,
    ) -> Result<Self, SideDataError> {
        Self::build_selected_with_cancellation(forest, select, || ControlFlow::Continue(()))
    }

    /// Checks cancellation between regions and groups. Canceled construction
    /// returns `SideDataError::Core(Error::Canceled)` without changing the forest.
    pub fn build_selected_with_cancellation(
        forest: &Forest,
        select: impl Fn(ForestRegion<'_>) -> bool,
        cancellation_callback: impl FnMut() -> ControlFlow<()>,
    ) -> Result<Self, SideDataError> {
        Ok(Self::build_selected_inner(forest, select, cancellation_callback, false)?.unwrap())
    }

    pub(crate) fn build_for_packing(
        forest: &Forest,
        select: impl Fn(ForestRegion<'_>) -> bool,
        cancellation_callback: Option<&dyn Fn() -> ControlFlow<()>>,
    ) -> Result<Option<Self>, SideDataError> {
        Self::build_selected_inner(
            forest,
            select,
            || cancellation_callback.map_or(ControlFlow::Continue(()), |callback| callback()),
            true,
        )
    }

    fn build_selected_inner(
        forest: &Forest,
        select: impl Fn(ForestRegion<'_>) -> bool,
        mut cancellation_callback: impl FnMut() -> ControlFlow<()>,
        omit_empty: bool,
    ) -> Result<Option<Self>, SideDataError> {
        let mut selected = SmallVec::<[bool; 1]>::new();
        selected
            .try_reserve(forest.data().regions.len())
            .map_err(|_| Error::Allocation)?;
        for region in forest.regions() {
            if cancellation_callback().is_break() {
                return Err(Error::Canceled.into());
            }
            selected.push(select(region));
        }
        if omit_empty && !selected.iter().any(|&present| present) {
            return Ok(None);
        }
        let mut length = 0usize;
        for (region, &present) in forest.regions().zip(&selected) {
            length = length
                .checked_add(presence_length(
                    region.group_count(),
                    region.language().tables().kind_count + 2,
                    present,
                )?)
                .ok_or(Error::Overflow)?;
        }
        let storage = unsafe {
            Slab::initialize(length, |destination| {
                let mut offset = 0;
                for (region, present) in forest.regions().zip(selected) {
                    if cancellation_callback().is_break() {
                        return Err(Error::Canceled);
                    }
                    let groups = region.group_count();
                    let symbols = region.language().tables().kind_count + 2;
                    let record_length =
                        presence_length(groups, symbols, present).map_err(Error::from)?;
                    let record = destination.add(offset);
                    write_header(
                        record,
                        if present {
                            PRESENCE_FORMAT
                        } else {
                            ABSENT_PRESENCE_FORMAT
                        },
                        groups,
                        symbols,
                    )?;
                    if present {
                        let payload = record.add(HEADER_BYTES);
                        // Bitmap updates read the previous word before setting bits.
                        ptr::write_bytes(payload, 0, record_length - HEADER_BYTES);
                        let first_group = region.data().slots.start.raw() / GROUP_SIZE;
                        let words = (groups as usize).div_ceil(64);
                        for group in 0..groups {
                            if cancellation_callback().is_break() {
                                return Err(Error::Canceled);
                            }
                            for slot in (first_group + group) * GROUP_SIZE
                                ..forest.data().group_end(GroupIx(first_group + group)).raw()
                            {
                                let symbol = forest.data().symbol_index(SlotIx(slot)).ix();
                                let bitmap = payload
                                    .add((symbol * words + group as usize / 64) * 8)
                                    .cast::<u64>();
                                let value =
                                    u64::from_le(bitmap.read_unaligned()) | 1 << (group % 64);
                                bitmap.write_unaligned(value.to_le());
                            }
                        }
                    }
                    offset += record_length;
                }
                Ok(())
            })?
        };
        Ok(Some(Self(Sidecar(storage))))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.bytes()
    }

    pub(crate) fn copy(&self) -> Result<Self, SideDataError> {
        Ok(Self(Sidecar(Slab::copy(self.as_bytes())?)))
    }

    /// Checks record layout without a forest. Attachment checks dimensions;
    /// [`Self::validate_for`] also checks bitmap contents.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SideDataError> {
        presence_records(bytes)?;
        Ok(Self(Sidecar(Slab::copy(bytes)?)))
    }

    pub fn copy_from_bytes(forest: &Forest, bytes: &[u8]) -> Result<Self, SideDataError> {
        let cache = Self::from_bytes(bytes)?;
        cache.validate_loaded(forest)?;
        Ok(cache)
    }

    pub fn from_retained(owner: impl StableSlab) -> Result<Self, SideDataError> {
        let sidecar = Sidecar(Slab::retained(owner)?);
        presence_records(sidecar.bytes())?;
        Ok(Self(sidecar))
    }

    /// Checks layout and forest compatibility, then rebuilds selected bitmaps
    /// to verify their contents. This check is explicit in every build profile.
    pub fn validate_for(&self, forest: &Forest) -> Result<(), SideDataError> {
        let records = self.validate_loaded(forest)?;
        let expected = Self::build_selected(forest, |region| {
            records[region.index().ix()].1.format == PRESENCE_FORMAT
        })?;
        if expected.as_bytes() != self.as_bytes() {
            return Err(SideDataError::InvalidTarget);
        }
        Ok(())
    }

    fn validate_loaded(&self, forest: &Forest) -> Result<Vec<(usize, Header)>, SideDataError> {
        let records = presence_records(self.as_bytes())?;
        if records.len() != forest.data().regions.len() {
            return Err(SideDataError::InvalidTarget);
        }
        for (region, (_, header)) in forest.regions().zip(&records) {
            if header.groups != region.group_count()
                || header.dimension != region.language().tables().kind_count + 2
            {
                return Err(SideDataError::InvalidTarget);
            }
        }
        Ok(records)
    }
}

// Resolved once on attachment, with group indices relative to this region.
#[derive(Clone, Copy)]
pub(crate) struct PresenceView {
    payload: NonNull<u8>,
    first_group: GroupIx,
    groups: u32,
}
unsafe impl Send for PresenceView {}
unsafe impl Sync for PresenceView {}

impl PresenceView {
    #[inline]
    fn word(self, offset: usize) -> u64 {
        u64::from_le(unsafe {
            self.payload
                .as_ptr()
                .add(offset)
                .cast::<u64>()
                .read_unaligned()
        })
    }

    pub(crate) fn has(self, group: GroupIx, symbol: SquatterKindId) -> bool {
        let group = group - self.first_group;
        let words = (self.groups as usize).div_ceil(64);
        self.word((symbol.ix() * words + group as usize / 64) * 8) & (1 << (group % 64)) != 0
    }
    pub(crate) fn find_matching_group(
        &self,
        range: std::ops::Range<GroupIx>,
        symbol: SquatterKindId,
        reverse: bool,
    ) -> Option<GroupIx> {
        let words = (self.groups as usize).div_ceil(64);
        let mut range = range.start - self.first_group..range.end - self.first_group;
        while !range.is_empty() {
            let word_index = if reverse {
                range.start / 64
            } else {
                (range.end - 1) / 64
            };
            let start = range.start.saturating_sub(word_index * 64);
            let end = (range.end - word_index * 64).min(64);
            let word = self.word((symbol.ix() * words + word_index as usize) * 8);
            let bits = word & (u64::MAX << start) & (u64::MAX >> (64 - end));
            if bits != 0 {
                return Some(
                    self.first_group
                        + word_index * 64
                        + if reverse {
                            bits.trailing_zeros()
                        } else {
                            63 - bits.leading_zeros()
                        },
                );
            }
            if reverse {
                range.start = (word_index + 1) * 64;
            } else {
                range.end = word_index * 64;
            }
        }
        None
    }
}

const POINT_GROUP_BYTES: usize = 16 + GROUP_SIZE as usize * 4;

fn point_length(groups: u32) -> Result<usize, SideDataError> {
    (groups as usize)
        .checked_mul(POINT_GROUP_BYTES)
        .and_then(|bytes| bytes.checked_add(HEADER_BYTES))
        .ok_or(Error::Overflow.into())
}

fn validate_points(bytes: &[u8]) -> Result<Header, SideDataError> {
    let header = header(bytes)?;
    if header.format != POINT_FORMAT
        || bytes.len() != point_length(header.groups)?
        || (header.groups == 0) != (header.dimension == 0)
        || header.dimension > header.groups
    {
        return Err(SideDataError::InvalidTarget);
    }
    Ok(header)
}

/// Optional row/column coordinates created while parsing or packing with
/// [`crate::PackOptions::points`]. Persist separately from the tree slab.
/// Points affect grouping and cannot be computed for an existing tree.
///
/// **Not in Tree-sitter**
pub struct PointsData(Sidecar);
impl PointsData {
    pub(crate) fn empty(forest: &Forest) -> Result<Self, SideDataError> {
        let length = point_length(forest.group_count())?;
        let storage = unsafe {
            Slab::initialize(length, |destination| {
                write_header(
                    destination,
                    POINT_FORMAT,
                    forest.group_count(),
                    forest.data().regions.len() as u32,
                )?;
                ptr::write_bytes(destination.add(HEADER_BYTES), 0, length - HEADER_BYTES);
                Ok(())
            })?
        };
        Ok(Self(Sidecar(storage)))
    }

    pub(crate) fn grow(&mut self, groups: u32) -> Result<(), SideDataError> {
        self.0.0.grow(point_length(groups)?)?;
        let regions = u32::from_le_bytes(self.as_bytes()[12..16].try_into().unwrap());
        self.0.header(0, POINT_FORMAT, groups, regions)
    }

    pub(crate) fn put_bases(&mut self, group: GroupIx, start: PackedPoint, end: PackedPoint) {
        let offset = HEADER_BYTES + group.ix() * POINT_GROUP_BYTES;
        self.0.put_word(offset, start.raw());
        self.0.put_word(offset + 8, end.raw());
    }
    pub(crate) fn put_deltas(&mut self, slot: SlotIx, start: u16, end: u16) {
        let offset =
            HEADER_BYTES + slot.group().ix() * POINT_GROUP_BYTES + 16 + slot.in_group().ix() * 2;
        let bytes = self.0.bytes_mut();
        bytes[offset..offset + 2].copy_from_slice(&start.to_le_bytes());
        let offset = offset + GROUP_SIZE as usize * 2;
        bytes[offset..offset + 2].copy_from_slice(&end.to_le_bytes());
    }
    #[inline]
    pub(crate) fn column<const END: bool>(&self, group: GroupIx) -> (PackedPoint, &[u8]) {
        let offset = HEADER_BYTES + group.ix() * POINT_GROUP_BYTES;
        let base = PackedPoint(self.0.word(offset + usize::from(END) * 8));
        let offset = offset + 16 + usize::from(END) * GROUP_SIZE as usize * 2;
        (
            base,
            &self.0.bytes()[offset..offset + GROUP_SIZE as usize * 2],
        )
    }
    fn point<const END: bool>(&self, slot: SlotIx) -> PackedPoint {
        let (base, deltas) = self.column::<END>(slot.group());
        let offset = slot.in_group().ix() * 2;
        let delta = u64::from(deltas[offset + 1]) << 32 | u64::from(deltas[offset]);
        if END { base - delta } else { base + delta }
    }
    pub(crate) fn start(&self, slot: SlotIx) -> PackedPoint {
        self.point::<false>(slot)
    }
    pub(crate) fn end(&self, slot: SlotIx) -> PackedPoint {
        self.point::<true>(slot)
    }
    /// Borrows separately serializable side-data bytes.
    pub fn as_bytes(&self) -> &[u8] {
        self.0.bytes()
    }

    pub(crate) fn copy(&self) -> Result<Self, SideDataError> {
        Ok(Self(Sidecar(Slab::copy(self.as_bytes())?)))
    }

    pub fn copy_from_bytes(forest: &Forest, bytes: &[u8]) -> Result<Self, SideDataError> {
        let points = Self::from_bytes(bytes)?;
        points.validate_loaded(forest)?;
        Ok(points)
    }

    pub fn from_retained(owner: impl StableSlab) -> Result<Self, SideDataError> {
        let sidecar = Sidecar(Slab::retained(owner)?);
        validate_points(sidecar.bytes())?;
        Ok(Self(sidecar))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SideDataError> {
        validate_points(bytes)?;
        Ok(Self(Sidecar(Slab::copy(bytes)?)))
    }

    /// Checks layout, forest dimensions, coordinate arithmetic and ordering,
    /// and unused slots. Does not verify coordinates against source text.
    /// This check is explicit in every build profile.
    pub fn validate_for(&self, forest: &Forest) -> Result<(), SideDataError> {
        self.validate_loaded(forest)?;
        for group in 0..forest.group_count() {
            let end = forest.data().group_end(GroupIx(group));
            let used = (end.raw() - group * GROUP_SIZE) as usize;
            for (_, deltas) in [
                self.column::<false>(GroupIx(group)),
                self.column::<true>(GroupIx(group)),
            ] {
                if deltas[used * 2..].iter().any(|&byte| byte != 0) {
                    return Err(SideDataError::InvalidTarget);
                }
            }
            for slot in group * GROUP_SIZE..end.raw() {
                if self.start(SlotIx(slot)) > self.end(SlotIx(slot)) {
                    return Err(SideDataError::InvalidTarget);
                }
            }
        }
        Ok(())
    }

    fn validate_loaded(&self, tree: &Forest) -> Result<(), SideDataError> {
        let header = validate_points(self.as_bytes())?;
        if header.groups != tree.group_count()
            || header.dimension != tree.data().regions.len() as u32
        {
            return Err(SideDataError::InvalidTarget);
        }
        for group in 0..tree.group_count() {
            let used = (tree.data().group_end(GroupIx(group)).raw() - group * GROUP_SIZE) as usize;
            for (end, (base, deltas)) in [
                (false, self.column::<false>(GroupIx(group))),
                (true, self.column::<true>(GroupIx(group))),
            ] {
                let base = base.point();
                for delta in deltas[..used * 2].chunks_exact(2) {
                    let row = u32::from(delta[1]);
                    let column = u32::from(delta[0]);
                    let valid = if end {
                        base.row as u32 >= row && base.column as u32 >= column
                    } else {
                        base.row as u32 <= u32::MAX - row && base.column as u32 <= u32::MAX - column
                    };
                    if !valid {
                        return Err(SideDataError::InvalidTarget);
                    }
                }
            }
        }
        Ok(())
    }
}

impl Forest {
    /// Borrows the attached symbol-presence cache, if any.
    ///
    /// **Not in Tree-sitter**
    pub fn presence_cache(&self) -> Option<&PresenceCache> {
        self.data().presence_cache.as_ref()
    }
    /// Borrows the attached point data, if any.
    ///
    /// **Not in Tree-sitter**
    pub fn point_data(&self) -> Option<&PointsData> {
        self.data().point_data.as_ref()
    }
    /// Checks dimensions and attaches separately loaded symbol-presence data.
    /// Use [`PresenceCache::validate_for`] to check bitmap contents.
    /// Release borrowed tree views before replacing side data.
    ///
    /// **Not in Tree-sitter**
    pub fn set_presence_cache(&mut self, cache: PresenceCache) -> Result<(), SideDataError> {
        cache.validate_loaded(self)?;
        self.set_presence_cache_trusted(cache);
        Ok(())
    }

    /// Cache records must match this forest's regions and grammars.
    pub(crate) fn set_presence_cache_trusted(&mut self, cache: PresenceCache) {
        let mut offset = 0;
        for region in &mut self.data_mut().regions {
            let present = cache.0.word(offset) as u32 == PRESENCE_FORMAT;
            let groups = region.group_count();
            offset += HEADER_BYTES;
            region.presence = present.then(|| PresenceView {
                payload: NonNull::from(&cache.as_bytes()[offset..]).cast(),
                first_group: region.slots.start.group(),
                groups,
            });
            if present {
                offset += (groups as usize).div_ceil(64)
                    * (region.language.tables().kind_count as usize + 2)
                    * 8;
            }
        }
        self.data_mut().presence_cache = Some(cache);
    }

    /// Checks dimensions and coordinate arithmetic, then attaches point data.
    /// Use [`PointsData::validate_for`] to check ordering and unused slots.
    /// Release borrowed tree views before replacing side data.
    ///
    /// **Not in Tree-sitter**
    pub fn set_point_data(&mut self, points: PointsData) -> Result<(), SideDataError> {
        points.validate_loaded(self)?;
        self.set_point_data_trusted(points);
        Ok(())
    }

    /// Point dimensions and coordinate arithmetic must be valid for this forest.
    pub(crate) fn set_point_data_trusted(&mut self, points: PointsData) {
        self.data_mut().point_data = Some(points);
    }

    /// Drops the optional cache. Scan results stay the same; scan
    /// cost can change.
    ///
    /// **Not in Tree-sitter**
    pub fn drop_presence_cache(&mut self) {
        for region in &mut self.data_mut().regions {
            region.presence = None;
        }
        self.data_mut().presence_cache = None;
    }
    /// Drops point data. Point-dependent APIs then use row zero and
    /// byte offsets as columns.
    ///
    /// **Not in Tree-sitter**
    pub fn drop_point_data(&mut self) {
        self.data_mut().point_data = None;
    }
}
