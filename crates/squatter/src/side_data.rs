use crate::{
    Error, Tree,
    storage::{GROUP_SIZE, StableSlab, slab_format},
    types::PackedPoint,
};

const PRESENCE_FORMAT: u32 = slab_format(0xfe, 0);
const POINT_FORMAT: u32 = slab_format(0xfd, 0);
const HEADER_BYTES: usize = 16;

/// Failure to build, load, or attach separate tree data.
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

enum Storage {
    Owned(Vec<u64>),
    Backed(Box<dyn StableSlab>),
}

impl Storage {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Owned(words) => unsafe {
                std::slice::from_raw_parts(words.as_ptr().cast(), words.len() * 8)
            },
            Self::Backed(owner) => owner.bytes(),
        }
    }
}

struct Sidecar {
    storage: Storage,
}

impl Sidecar {
    fn new(
        format: u32,
        groups: u32,
        symbols: u32,
        payload_bytes: usize,
    ) -> Result<Self, SideDataError> {
        let length = HEADER_BYTES
            .checked_add(payload_bytes)
            .ok_or(Error::Overflow)?;
        let words = length.checked_add(7).ok_or(Error::Overflow)? / 8;
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(words)
            .map_err(|_| Error::Allocation)?;
        buffer.resize(words, 0);
        let mut result = Self {
            storage: Storage::Owned(buffer),
        };
        let bytes = result.bytes_mut();
        bytes[0..4].copy_from_slice(&format.to_le_bytes());
        bytes[4..8].copy_from_slice(&groups.to_le_bytes());
        bytes[8..12].copy_from_slice(
            &(groups.checked_mul(GROUP_SIZE).ok_or(Error::Overflow)?).to_le_bytes(),
        );
        bytes[12..16].copy_from_slice(&symbols.to_le_bytes());
        Ok(result)
    }

    fn bytes(&self) -> &[u8] {
        self.storage.bytes()
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        match &mut self.storage {
            Storage::Owned(words) => unsafe {
                std::slice::from_raw_parts_mut(words.as_mut_ptr().cast(), words.len() * 8)
            },
            Storage::Backed(_) => unreachable!(),
        }
    }
    fn word(&self, byte: usize) -> u64 {
        u64::from_le_bytes(self.bytes()[byte..byte + 8].try_into().unwrap())
    }
    fn put_word(&mut self, byte: usize, value: u64) {
        self.bytes_mut()[byte..byte + 8].copy_from_slice(&value.to_le_bytes());
    }
    fn validate(&self, tree: &Tree, format: u32, length: usize) -> Result<(), SideDataError> {
        Self::validate_bytes(self.bytes(), tree, format, length)
    }
    fn validate_bytes(
        bytes: &[u8],
        tree: &Tree,
        format: u32,
        length: usize,
    ) -> Result<(), SideDataError> {
        if bytes.len() != length || bytes.len() < HEADER_BYTES {
            return Err(SideDataError::InvalidTarget);
        }
        let header = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if header(0) != format
            || header(4) != tree.group_count()
            || header(8) != tree.slot_count()
            || header(12) != tree.data().tables().kind_count + 2
        {
            return Err(SideDataError::InvalidTarget);
        }
        Ok(())
    }
    fn from_backing(owner: impl StableSlab) -> Result<Self, SideDataError> {
        if owner.bytes().as_ptr() as usize % 8 != 0 {
            return Err(SideDataError::InvalidTarget);
        }
        Ok(Self {
            storage: Storage::Backed(Box::new(owner)),
        })
    }
    fn copy_from_bytes(bytes: &[u8]) -> Result<Self, SideDataError> {
        if bytes.len() % 8 != 0 {
            return Err(SideDataError::InvalidTarget);
        }
        let mut words = Vec::<u64>::new();
        words
            .try_reserve_exact(bytes.len() / 8)
            .map_err(|_| Error::Allocation)?;
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr().cast(), bytes.len());
            words.set_len(bytes.len() / 8);
        }
        Ok(Self {
            storage: Storage::Owned(words),
        })
    }
}

