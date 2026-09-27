use std::ops::ControlFlow;
use tree_sitter::Point;

use crate::{
    Error, Language, PackContext, PackOptions, Tree,
    native::NativeParser,
    packing::Progress,
    traits::{Parse, ParseStateLike, ParseWithCallback},
};

/// Controls parsing for native and packed output.
#[derive(Default)]
pub struct ParseOptions<'a> {
    pub progress_callback: Option<&'a mut dyn FnMut(&dyn ParseStateLike) -> ControlFlow<()>>,
}

impl<'a> ParseOptions<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn progress_callback<F: FnMut(&dyn ParseStateLike) -> ControlFlow<()>>(
        mut self,
        callback: &'a mut F,
    ) -> Self {
        self.progress_callback = Some(callback);
        self
    }

    pub fn reborrow(&mut self) -> ParseOptions<'_> {
        ParseOptions {
            progress_callback: match &mut self.progress_callback {
                Some(callback) => Some(*callback),
                None => None,
            },
        }
    }
}

/// Parser controls and packed-tree storage options.
#[derive(Default)]
pub struct PackedParseOptions<'a> {
    pub parse: ParseOptions<'a>,
    pub pack: PackOptions,
}

impl PackedParseOptions<'_> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reborrow(&mut self) -> PackedParseOptions<'_> {
        PackedParseOptions {
            parse: self.parse.reborrow(),
            pack: self.pack,
        }
    }
}

impl<'a> From<ParseOptions<'a>> for PackedParseOptions<'a> {
    fn from(parse: ParseOptions<'a>) -> Self {
        Self {
            parse,
            pack: PackOptions::default(),
        }
    }
}

/// Progress through Tree-sitter parsing or packing.
/// Offsets describe the current or last input node during conversion, not work completed.
pub struct ParseState {
    byte: usize,
    has_error: bool,
    converting: bool,
}

impl ParseState {
    pub fn current_byte_offset(&self) -> usize {
        self.byte
    }

    pub fn has_error(&self) -> bool {
        self.has_error
    }

    pub fn is_converting(&self) -> bool {
        self.converting
    }

    pub fn current_byte_offset_descends(&self) -> bool {
        self.converting
    }
}

impl ParseStateLike for ParseState {
    fn current_byte_offset(&self) -> usize {
        self.current_byte_offset()
    }
    fn has_error(&self) -> bool {
        self.has_error()
    }
    fn is_converting(&self) -> bool {
        self.is_converting()
    }
    fn current_byte_offset_descends(&self) -> bool {
        self.current_byte_offset_descends()
    }
}

impl ParseStateLike for tree_sitter::ParseState {
    fn current_byte_offset(&self) -> usize {
        self.current_byte_offset()
    }
    fn has_error(&self) -> bool {
        self.has_error()
    }
    fn is_converting(&self) -> bool {
        false
    }
    fn current_byte_offset_descends(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParserError {
    NoLanguage,
    Canceled,
    Pack(Error),
}

impl std::fmt::Display for ParserError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLanguage => formatter.write_str("no language selected"),
            Self::Canceled => formatter.write_str("parse canceled"),
            Self::Pack(error) => write!(formatter, "packing failed: {error}"),
        }
    }
}

impl std::error::Error for ParserError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Pack(error) => Some(error),
            _ => None,
        }
    }
}

impl From<Error> for ParserError {
    fn from(error: Error) -> Self {
        match error {
            Error::Canceled => Self::Canceled,
            error => Self::Pack(error),
        }
    }
}

/// Parses with Tree-sitter's error recovery and returns independent packed snapshots.
#[derive(Default)]
pub struct Parser {
    native: tree_sitter::Parser,
    language: Option<Language>,
    pack: PackContext,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// A failed selection preserves the previous language.
    pub fn set_language(&mut self, language: &Language) -> Result<(), tree_sitter::LanguageError> {
        self.native.set_language(&language.tree_sitter_language())?;
        self.language = Some(language.clone());
        Ok(())
    }

    pub fn language(&self) -> Option<&Language> {
        self.language.as_ref()
    }

    /// Discards partial parsing state while retaining language and allocation capacity.
    pub fn reset(&mut self) {
        self.native.reset();
    }

    pub fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Tree, ParserError> {
        Parse::parse(self, source)
    }

    pub fn parse_with_options(
        &mut self,
        source: impl AsRef<[u8]>,
        options: PackedParseOptions<'_>,
    ) -> Result<Tree, ParserError> {
        Parse::parse_with_options(self, source, options)
    }

    /// Returns bytes starting at the requested offset; an empty chunk ends input.
    /// Chunk sizes and input offsets must fit Tree-sitter's u32 representation.
    pub fn parse_with_callback<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self,
        callback: &mut F,
        options: PackedParseOptions<'_>,
    ) -> Result<Tree, ParserError> {
        ParseWithCallback::parse_with_callback(self, callback, options)
    }

    /// Releases packing scratch while retaining the selected language.
    pub fn drop_scratch(&mut self) {
        self.pack.drop_scratch();
    }
}

