use crate::{
    Error, Tree,
    storage::{GROUP_SIZE, StableSlab},
    types::PackedPoint,
};
use std::sync::atomic::{AtomicBool, Ordering};
use tree_sitter::Point;

const PRESENCE_MAGIC: u32 = 0x5052_0000;
const POINT_MAGIC: u32 = 0x5054_0000;
const HEADER_BYTES: usize = 16;

#[derive(Debug)]
pub enum SideDataError {
    Cancelled,
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

fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), SideDataError> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        Err(SideDataError::Cancelled)
    } else {
        Ok(())
    }
}

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
        magic: u32,
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
        bytes[0..4].copy_from_slice(&magic.to_le_bytes());
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
    fn validate(&self, tree: &Tree, magic: u32, length: usize) -> Result<(), SideDataError> {
        Self::validate_bytes(self.bytes(), tree, magic, length)
    }
    fn validate_bytes(
        bytes: &[u8],
        tree: &Tree,
        magic: u32,
        length: usize,
    ) -> Result<(), SideDataError> {
        if bytes.len() != length || bytes.len() < HEADER_BYTES {
            return Err(SideDataError::InvalidTarget);
        }
        let header = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if header(0) != magic
            || header(4) != tree.group_count()
            || header(8) != tree.slot_count()
            || header(12) != tree.data().tables().symbol_count + 2
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
        let mut words = Vec::new();
        words
            .try_reserve_exact(bytes.len() / 8)
            .map_err(|_| Error::Allocation)?;
        words.resize(bytes.len() / 8, 0);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr().cast(), bytes.len());
        }
        Ok(Self {
            storage: Storage::Owned(words),
        })
    }
}

