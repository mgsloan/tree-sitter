# Parser API design

Implementation design for `tree-squatter`. Signatures omit routine lifetimes
and method bodies.

## Scope

`Parser` is the recommended entry point and returns packed `Tree` snapshots. It
uses Tree-sitter to parse, then `PackContext` to pack. This supports Tree-sitter's
error recovery for grammars accepted by `Language::new`. Do not select the direct backend
automatically in this implementation: its grammar, input, and error behavior
differs. A later fast path needs equivalent output and cancellation semantics.

Rename the current `Parser` to `TreeFellerParser`. It remains the explicit direct
backend: ABI 15 only, no external scanners or nonterminal extras, and no syntax
error recovery. Keep its owned parse diagnostics and reusable scratch.

`tree_sitter::Parser` remains useful on its own. A shared `traits::Parse` trait lets
generic callers parse contiguous UTF-8 bytes with any of the three parsers and
then navigate the result through `TreeLike`.

Edit registration, old-tree input, incremental reuse, and change tracking are
outside this API. Each call produces a fresh snapshot. Existing Tree-sitter
inherent APIs remain available on `tree_sitter::Parser`.

## Shared parsing contract

```rust
pub trait Parse {
    type Tree: TreeLike;
    type Error;

    fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Self::Tree, Self::Error>;
}

impl Parse for Parser {
    type Tree = Tree;
    type Error = ParserError;
}

impl Parse for TreeFellerParser {
    type Tree = Tree;
    type Error = ParseError;
}

impl Parse for tree_sitter::Parser {
    type Tree = tree_sitter::Tree;
    type Error = ParserError;
}

pub trait ParseWithCallback: Parse {
    type Options<'a>: Default;

    fn parse_with_options<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self,
        callback: &mut F,
        options: Option<Self::Options<'_>>,
    ) -> Result<Self::Tree, Self::Error>;
}

impl ParseWithCallback for Parser {
    type Options<'a> = ParseOptions<'a>;
}

impl ParseWithCallback for tree_sitter::Parser {
    type Options<'a> = tree_sitter::ParseOptions<'a>;
}
```

`ParseWithCallback` shares chunked UTF-8 input while retaining each parser's
result, error, and options types. Generic callers can pass `None` for default
options or constrain the associated options type when they need specific
controls. They can require packed output with
`P: ParseWithCallback<Tree = Tree>`. Its implementations omit old-tree input,
as in `Parse`.
`TreeFellerParser` does not implement it because tree-feller currently
requires contiguous input.

Progress callbacks use parser-specific state types. A separate shared trait
exposes the offset and its traversal direction:

```rust
pub trait ParseStateLike {
    fn current_byte_offset(&self) -> usize;
    fn has_error(&self) -> bool;
    fn is_converting(&self) -> bool;
    fn current_byte_offset_descends(&self) -> bool;
}

impl ParseStateLike for ParseState { /* delegate to inherent methods */ }
impl ParseStateLike for TreeFellerParseState { /* delegate to inherent methods */ }
```

`current_byte_offset_descends()` is true during packing and false during parsing
for both Squatter state types. It describes the phase's traversal direction;
individual offsets are not guaranteed to change monotonically.
`TreeFellerParseState::has_error()` is always false when a callback runs:
the direct parser returns syntax errors rather than recovering from them.

The `Parse` trait covers only contiguous byte input interpreted as UTF-8.
The direct backend has no chunked input reader, and its input size is limited
to `u32::MAX` bytes. The native implementation resets any previously
interrupted parse before calling `tree_sitter::Parser::parse(source, None)`.
It returns `NoLanguage` when no
language was selected; it has no progress callback in this method. The
compatible parser's contiguous input method checks the byte limit before
parsing so packing cannot receive offsets outside its representation.

`Parse` uses associated tree and error types because a native parser should
return its native tree, while the direct parser retains its diagnostic error.
Callers requiring packed output use `P: Parse<Tree = Tree>`; other callers
can use `P::Tree: TreeLike`. The trait does not include language selection,
options, or reset: their contracts differ. Both traits have generic methods,
so they are unsuitable for trait objects; dynamic dispatch is not required.

Tree-sitter's inherent parsing methods shadow the trait methods on a concrete
`tree_sitter::Parser`. Use `Parse::parse(&mut parser, source)` and
`ParseWithCallback::parse_with_options(&mut parser, callback, options)` there.

## Compatible parser

```rust
pub enum ParserError {
    NoLanguage,
    Canceled,
    Pack(Error),
}

pub struct Parser { /* Tree-sitter parser, selected Language, PackContext */ }

impl Parser {
    pub fn new() -> Self;
    pub fn set_language(&mut self, language: &Language)
        -> Result<(), tree_sitter::LanguageError>;
    pub fn language(&self) -> Option<&Language>;
    pub fn reset(&mut self);
    pub fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Tree, ParserError>;
    pub fn parse_with_options<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self,
        callback: &mut F,
        options: Option<ParseOptions<'_>>,
    ) -> Result<Tree, ParserError>;
    pub fn trim(&mut self);
}

#[derive(Default)]
pub struct ParseOptions<'a> {
    pub pack: PackOptions,
    pub progress_callback: Option<&'a mut dyn FnMut(&ParseState) -> ControlFlow<()>>,
}

impl<'a> ParseOptions<'a> {
    pub fn new() -> Self;
    pub fn progress_callback<F: FnMut(&ParseState) -> ControlFlow<()>>(
        self,
        callback: &'a mut F,
    ) -> Self;
    pub fn reborrow(&mut self) -> ParseOptions<'_>;
}

pub struct ParseState { /* valid only during the callback */ }
impl ParseState {
    pub fn current_byte_offset(&self) -> usize;
    pub fn has_error(&self) -> bool;
    pub fn is_converting(&self) -> bool;
    pub fn current_byte_offset_descends(&self) -> bool;
}
```