fn presence_length(tree: &Tree) -> Result<usize, SideDataError> {
    let symbols = tree.data().tables().kind_count as usize + 2;
    let words = (tree.group_count() as usize).div_ceil(64);
    HEADER_BYTES
        .checked_add(
            symbols
                .checked_mul(words)
                .and_then(|count| count.checked_mul(8))
                .ok_or(Error::Overflow)?,
        )
        .ok_or(Error::Overflow.into())
}

/// Optional per-group symbol membership data. Persist separately
/// from the tree slab; removing it affects cost, not scan results.
///
/// **Not in Tree-sitter**
pub struct PresenceCache(Sidecar);
impl PresenceCache {
    /// Builds symbol membership from the tree.
    pub fn build(tree: &Tree) -> Result<Self, SideDataError> {
        Self::build_with_progress(tree, &mut Default::default())
    }

    pub(crate) fn build_with_progress(
        tree: &Tree,
        progress: &mut crate::packing::Progress<'_>,
    ) -> Result<Self, SideDataError> {
        progress.poll()?;
        let symbols = tree.data().tables().kind_count + 2;
        let mut sidecar = Sidecar::new(
            PRESENCE_FORMAT,
            tree.group_count(),
            symbols,
            presence_length(tree)? - HEADER_BYTES,
        )?;
        let words = (tree.group_count() as usize).div_ceil(64);
        let data = tree.data();
        for group in 0..tree.group_count() {
            progress.tick()?;
            for slot in group * GROUP_SIZE..data.group_end(group) {
                let symbol = data.symbol_index(slot).get() as usize;
                let offset = HEADER_BYTES + (symbol * words + group as usize / 64) * 8;
                sidecar.put_word(offset, sidecar.word(offset) | 1 << (group % 64));
            }
        }
        Ok(Self(sidecar))
    }
    /// Borrows separately serializable side-data bytes.
    pub fn as_bytes(&self) -> &[u8] {
        self.0.bytes()
    }
    /// Validates and retains stable backing bytes for this tree.
    /// Use data persisted for this exact snapshot; structural checks do not establish
    /// source identity.
    pub fn from_backing(tree: &Tree, backing: impl StableSlab) -> Result<Self, SideDataError> {
        let result = Self(Sidecar::from_backing(backing)?);
        result.validate_loaded(tree)?;
        Ok(result)
    }
    /// Validates and copies side-data bytes for this tree. Use data
    /// persisted for this exact snapshot; structural checks do not establish source
    /// identity.
    pub fn copy_from_bytes(tree: &Tree, bytes: &[u8]) -> Result<Self, SideDataError> {
        Sidecar::validate_bytes(bytes, tree, PRESENCE_FORMAT, presence_length(tree)?)?;
        let result = Self(Sidecar::copy_from_bytes(bytes)?);
        result.validate_loaded(tree)?;
        Ok(result)
    }
    fn validate_loaded(&self, tree: &Tree) -> Result<(), SideDataError> {
        self.0
            .validate(tree, PRESENCE_FORMAT, presence_length(tree)?)?;
        #[cfg(debug_assertions)]
        {
            let expected = Self::build(tree)?;
            if self.as_bytes() != expected.as_bytes() {
                return Err(SideDataError::InvalidTarget);
            }
        }
        Ok(())
    }
    pub(crate) fn has(&self, group: u32, symbol: usize, groups: u32) -> bool {
        let words = (groups as usize).div_ceil(64);
        self.0
            .word(HEADER_BYTES + (symbol * words + group as usize / 64) * 8)
            & (1 << (group % 64))
            != 0
    }
    pub(crate) fn find_matching_group(
        &self,
        mut range: std::ops::Range<u32>,
        symbol: usize,
        groups: u32,
        reverse: bool,
    ) -> Option<u32> {
        let words = (groups as usize).div_ceil(64);
        while !range.is_empty() {
            let word_index = if reverse {
                range.start / 64
            } else {
                (range.end - 1) / 64
            };
            let start = range.start.saturating_sub(word_index * 64);
            let end = (range.end - word_index * 64).min(64);
            let word = self
                .0
                .word(HEADER_BYTES + (symbol * words + word_index as usize) * 8);
            let bits = word & (u64::MAX << start) & (u64::MAX >> (64 - end));
            if bits != 0 {
                return Some(
                    word_index * 64
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

fn point_length(tree: &Tree) -> Result<usize, SideDataError> {
    (tree.group_count() as usize)
        .checked_mul(POINT_GROUP_BYTES)
        .and_then(|length| length.checked_add(HEADER_BYTES))
        .ok_or(Error::Overflow.into())
}

/// Optional row/column coordinates created while parsing or packing with
/// [`crate::PackOptions::points`]. Persist separately from the tree slab.
/// Points affect grouping and cannot be computed for an existing tree.
///
/// **Not in Tree-sitter**
pub struct PointsData(Sidecar);
impl PointsData {
    pub(crate) fn empty(tree: &Tree) -> Result<Self, SideDataError> {
        Ok(Self(Sidecar::new(
            POINT_FORMAT,
            tree.group_count(),
            tree.data().tables().kind_count + 2,
            point_length(tree)? - HEADER_BYTES,
        )?))
    }
    pub(crate) fn grow(&mut self, groups: u32) -> Result<(), SideDataError> {
        let slots = groups.checked_mul(GROUP_SIZE).ok_or(Error::Overflow)?;
        let length = (groups as usize)
            .checked_mul(POINT_GROUP_BYTES / 8)
            .and_then(|words| words.checked_add(HEADER_BYTES / 8))
            .ok_or(Error::Overflow)?;
        let Storage::Owned(words) = &mut self.0.storage else {
            unreachable!();
        };
        debug_assert!(length >= words.len());
        words
            .try_reserve(length - words.len())
            .map_err(|_| Error::Allocation)?;
        words.resize(length, 0);
        let bytes = self.0.bytes_mut();
        bytes[4..8].copy_from_slice(&groups.to_le_bytes());
        bytes[8..12].copy_from_slice(&slots.to_le_bytes());
        Ok(())
    }

    pub(crate) fn put_bases(&mut self, group: u32, start: PackedPoint, end: PackedPoint) {
        let offset = HEADER_BYTES + group as usize * POINT_GROUP_BYTES;
        self.0.put_word(offset, start.get());
        self.0.put_word(offset + 8, end.get());
    }
    pub(crate) fn put_deltas(&mut self, slot: u32, start: u16, end: u16) {
        let offset = HEADER_BYTES
            + (slot / GROUP_SIZE) as usize * POINT_GROUP_BYTES
            + 16
            + (slot % GROUP_SIZE) as usize * 2;
        let bytes = self.0.bytes_mut();
        bytes[offset..offset + 2].copy_from_slice(&start.to_le_bytes());
        let offset = offset + GROUP_SIZE as usize * 2;
        bytes[offset..offset + 2].copy_from_slice(&end.to_le_bytes());
    }
    #[inline]
    pub(crate) fn column<const END: bool>(&self, group: u32) -> (PackedPoint, &[u8]) {
        let offset = HEADER_BYTES + group as usize * POINT_GROUP_BYTES;
        let base = PackedPoint(self.0.word(offset + usize::from(END) * 8));
        let offset = offset + 16 + usize::from(END) * GROUP_SIZE as usize * 2;
        (
            base,
            &self.0.bytes()[offset..offset + GROUP_SIZE as usize * 2],
        )
    }
    fn point<const END: bool>(&self, slot: u32) -> PackedPoint {
        let (base, deltas) = self.column::<END>(slot / GROUP_SIZE);
        let offset = (slot % GROUP_SIZE) as usize * 2;
        let delta = u64::from(deltas[offset + 1]) << 32 | u64::from(deltas[offset]);
        if END { base - delta } else { base + delta }
    }
    pub(crate) fn start(&self, slot: u32) -> PackedPoint {
        self.point::<false>(slot)
    }
    pub(crate) fn end(&self, slot: u32) -> PackedPoint {
        self.point::<true>(slot)
    }
    /// Borrows separately serializable side-data bytes.
    pub fn as_bytes(&self) -> &[u8] {
        self.0.bytes()
    }
    /// Validates and retains stable backing bytes for this tree.
    /// Use data persisted for this exact snapshot; structural checks do not establish
    /// source identity.
    pub fn from_backing(tree: &Tree, backing: impl StableSlab) -> Result<Self, SideDataError> {
        let result = Self(Sidecar::from_backing(backing)?);
        result.validate_loaded(tree)?;
        Ok(result)
    }
    /// Validates and copies side-data bytes for this tree. Use data
    /// persisted for this exact snapshot; structural checks do not establish source
    /// identity.
    pub fn copy_from_bytes(tree: &Tree, bytes: &[u8]) -> Result<Self, SideDataError> {
        Sidecar::validate_bytes(bytes, tree, POINT_FORMAT, point_length(tree)?)?;
        let result = Self(Sidecar::copy_from_bytes(bytes)?);
        result.validate_loaded(tree)?;
        Ok(result)
    }
    fn validate_loaded(&self, tree: &Tree) -> Result<(), SideDataError> {
        self.0.validate(tree, POINT_FORMAT, point_length(tree)?)?;
        self.validate_points(tree, &mut Default::default())
    }

    fn validate_points(
        &self,
        tree: &Tree,
        _progress: &mut crate::packing::Progress<'_>,
    ) -> Result<(), SideDataError> {
        for group in 0..tree.group_count() {
            _progress.tick()?;
            let used = (tree.data().group_end(group) - group * GROUP_SIZE) as usize;
            for (end, (base, deltas)) in [
                (false, self.column::<false>(group)),
                (true, self.column::<true>(group)),
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
                #[cfg(debug_assertions)]
                if deltas[used * 2..].iter().any(|&byte| byte != 0) {
                    return Err(SideDataError::InvalidTarget);
                }
            }
            #[cfg(debug_assertions)]
            for slot in group * GROUP_SIZE..tree.data().group_end(group) {
                if self.start(slot) > self.end(slot) {
                    return Err(SideDataError::InvalidTarget);
                }
            }
        }
        Ok(())
    }
}

impl Tree {
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
    /// Validates and attaches separately loaded symbol-presence
    /// data. Release borrowed tree views before replacing side data.
    ///
    /// **Not in Tree-sitter**
    pub fn set_presence_cache(&mut self, cache: PresenceCache) -> Result<(), SideDataError> {
        cache.validate_loaded(self)?;
        self.data_mut().presence_cache = Some(cache);
        Ok(())
    }
    /// Validates and attaches separately loaded point data. Release
    /// borrowed tree views before replacing side data.
    ///
    /// **Not in Tree-sitter**
    pub fn set_point_data(&mut self, points: PointsData) -> Result<(), SideDataError> {
        self.set_point_data_with_progress(points, &mut Default::default())
    }

    pub(crate) fn set_point_data_with_progress(
        &mut self,
        points: PointsData,
        progress: &mut crate::packing::Progress<'_>,
    ) -> Result<(), SideDataError> {
        progress.poll()?;
        points.0.validate(self, POINT_FORMAT, point_length(self)?)?;
        points.validate_points(self, progress)?;
        self.data_mut().point_data = Some(points);
        Ok(())
    }
    /// Drops the optional cache. Scan results stay the same; scan
    /// cost can change.
    ///
    /// **Not in Tree-sitter**
    pub fn drop_presence_cache(&mut self) {
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