impl Parse for Parser {
    type Tree = Tree;
    type Error = ParserError;
    type Options<'a> = PackedParseOptions<'a>;

    fn parse_with_options(
        &mut self,
        source: impl AsRef<[u8]>,
        options: PackedParseOptions<'_>,
    ) -> Result<Tree, ParserError> {
        let source = source.as_ref();
        if source.len() > u32::MAX as usize {
            return Err(ParserError::Pack(Error::Overflow));
        }
        self.parse_with_callback(&mut slice_callback(source), options)
    }
}

impl ParseWithCallback for Parser {
    fn parse_with_callback<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self,
        callback: &mut F,
        mut options: PackedParseOptions<'_>,
    ) -> Result<Tree, ParserError> {
        let language = self.language.as_ref().ok_or(ParserError::NoLanguage)?;
        let tree = if let Some(progress) = &mut options.parse.progress_callback {
            let mut report = |state: &tree_sitter::ParseState| {
                progress(&ParseState {
                    byte: state.current_byte_offset(),
                    has_error: state.has_error(),
                    converting: false,
                })
            };
            parse_native(&mut self.native, callback, Some(&mut report))?
        } else {
            parse_native(&mut self.native, callback, None)?
        };
        if let Some(progress) = options.parse.progress_callback {
            let has_error = tree.root_node().has_error();
            let mut report = |byte: u32| {
                progress(&ParseState {
                    byte: byte as usize,
                    has_error,
                    converting: true,
                })
            };
            Ok(self.pack.pack_with_progress(
                language,
                &tree,
                options.pack,
                &mut Progress::new(&mut report),
            )?)
        } else {
            Ok(self.pack.pack_with_options(language, &tree, options.pack)?)
        }
    }
}

fn slice_callback<'a>(source: &'a [u8]) -> impl FnMut(usize, Point) -> &'a [u8] {
    move |byte, _| source.get(byte..).unwrap_or_default()
}

fn parse_native<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
    parser: &mut tree_sitter::Parser,
    callback: &mut F,
    progress_callback: Option<&mut dyn FnMut(&tree_sitter::ParseState) -> ControlFlow<()>>,
) -> Result<tree_sitter::Tree, ParserError> {
    parser.reset();
    if parser.language().is_none() {
        return Err(ParserError::NoLanguage);
    }
    let result = parser.parse_with_options(
        callback,
        None,
        Some(tree_sitter::ParseOptions { progress_callback }),
    );
    result.ok_or_else(|| {
        parser.reset();
        ParserError::Canceled
    })
}

impl Parse for tree_sitter::Parser {
    type Tree = tree_sitter::Tree;
    type Error = ParserError;
    type Options<'a> = ParseOptions<'a>;

    fn parse_with_options(
        &mut self,
        source: impl AsRef<[u8]>,
        options: ParseOptions<'_>,
    ) -> Result<tree_sitter::Tree, ParserError> {
        ParseWithCallback::parse_with_callback(self, &mut slice_callback(source.as_ref()), options)
    }
}

impl ParseWithCallback for tree_sitter::Parser {
    fn parse_with_callback<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self,
        callback: &mut F,
        options: ParseOptions<'_>,
    ) -> Result<tree_sitter::Tree, ParserError> {
        if let Some(progress) = options.progress_callback {
            parse_native(self, callback, Some(&mut |state| progress(state)))
        } else {
            parse_native(self, callback, None)
        }
    }
}

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
/// Progress callbacks are unsupported, so this backend does not implement [`Parse`].
pub struct TreeFellerParser {
    native: NativeParser,
    pack: PackContext,
}

impl TreeFellerParser {
    /// Reuses the language's shared direct-parser tables, preparing them on first use.
    pub fn new(language: &Language) -> Result<Self, ParseError> {
        Ok(Self {
            native: NativeParser::new(language)?,
            pack: PackContext::default(),
        })
    }

    pub fn language(&self) -> &Language {
        self.native.language()
    }

    pub fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Tree, ParseError> {
        self.parse_with_options(source, PackOptions::default())
    }

    /// Parses a fresh document. Failure does not prevent subsequent reuse.
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
    pub fn drop_scratch(&mut self) {
        self.native.drop_scratch();
        self.pack.drop_scratch();
    }
}

impl Tree {
    /// Parses directly without constructing a mainline tree.
    /// Use [`TreeFellerParser`] to reuse scratch across documents.
    pub fn parse_direct(language: &Language, source: impl AsRef<[u8]>) -> Result<Self, ParseError> {
        Self::parse_direct_with_options(language, source, PackOptions::default())
    }

    pub fn parse_direct_with_options(
        language: &Language,
        source: impl AsRef<[u8]>,
        options: PackOptions,
    ) -> Result<Self, ParseError> {
        TreeFellerParser::new(language)?.parse_with_options(source, options)
    }
}