`new()` is infallible: initialize empty packing scratch without allocating
(adding `Default` to `PackContext` if needed) and use Tree-sitter's infallible
constructor. `set_language` assigns the underlying
Tree-sitter language first, then retains a clone of the prepared `Language`.
A failed selection preserves the previous language. `language()` returns that
retained wrapper; parsing without it returns `NoLanguage`.

The callback input contract matches Tree-sitter: given a byte offset and point,
return bytes starting there; an empty slice ends input. `parse()` adapts a
contiguous slice to that callback. `ParseOptions::pack` defaults to the existing
`PackOptions` defaults: zero initial group capacity, no repacking, presence and
points enabled. `reborrow()` copies `pack` and reborrows the mutable callback.
`None` options use these defaults.

The callback input path follows Tree-sitter's size behavior. Its Rust binding
passes a `u32` byte offset to the callback and casts each returned chunk length
to `u32` without an overflow check; the C lexer also tracks byte positions in
`u32`. Squatter adds no callback-input size check or overflow error. Inputs
that exceed this offset range have no reliable result. The contiguous
`Parser::parse` path still rejects slices longer than `u32::MAX` before
parsing, as required by packed tree storage.

The progress callback runs during Tree-sitter parsing and packing.
`Continue(())` continues; `Break(())` returns `Canceled`.
`ParseState::is_converting()` is false while parsing and true from the start
of packing through its final layout and side-data work. During parsing,
`current_byte_offset()` and `has_error()` report Tree-sitter's parse state.
During packing, the offset is the start byte of the current or last visited
input node; the first converting callback occurs after selecting the root.
`has_error()` reports whether the parsed tree contains an error. The packing
traversal visits children right to left, so the offset can move backward. In
neither phase is it a monotonic work counter or completion estimate.

A parse that returns `None` after language selection is cancellation. Reset
the underlying parser before returning so the next call starts a fresh
document. `reset()` explicitly discards any partial parse state while
retaining language and allocation capacity. `trim()` releases packing
scratch; it does not change language.

Packing errors return `ParserError::Pack(error)`. Poll conversion traversal
and long finalization loops so cancellation can discard a partial packed tree.
No partial tree is returned on cancellation or packing failure.

## Direct parser

```rust
pub struct TreeFellerParser { /* current Parser, renamed */ }

impl TreeFellerParser {
    pub fn new(language: &Language) -> Result<Self, ParseError>;
    pub fn language(&self) -> &Language;
    pub fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Tree, ParseError>;
    pub fn parse_with_options(
        &mut self,
        source: impl AsRef<[u8]>,
        options: TreeFellerParseOptions<'_>,
    ) -> Result<Tree, ParseError>;
    pub fn trim(&mut self);
}

#[derive(Default)]
pub struct TreeFellerParseOptions<'a> {
    pub pack: PackOptions,
    pub progress_callback:
        Option<&'a mut dyn FnMut(&TreeFellerParseState) -> ControlFlow<()>>,
}

impl<'a> TreeFellerParseOptions<'a> {
    pub fn new() -> Self;
    pub fn progress_callback<F: FnMut(&TreeFellerParseState) -> ControlFlow<()>>(
        self,
        callback: &'a mut F,
    ) -> Self;
    pub fn reborrow(&mut self) -> TreeFellerParseOptions<'_>;
}

pub struct TreeFellerParseState { /* valid only during the callback */ }
impl TreeFellerParseState {
    pub fn current_byte_offset(&self) -> usize;
    pub fn has_error(&self) -> bool;
    pub fn is_converting(&self) -> bool;
    pub fn current_byte_offset_descends(&self) -> bool;
}
```

Keep the direct constructor fallible. It validates the language and prepares
driver tables; an infallible `new()` would need deferred table preparation and
new no-language behavior. `language()` borrows its retained wrapper. Rename the
current `parse_with_options(source, PackOptions)` use sites to pass
`TreeFellerParseOptions { pack, ..Default::default() }`. Keep `Tree::parse_direct`
and `Tree::parse_direct_with_options` as convenience methods taking `PackOptions`.

Add a progress hook to tree-feller's parsing loop, including long speculative
scans, and propagate cancellation through the C wrapper. The callback receives
the current source byte offset. Polling frequency is an implementation
detail, but a pending cancellation must be observed during parsing, including
when no reductions occur. Add `Error::Canceled` and return a diagnostic
`ParseError` with that code at the last reported offset. Failed or canceled
parses clear logical state and leave the parser reusable on a different source
without reset. The direct callback also runs during packing. Its
`is_converting()` and offset follow the same phase and input-node rules as
`ParseState`; `Break(())` discards the partial packed tree.

## Implementation order and checks

1. Rename the current direct parser and update exports and call sites. Preserve
   its existing parsing and diagnostic behavior.
2. Add `Parse` with three implementations and `ParseWithCallback` with two,
   including associated tree, error, and options types. Add the compatible
   `Parser` using Tree-sitter and `PackContext`.
3. Add options, callback input, cancellation, and reset to the compatible parser.
4. Add progress and cancellation to conversion traversal and finalization, then
   to tree-feller and its Rust wrapper. Rename the direct options parameter as
   above.
5. Check language replacement after failed selection, syntax-error recovery
   versus direct rejection, callback input, cancellation in both phases followed
   by reuse, and packing failure. Check both phase flags and backward offsets
   during conversion. Compare successful packed trees from both paths for a
   grammar and source accepted by tree-feller.

Included ranges, UTF-16/custom decoding, logging, DOT output, parse-state
inspection beyond progress callbacks, and lookahead APIs are separate work.
Packed trees must never be exposed as `TSTree` pointers.