fn presence_length(tree: &Tree) -> Result<usize, SideDataError> {
    let symbols = tree.data().tables().symbol_count as usize + 2;
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

pub struct PresenceCache(Sidecar);
impl PresenceCache {
    pub fn build(tree: &Tree, cancel: Option<&AtomicBool>) -> Result<Self, SideDataError> {
        let symbols = tree.data().tables().symbol_count + 2;
        let mut sidecar = Sidecar::new(
            PRESENCE_MAGIC,
            tree.group_count(),
            symbols,
            presence_length(tree)? - HEADER_BYTES,
        )?;
        let words = (tree.group_count() as usize).div_ceil(64);
        let data = tree.data();
        for group in 0..tree.group_count() {
            check_cancel(cancel)?;
            for slot in group * GROUP_SIZE..data.group_end(group) {
                let symbol = data.symbol_index(slot).get() as usize;
                let offset = HEADER_BYTES + (symbol * words + group as usize / 64) * 8;
                sidecar.put_word(offset, sidecar.word(offset) | 1 << (group % 64));
            }
        }
        Ok(Self(sidecar))
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.0.bytes()
    }
    pub fn from_backing(tree: &Tree, backing: impl StableSlab) -> Result<Self, SideDataError> {
        let result = Self(Sidecar::from_backing(backing)?);
        result.validate_loaded(tree)?;
        Ok(result)
    }
    pub fn copy_from_bytes(tree: &Tree, bytes: &[u8]) -> Result<Self, SideDataError> {
        Sidecar::validate_bytes(bytes, tree, PRESENCE_MAGIC, presence_length(tree)?)?;
        let result = Self(Sidecar::copy_from_bytes(bytes)?);
        result.validate_loaded(tree)?;
        Ok(result)
    }
    fn validate_loaded(&self, tree: &Tree) -> Result<(), SideDataError> {
        self.0
            .validate(tree, PRESENCE_MAGIC, presence_length(tree)?)?;
        #[cfg(debug_assertions)]
        {
            let expected = Self::build(tree, None)?;
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
    pub(crate) fn next_group(
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

fn point_length(tree: &Tree) -> Result<usize, SideDataError> {
    HEADER_BYTES
        .checked_add(
            (tree.slot_count() as usize)
                .checked_mul(16)
                .ok_or(Error::Overflow)?,
        )
        .ok_or(Error::Overflow.into())
}

pub struct PointData(Sidecar);
impl PointData {
    pub fn build(
        tree: &Tree,
        source: &SourcePoints<'_>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, SideDataError> {
        let mut result = Self::empty(tree)?;
        for group in 0..tree.group_count() {
            check_cancel(cancel)?;
            for slot in group * GROUP_SIZE..tree.data().group_end(group) {
                let node = tree.node_at_slot(crate::SlotIx(slot)).unwrap();
                result.put(
                    slot,
                    source.point(node.start_byte())?,
                    source.point(node.end_byte())?,
                )?;
            }
        }
        Ok(result)
    }
    pub(crate) fn empty(tree: &Tree) -> Result<Self, SideDataError> {
        Ok(Self(Sidecar::new(
            POINT_MAGIC,
            tree.group_count(),
            tree.data().tables().symbol_count + 2,
            point_length(tree)? - HEADER_BYTES,
        )?))
    }
    pub(crate) fn put(&mut self, slot: u32, start: Point, end: Point) -> Result<(), SideDataError> {
        let start = PackedPoint::from_point(start).ok_or(Error::Overflow)?;
        let end = PackedPoint::from_point(end).ok_or(Error::Overflow)?;
        let offset = HEADER_BYTES + slot as usize * 16;
        self.0.put_word(offset, start.get());
        self.0.put_word(offset + 8, end.get());
        Ok(())
    }
    pub(crate) fn start(&self, slot: u32) -> PackedPoint {
        PackedPoint(self.0.word(HEADER_BYTES + slot as usize * 16))
    }
    pub(crate) fn end(&self, slot: u32) -> PackedPoint {
        PackedPoint(self.0.word(HEADER_BYTES + slot as usize * 16 + 8))
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.0.bytes()
    }
    pub fn from_backing(tree: &Tree, backing: impl StableSlab) -> Result<Self, SideDataError> {
        let result = Self(Sidecar::from_backing(backing)?);
        result.validate_loaded(tree)?;
        Ok(result)
    }
    pub fn copy_from_bytes(tree: &Tree, bytes: &[u8]) -> Result<Self, SideDataError> {
        Sidecar::validate_bytes(bytes, tree, POINT_MAGIC, point_length(tree)?)?;
        let result = Self(Sidecar::copy_from_bytes(bytes)?);
        result.validate_loaded(tree)?;
        Ok(result)
    }
    fn validate_loaded(&self, tree: &Tree) -> Result<(), SideDataError> {
        self.0.validate(tree, POINT_MAGIC, point_length(tree)?)?;
        #[cfg(debug_assertions)]
        for group in 0..tree.group_count() {
            for slot in group * GROUP_SIZE..tree.data().group_end(group) {
                if self.start(slot) > self.end(slot) {
                    return Err(SideDataError::InvalidTarget);
                }
            }
            for slot in tree.data().group_end(group)..(group + 1) * GROUP_SIZE {
                if self.start(slot) != PackedPoint(0) || self.end(slot) != PackedPoint(0) {
                    return Err(SideDataError::InvalidTarget);
                }
            }
        }
        Ok(())
    }
}

impl Tree {
    pub fn presence_cache(&self) -> Option<&PresenceCache> {
        self.data().presence_cache.as_ref()
    }
    pub fn point_data(&self) -> Option<&PointData> {
        self.data().point_data.as_ref()
    }
    pub fn set_presence_cache(&mut self, cache: PresenceCache) -> Result<(), SideDataError> {
        cache.validate_loaded(self)?;
        self.data_mut().presence_cache = Some(cache);
        Ok(())
    }
    pub fn set_point_data(&mut self, points: PointData) -> Result<(), SideDataError> {
        points.validate_loaded(self)?;
        self.data_mut().point_data = Some(points);
        Ok(())
    }
    pub fn drop_presence_cache(&mut self) {
        self.data_mut().presence_cache = None;
    }
    pub fn drop_point_data(&mut self) {
        self.data_mut().point_data = None;
    }
}

pub struct SourcePoints<'bytes> {
    bytes: &'bytes [u8],
    line_starts: Vec<usize>,
}
impl<'bytes> SourcePoints<'bytes> {
    pub fn new(bytes: &'bytes [u8]) -> Result<Self, Error> {
        let mut line_starts = Vec::new();
        line_starts.try_reserve(1).map_err(|_| Error::Allocation)?;
        line_starts.push(0);
        for (index, &byte) in bytes.iter().enumerate() {
            if byte == b'\n' {
                line_starts.try_reserve(1).map_err(|_| Error::Allocation)?;
                line_starts.push(index + 1);
            }
        }
        Ok(Self { bytes, line_starts })
    }
    pub fn bytes(&self) -> &'bytes [u8] {
        self.bytes
    }
    pub fn point(&self, byte: usize) -> Result<Point, Error> {
        if byte > self.bytes.len() {
            return Err(Error::InvalidArgument);
        }
        let row = self.line_starts.partition_point(|&start| start <= byte) - 1;
        Ok(Point::new(row, byte - self.line_starts[row]))
    }
}
