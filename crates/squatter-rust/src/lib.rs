mod native;
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
            Self::Allocation => "allocation failure",
            Self::Overflow => "overflow",
            Self::DictionaryFull => "supertype dictionary full",
            Self::InvalidSlab => "invalid slab",
            Self::Language => "incompatible language",
            Self::Parse => "parse error",
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
