use super::*;

/// Direct-parser failure, including its owned diagnostic and source position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    pub code: Error,
    pub byte: u32,
    pub point: Point,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}: {}", self.code, self.byte, self.message)
    }
}

impl std::error::Error for ParseError {}

#[repr(C)]
struct RawParseError {
    code: i32,
    byte: u32,
    point: RawPoint,
    message: [u8; 512],
}

impl RawParseError {
    fn new() -> Self {
        Self {
            code: 0,
            byte: 0,
            point: RawPoint { row: 0, column: 0 },
            message: [0; 512],
        }
    }

    fn into_error(self) -> ParseError {
        let length = self
            .message
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(512);
        ParseError {
            code: error(self.code),
            byte: self.byte,
            point: self.point.into(),
            message: String::from_utf8_lossy(&self.message[..length]).into_owned(),
        }
    }
}

fn source_length(source: &[u8]) -> Result<u32, ParseError> {
    source.len().try_into().map_err(|_| ParseError {
        code: Error::Overflow,
        byte: 0,
        point: Point::default(),
        message: "source exceeds the 32-bit byte limit".into(),
    })
}

/// Reusable direct parser retaining its grammar and worker-local scratch.
///
/// Requires an ABI 15 grammar without external scanners or nonterminal extras.
/// Syntax errors are returned rather than recovered; no mainline tree is built.
/// Raw reductions are buffered for the whole parse before column encoding.
/// Output trees own their storage and remain valid across reuse or parser drop.
pub struct Parser(NonNull<c_void>);

// The native parser exclusively owns its mutable scratch and retains immutable
// grammar tables. Moving ownership is safe; concurrent shared use is not.
unsafe impl Send for Parser {}

impl Parser {
    pub fn new(grammar: &Grammar) -> Result<Self, ParseError> {
        let mut status = RawParseError::new();
        let raw = unsafe { ffi::sq_parser_new(grammar.0.as_ptr(), &mut status) };
        NonNull::new(raw)
            .map(Self)
            .ok_or_else(|| status.into_error())
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
        let source = source.as_ref();
        let length = source_length(source)?;
        let mut status = RawParseError::new();
        let raw = unsafe {
            ffi::sq_parser_parse(
                self.0.as_ptr(),
                source.as_ptr().cast(),
                length,
                options,
                &mut status,
            )
        };
        NonNull::new(raw)
            .map(Tree)
            .ok_or_else(|| status.into_error())
    }

    /// Release high-water scratch while retaining the prepared grammar.
    pub fn trim(&mut self) {
        unsafe { ffi::sq_parser_trim(self.0.as_ptr()) }
    }
}

impl Drop for Parser {
    fn drop(&mut self) {
        unsafe { ffi::sq_parser_delete(self.0.as_ptr()) }
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

mod ffi {
    use super::*;

    unsafe extern "C" {
        pub fn sq_parser_new(grammar: *mut c_void, error: *mut RawParseError) -> *mut c_void;
        pub fn sq_parser_delete(parser: *mut c_void);
        pub fn sq_parser_trim(parser: *mut c_void);
        pub fn sq_parser_parse(
            parser: *mut c_void,
            source: *const c_char,
            length: u32,
            options: PackOptions,
            error: *mut RawParseError,
        ) -> *mut c_void;
    }
}
