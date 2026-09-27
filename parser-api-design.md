# Parser API design

Implementation design for `tree-squatter`. Signatures omit routine lifetimes
and method bodies.

Implementation scope: direct-parser progress and cancellation are deferred to
`experimental/tree-feller-cancel`. On `parser-api`, `TreeFellerParser` keeps
`parse_with_options(source, PackOptions)` and does not implement `Parse` or
expose `TreeFellerParseState`. The direct-parser callback design below describes
that experiment; the compatible parser and native Tree-sitter implement the
shared parsing traits.

## Scope

`Parser` is the recommended entry point and returns packed `Tree` snapshots. It
uses Tree-sitter to parse, then `PackContext` to pack. This supports Tree-sitter's
error recovery for grammars accepted by `Language::new`. Do not select the
direct backend automatically in this implementation: its grammar, input, and
error behavior differs. A later fast path needs equivalent output and
cancellation semantics.

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
    type Options<'a>: Default + From<ParseOptions<'a>>;

    fn parse_with_options(
        &mut self,
        source: impl AsRef<[u8]>,
        options: Self::Options<'_>,
    ) -> Result<Self::Tree, Self::Error>;

    fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Self::Tree, Self::Error> {
        self.parse_with_options(source, Default::default())
    }
}

impl Parse for Parser {
    type Tree = Tree;
    type Error = ParserError;
    type Options<'a> = PackedParseOptions<'a>;
}

impl Parse for TreeFellerParser {
    type Tree = Tree;
    type Error = ParseError;
    type Options<'a> = PackedParseOptions<'a>;
}

impl Parse for tree_sitter::Parser {
    type Tree = tree_sitter::Tree;
    type Error = ParserError;
    type Options<'a> = ParseOptions<'a>;
}

pub trait ParseWithCallback: Parse {
    fn parse_with_callback<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self,
        callback: &mut F,
        options: Self::Options<'_>,
    ) -> Result<Self::Tree, Self::Error>;
}

impl ParseWithCallback for Parser {}
impl ParseWithCallback for tree_sitter::Parser {}
```

`ParseWithCallback` shares chunked UTF-8 input while retaining each parser's
result, error, and options types from `Parse`. Generic callers can pass
`Default::default()` or convert shared `ParseOptions` with `.into()`.
They can require packed output with
`P: ParseWithCallback<Tree = Tree>`. Its implementations omit old-tree input,
as in `Parse`. `TreeFellerParser` does not implement it because tree-feller
currently requires contiguous input.

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
impl ParseStateLike for tree_sitter::ParseState { /* native accessors */ }
```

Shared `ParseOptions` carries only parser controls. Packed parsers add
`PackOptions` without making native Tree-sitter accept meaningless packing
settings:

```rust
#[derive(Default)]
pub struct ParseOptions<'a> {
    pub progress_callback:
        Option<&'a mut dyn FnMut(&dyn ParseStateLike) -> ControlFlow<()>>,
}

impl<'a> ParseOptions<'a> {
    pub fn new() -> Self;
    pub fn progress_callback<F: FnMut(&dyn ParseStateLike) -> ControlFlow<()>>(
        self,
        callback: &'a mut F,
    ) -> Self;
    pub fn reborrow(&mut self) -> ParseOptions<'_>;
}

#[derive(Default)]
pub struct PackedParseOptions<'a> {
    pub parse: ParseOptions<'a>,
    pub pack: PackOptions,
}

impl<'a> PackedParseOptions<'a> {
    pub fn new() -> Self;
    pub fn reborrow(&mut self) -> PackedParseOptions<'_>;
}

impl<'a> From<ParseOptions<'a>> for PackedParseOptions<'a> {
    fn from(parse: ParseOptions<'a>) -> Self {
        Self { parse, pack: PackOptions::default() }
    }
}
```

