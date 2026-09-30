use std::{error, fmt};

mod types;
pub(crate) use types::SlotIx;
pub use types::{
    CaptureIx, ChildIx, FieldId, GrammarId, KindId, MatchCaptureIx, MatchId, NamedChildIx, NodeId,
    PatternIx, RegionIx, RepresentationId, SquatterGrammarId, SquatterKindId, TreeIx,
};
mod node;
use node::RawNode;
pub use node::{Node, TreeCursor};
pub mod scan;
pub use scan::{Postorder, Preorder, Scan};
mod native;
mod packing;
mod parser;
mod side_data;
mod simd;
mod storage;
pub mod traits;
pub use packing::{PackOptions, PackRegion, Packer};
pub use parser::{
    PackedParseOptions, ParseError, ParseOptions, ParseState, Parser, ParserError, TreeFellerParser,
};
pub use side_data::{PointsData, PresenceCache, SideDataError};
pub use storage::{BorrowedForest, Forest, ForestRegion, StableSlab, Tree, representation_id};
pub mod query;
mod query_exec;
mod query_plan;
pub use native::{Language, LanguageHash, language_hash};
pub use query::{
    CaptureQuantifier, Query, QueryCapture, QueryCaptures, QueryCursor, QueryCursorOptions,
    QueryCursorState, QueryError, QueryErrorKind, QueryExecution, QueryExecutionError, QueryMatch,
    QueryMatches, QueryPredicate, QueryPredicateArg, QueryProperty, QueryScope, StreamingIterator,
    TextProvider,
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
    Canceled = 8,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidArgument => "invalid argument",
            Self::Allocation => "allocation failed",
            Self::Overflow => "grammar IDs or slab size exceed representation limits",
            Self::DictionaryFull => "more than 65536 supertype masks",
            Self::InvalidSlab => "invalid or incompatible slab",
            Self::Language => "unsupported language",
            Self::Parse => "parse failed",
            Self::Canceled => "parse canceled",
        })
    }
}

impl error::Error for Error {}
impl Error {
    pub(crate) fn from_code(code: i32) -> Error {
        match code {
            2 => Error::Allocation,
            3 => Error::Overflow,
            4 => Error::DictionaryFull,
            5 => Error::InvalidSlab,
            6 => Error::Language,
            7 => Error::Parse,
            8 => Error::Canceled,
            _ => Error::InvalidArgument,
        }
    }
}

/// IDs supported by reusable scan sets.
///
/// **Not in Tree-sitter**. An identifier domain usable by reusable scan sets.
pub trait Id: Copy + Ord + private::Id {
    #[doc(hidden)]
    fn raw(self) -> u16;
}
mod private {
    pub trait Id {}
}
impl private::Id for SquatterKindId {}
impl Id for SquatterKindId {
    #[inline]
    fn raw(self) -> u16 {
        self.raw()
    }
}
impl private::Id for KindId {}
impl Id for KindId {
    #[inline]
    fn raw(self) -> u16 {
        self.raw()
    }
}
impl private::Id for Option<FieldId> {}
impl Id for Option<FieldId> {
    #[inline]
    fn raw(self) -> u16 {
        self.map_or(0, FieldId::raw)
    }
}

/// A reusable set of IDs from one domain and grammar.
///
/// **Not in Tree-sitter**. Stores selected IDs from one grammar and domain for repeated
/// scans.
#[derive(Clone, Debug)]
pub struct IdSet<I: Id> {
    ids: Vec<I>,
    words: Vec<u64>,
}
impl<I: Id> Default for IdSet<I> {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            words: Vec::new(),
        }
    }
}
impl<I: Id> IdSet<I> {
    pub fn new(ids: impl IntoIterator<Item = I>) -> Self {
        ids.into_iter().collect()
    }

    pub fn intersection(&self, other: &Self) -> Self {
        let (smaller, larger) = if self.ids.len() <= other.ids.len() {
            (self, other)
        } else {
            (other, self)
        };
        Self::new(
            smaller
                .ids
                .iter()
                .copied()
                .filter(|&id| larger.contains(id)),
        )
    }

    pub fn contains(&self, id: I) -> bool {
        let id = id.raw();
        self.words
            .get(id as usize / 64)
            .is_some_and(|word| word & (1u64 << (id % 64)) != 0)
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}
impl<I: Id> FromIterator<I> for IdSet<I> {
    fn from_iter<T: IntoIterator<Item = I>>(ids: T) -> Self {
        let mut ids: Vec<_> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        let mut words = vec![0; ids.last().map_or(0, |id| id.raw() as usize / 64 + 1)];
        for &id in &ids {
            let id = id.raw();
            words[id as usize / 64] |= 1u64 << (id % 64);
        }
        Self { ids, words }
    }
}

/// Public kind IDs, interpreted in the scanned tree's grammar.
pub type KindSet = IdSet<KindId>;
/// Field selections; `None` selects nodes with no field.
pub type FieldSet = IdSet<Option<FieldId>>;

/// Preorder traversal filtered by public kind IDs.
pub type KindMatches<'tree, 'kinds> =
    scan::Nodes<'tree, scan::Filtered<Preorder<'tree>, scan::KindIds<'kinds>>>;
