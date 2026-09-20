use crate::{
    Error, Grammar, PackOptions, Tree,
    native::{NativeParser, Traversal},
    packing::{Scratch, encode},
};
use tree_sitter::Point;

/// Direct-parser failure, including its owned diagnostic and source position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    pub code: Error,
    pub byte: u32,
    pub point: Point,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} at byte {}: {}",
            self.code, self.byte, self.message
        )
    }
}

impl std::error::Error for ParseError {}

impl From<Error> for ParseError {
    fn from(code: Error) -> Self {
        Self {
            code,
            byte: 0,
            point: Point::default(),
            message: code.to_string(),
        }
    }
}

/// Reusable direct parser retaining its grammar and worker-local scratch.
///
/// Requires an ABI 15 grammar without external scanners or nonterminal extras.
/// Syntax errors are returned rather than recovered; no mainline tree is built.
/// Raw reductions are buffered for the whole parse before column encoding.
/// Output trees own their storage and remain valid across reuse or parser drop.
pub struct Parser {
    native: NativeParser,
    traversal: Traversal,
    scratch: Scratch,
}

impl Parser {
    /// Reuses the grammar's shared direct-parser tables, preparing them on first use.
    pub fn new(grammar: &Grammar) -> Result<Self, ParseError> {
        Ok(Self {
            native: NativeParser::new(grammar)?,
            traversal: Traversal::new()?,
            scratch: Scratch::default(),
        })
    }

    pub fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Tree, ParseError> {
        self.parse_with_options(source, PackOptions::default())
    }

    /// Parse a fresh document. A failed parse does not prevent subsequent reuse.
    pub fn parse_with_options(
        &mut self,
        source: impl AsRef<[u8]>,
        options: PackOptions,
    ) -> Result<Tree, ParseError> {
        let reductions = self.native.parse(source.as_ref())?;
        let mut events = reductions.events(&mut self.traversal, options.points)?;
        Ok(encode(
            reductions.grammar(),
            &mut events,
            &mut self.scratch,
            options,
        )?)
    }

    /// Release high-water scratch while retaining the prepared grammar.
    pub fn trim(&mut self) {
        self.native.trim();
        self.traversal.trim();
        self.scratch.trim();
    }
}

impl Tree {
    /// Parse directly into reverse preorder without constructing a mainline tree.
    /// Use [`Parser`] to reuse scratch across documents.
    pub fn parse_direct(grammar: &Grammar, source: impl AsRef<[u8]>) -> Result<Self, ParseError> {
        Self::parse_direct_with_options(grammar, source, PackOptions::default())
    }

    pub fn parse_direct_with_options(
        grammar: &Grammar,
        source: impl AsRef<[u8]>,
        options: PackOptions,
    ) -> Result<Self, ParseError> {
        Parser::new(grammar)?.parse_with_options(source, options)
    }
}