`current_byte_offset_descends()` is true during packing and false during parsing
for both Squatter state types. It describes the phase's traversal direction;
individual offsets are not guaranteed to change monotonically.
`TreeFellerParseState::has_error()` is always false when a callback runs:
the direct parser returns syntax errors rather than recovering from them.
For native `tree_sitter::ParseState`, `is_converting()` and
`current_byte_offset_descends()` are false; the other methods delegate to
Tree-sitter. A false direction flag is not a monotonicity guarantee.

The shared callback receives a borrowed `dyn ParseStateLike` valid only during
the call. Each backend adapts its own progress state to that interface. Native
trait implementations adapt shared `ParseOptions` to Tree-sitter's inherent
`tree_sitter::ParseOptions`. The callback is absent by default, so no state
adaptation runs unless progress reporting is requested.

Generic callers can install the same callback for all three parsers:

```rust
fn parse_with_progress<P: Parse>(
    parser: &mut P,
    source: &[u8],
    callback: &mut impl FnMut(&dyn ParseStateLike) -> ControlFlow<()>,
) -> Result<P::Tree, P::Error> {
    let options = ParseOptions::new().progress_callback(callback);
    parser.parse_with_options(source, options.into())
}
```

The `Parse` trait covers only contiguous byte input interpreted as UTF-8.
The direct backend has no chunked input reader, and its input size is limited
to `u32::MAX` bytes. The native implementation resets any previously
interrupted parse before calling Tree-sitter with no old tree.
It returns `NoLanguage` when no language was selected. The default
`parse()` call has no progress callback; `parse_with_options` can supply one.
The compatible parser's contiguous input method checks the byte limit before
parsing so packing cannot receive offsets outside its representation.

`Parse` uses associated tree and error types because a native parser should
return its native tree, while the direct parser retains its diagnostic error.
Callers requiring packed output use `P: Parse<Tree = Tree>`; other callers
can use `P::Tree: TreeLike`. `parse()` defaults to
`parse_with_options(source, Default::default())`. The two callback parsers
can share a byte-slice-to-callback helper, but implement `Parse` explicitly:
the packed parser checks contiguous length before parsing, while the native
parser follows Tree-sitter's input behavior. A blanket `Parse` implementation
for `ParseWithCallback` would need an extra per-parser validation hook and
would conflict with the current supertrait relationship. Neither trait
includes language selection or reset; their contracts differ. Both traits
have generic methods, so they are unsuitable for trait objects.

Tree-sitter's inherent parsing methods shadow the trait methods on a concrete
`tree_sitter::Parser`. Use `Parse::parse(&mut parser, source)`,
`Parse::parse_with_options(&mut parser, source, options)`, and
`ParseWithCallback::parse_with_callback(&mut parser, callback, options)` there.

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
    pub fn parse_with_options(
        &mut self,
        source: impl AsRef<[u8]>,
        options: PackedParseOptions<'_>,
    ) -> Result<Tree, ParserError>;
    pub fn parse_with_callback<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self,
        callback: &mut F,
        options: PackedParseOptions<'_>,
    ) -> Result<Tree, ParserError>;
    pub fn drop_scratch(&mut self);
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
contiguous slice to that callback. `PackedParseOptions::pack` uses the
existing `PackOptions` defaults: zero initial group capacity, no repacking,
presence and points enabled. `PackedParseOptions::reborrow()` copies `pack`
and reborrows the shared progress callback. `parse()` uses these defaults.

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
retaining language and allocation capacity. `drop_scratch()` releases packing
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
        options: PackedParseOptions<'_>,
    ) -> Result<Tree, ParseError>;
    pub fn drop_scratch(&mut self);
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
`PackedParseOptions { pack, ..Default::default() }`. Keep `Tree::parse_direct`
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
   including associated tree and error types. Add shared `ParseOptions`,
   `PackedParseOptions`, and progress-state adapters. Share the callback
   input adapter where possible. Add the compatible `Parser` using
   Tree-sitter and `PackContext`.
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
