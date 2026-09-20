mod node;
use node::RawNode;
pub use node::{Children, Cursor, Node};
pub mod scan;
pub use scan::{Postorder, Preorder, Scan};
mod native;
mod packing;
mod parser;
mod storage;
pub mod traits;
pub use packing::{PackContext, PackOptions};
pub use parser::{ParseError, Parser};
pub use storage::{BackedTree, BorrowedTree, StableSlab, Tree, representation_id};
pub mod query;
mod query_exec;
mod query_plan;
pub use native::Grammar;
pub use query::{
    Query, QueryCapture, QueryCursor, QueryError, QueryExecution, QueryExecutionError, QueryMatch,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum Error {
    InvalidArgument = 1,
    Allocation = 2,
    Overflow = 3,
    DictionaryFull = 4,
    InvalidSlab = 5,
    Language = 6,
    Parse = 7,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidArgument => "invalid argument",
            Self::Allocation => "allocation failed",
            Self::Overflow => "grammar IDs or slab size exceed representation limits",
            Self::DictionaryFull => "more than 65536 supertype masks",
            Self::InvalidSlab => "invalid or incompatible slab",
            Self::Language => "unsupported language",
            Self::Parse => "parse failed",
        })
    }
}

impl std::error::Error for Error {}
impl Error {
    pub(crate) fn from_code(code: i32) -> Error {
        match code {
            2 => Error::Allocation,
            3 => Error::Overflow,
            4 => Error::DictionaryFull,
            5 => Error::InvalidSlab,
            6 => Error::Language,
            7 => Error::Parse,
            _ => Error::InvalidArgument,
        }
    }
}

/// A reusable runtime-sized set of IDs, interpreted by the selected scan filter.
#[derive(Clone, Debug, Default)]
pub struct IdSet {
    ids: Vec<u16>,
    words: Vec<u64>,
}
impl IdSet {
    pub fn new(ids: impl IntoIterator<Item = u16>) -> Self {
        ids.into_iter().collect()
    }
    pub fn contains(&self, id: u16) -> bool {
        self.words
            .get(id as usize / 64)
            .is_some_and(|word| word & (1u64 << (id % 64)) != 0)
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}
impl FromIterator<u16> for IdSet {
    fn from_iter<I: IntoIterator<Item = u16>>(ids: I) -> Self {
        let mut ids: Vec<_> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        let mut words = vec![0; ids.last().map_or(0, |&kind| kind as usize / 64 + 1)];
        for &id in &ids {
            words[id as usize / 64] |= 1u64 << (id % 64);
        }
        Self { ids, words }
    }
}

/// A reusable set of public kind IDs, interpreted in the scanned tree's language.
pub type KindSet = IdSet;

/// Preorder traversal filtered by public kind IDs.
pub type KindMatches<'tree, 'kinds> =
    scan::Nodes<'tree, scan::Filtered<Preorder<'tree>, scan::KindIds<'kinds>>>;
