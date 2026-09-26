use crate::{Error, Language, PackContext, PackOptions, Tree, native::NativeParser};
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

/// Reusable direct parser retaining its language and worker-local scratch.
///
/// Requires an ABI 15 language without external scanners or nonterminal extras.
/// Syntax errors are returned rather than recovered; no mainline tree is built.
/// Raw reductions are buffered for the whole parse before column encoding.
/// Output trees own their storage and remain valid across reuse or parser drop.
pub struct Parser {
    native: NativeParser,
    pack: PackContext,
}

impl Parser {
    /// Reuses the language's shared direct-parser tables, preparing them on first use.
    pub fn new(language: &Language) -> Result<Self, ParseError> {
        Ok(Self {
            native: NativeParser::new(language)?,
            pack: PackContext::new()?,
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
        let (nodes, root) = reductions.nodes();
        Ok(self
            .pack
            .pack_reductions(reductions.language(), nodes, root, options)?)
    }

    /// Release high-water scratch while retaining the prepared language.
    pub fn trim(&mut self) {
        self.native.trim();
        self.pack.trim();
    }
}

impl Tree {
    /// Parse directly into reverse preorder without constructing a mainline tree.
    /// Use [`Parser`] to reuse scratch across documents.
    pub fn parse_direct(language: &Language, source: impl AsRef<[u8]>) -> Result<Self, ParseError> {
        Self::parse_direct_with_options(language, source, PackOptions::default())
    }

    pub fn parse_direct_with_options(
        language: &Language,
        source: impl AsRef<[u8]>,
        options: PackOptions,
    ) -> Result<Self, ParseError> {
        Parser::new(language)?.parse_with_options(source, options)
    }
}
