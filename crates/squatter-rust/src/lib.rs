mod native;
mod packing;
mod parser;
mod storage;
pub use packing::{PackContext, PackOptions};
pub use parser::{ParseError, Parser};
pub use storage::{BackedTree, BorrowedTree, StableSlab, Tree, representation_id};
pub mod query;
pub use native::Grammar;
pub use query::{Query, QueryError, QueryExecutionError};

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
