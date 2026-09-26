# Parser API design

Reserve the exported `Parser` struct for the recommended way to parse into a
squatter tree. Expose `TreeFellerParser` for the restricted direct backend, and
provide a parser trait implemented by both of these and `tree_sitter::Parser`.
The trait's name and contract remain open; `Parse` is a candidate name.

This document owns parser design previously included in
[API differences to fix](api-differences-to-fix.md). It records proposed APIs,
not implemented functionality. The concrete outlines below predate the trait
discussion; reconcile them with the decisions below before implementation.
Declaration blocks omit bodies and some lifetime details.

## Implementations

- `Parser`: recommended squatter entry point. Backend selection and any fallback
  policy remain undecided. The compatible path must recover syntax errors and
  support the corresponding Tree-sitter grammars, potentially by parsing through
  Tree-sitter and packing the result.
- `TreeFellerParser`: explicit direct backend with diagnostic errors. Current
  restrictions are ABI 15, no external scanners or nonterminal extras, and no
  syntax-error recovery. Rename the existing squatter `Parser` to this name.
- `tree_sitter::Parser`: implement the local parser trait on the native type.
  Whether that implementation also packs its output is an open decision.

## Shared trait decisions

The main choice is the result tree:

- Always return a squatter `Tree`. Implementing the trait for
  `tree_sitter::Parser` then means parsing and packing. `ParseOptions::pack`
  applies uniformly. A bare native parser has nowhere to retain squatter packing
  scratch; the recommended `Parser` can own it.
- Use an associated tree type. Tree-sitter returns its native tree and callers
  can use the shared navigation traits. Packing options then belong to
  squatter-specific configuration rather than the common parsing contract.

Always returning a squatter tree was suggested, but has not been selected.
Do not commit to a trait signature until resolving this choice and these points:

- Error shape: the compatible outline returns `Option<Tree>`, whereas the direct
  backend returns `Result<Tree, ParseError>`. Decide whether the trait uses a
  common error or an associated error, preserving cancellation and diagnostics.
- Input shape: the compatible `parse_with_options` takes an input callback;
  the direct outline takes contiguous bytes. Decide which operations are common
  and what adapters or backend extensions are necessary.
- Language shape: squatter uses its prepared `Language` wrapper; the native parser
  owns a Tree-sitter language. Language access through the trait cannot simply
  promise a borrowed prepared wrapper stored inside a bare native parser.
- Options: planned squatter `ParseOptions` includes `PackOptions` and an optional
  progress callback. The direct outline still takes `PackOptions`; decide how it
  adopts parsing options when adding progress support.

Tree-sitter's inherent methods can shadow identically named trait methods.
Generic code bounded by the trait can use its methods; concrete native-parser
calls may need qualified syntax such as `Parse::parse_with_options(...)`.
Generic input methods also make the trait unsuitable for trait objects without
further design. Dynamic dispatch is not currently a requirement.

## Language selection and lifecycle

Consider giving `TreeFellerParser` the `new()` / `set_language(...)` lifecycle.
This would allow a worker to retain parser and packing allocation capacity while
switching languages. Validate backend restrictions and prepare tables during
language selection; replace or reset language-specific state on success.
Suggested behavior is to preserve the previous language after failed selection
and return an error when parsing without a language.

This lifecycle is not yet selected. Its current constructor is fallible; inspect
which allocations can be deferred before promising infallible `new()`. The API
comparison retains the earlier constructor until this decision is made.

## Options and progress

Use parser-specific state, not a generic cross-API progress type. Match
Tree-sitter's current-byte-offset and error-state accessors where meaningful.
Callbacks return `ControlFlow<()>`; deadlines and cancellation flags can be
captured by the caller. The direct backend needs an interruption hook: its current
reduction sink cannot stop `tf_parse`. Also specify whether and how cancellation
covers packing after parsing; a parser callback alone does not cover that work.

The current proposal passes options by value, following Tree-sitter. `reborrow()`
allows sequential reuse of the mutable callback without cloning or allocating;
for squatter it also copies `PackOptions`. Taking options by mutable reference is
an alternative, not a settled change.

An optional callback is sufficient to represent disabled callbacks. A configurable
polling stride remains a possible addition with parser-specific units and defaults;
Tree-sitter uses internal throttling without exposing a stride option. No special
`u32::MAX` sentinel is needed when callback absence expresses disabled callbacks.
Source position is not a monotonic work counter or total-work estimate.

Specify cancellation, parser reuse, and resumption separately. Omitting incremental
old-tree input does not determine whether an interrupted parse can resume. Likewise,
`reset()` needs a concrete contract before implementation.

## API comparison

Parser compatibility is a larger target. Edit registration, incremental reuse,
and change tracking are explicitly excluded.

**Current tree-sitter**

```rust
use std::ops::ControlFlow;

pub struct ParseState { /* backend-specific representation */ }
impl ParseState {
    pub const fn current_byte_offset(&self) -> usize;
    pub const fn has_error(&self) -> bool;
}

#[derive(Default)]
pub struct ParseOptions<'a> {
    pub progress_callback: Option<&'a mut dyn FnMut(&ParseState) -> ControlFlow<()>>,
}
impl<'a> ParseOptions<'a> {
    pub fn new() -> Self;
    pub fn progress_callback<F: FnMut(&ParseState) -> ControlFlow<()>>(
        self, callback: &'a mut F,
    ) -> Self;
    pub fn reborrow(&mut self) -> ParseOptions<'_>;
}

impl Parser {
    pub fn new() -> Self;
    pub fn set_language(&mut self, language: &Language) -> Result<(), LanguageError>;
    pub fn language(&self) -> Option<LanguageRef<'_>>;
    pub fn reset(&mut self);
    pub fn parse(&mut self, source: impl AsRef<[u8]>, old_tree: Option<&Tree>)
        -> Option<Tree>;
    pub fn parse_with_options<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self, callback: &mut F, old_tree: Option<&Tree>, options: Option<ParseOptions<'_>>,
    ) -> Option<Tree>;
}
impl Node<'_> {
    pub fn has_changes(&self) -> bool;
    pub fn edit(&mut self, edit: &InputEdit);
}
impl Tree {
    pub fn edit(&mut self, edit: &InputEdit);
    pub fn changed_ranges(&self, other: &Self) -> impl ExactSizeIterator<Item = Range>;
}
```

**Current tree-squatter**

```rust
// no ParseOptions or ParseState; parse_with_options takes packing controls
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PackOptions {
    pub initial_group_capacity: u32,
    pub repack: bool,
    pub symbol_presence: bool,
    pub points: bool,
}

impl Parser {
    pub fn new(language: &Language) -> Result<Self, ParseError>;
    pub fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Tree, ParseError>;
    pub fn parse_with_options(&mut self, source: impl AsRef<[u8]>, options: PackOptions)
        -> Result<Tree, ParseError>;
    pub fn trim(&mut self);
}
// direct parser rejects syntax errors and some grammars
impl Node<'_> {
    pub fn has_changes(self) -> bool; // always false
}
// no edits, old-tree input, or changed ranges
```

**Proposed tree-squatter, before resolving the shared trait**

```rust
use std::ops::ControlFlow;

pub struct ParseState { /* backend-specific representation */ }
impl ParseState {
    pub const fn current_byte_offset(&self) -> usize;
    pub const fn has_error(&self) -> bool;
}

#[derive(Default)]
pub struct ParseOptions<'a> {
    pub pack: PackOptions,
    pub progress_callback: Option<&'a mut dyn FnMut(&ParseState) -> ControlFlow<()>>,
}
impl<'a> ParseOptions<'a> {
    pub fn new() -> Self;
    pub fn progress_callback<F: FnMut(&ParseState) -> ControlFlow<()>>(
        self, callback: &'a mut F,
    ) -> Self;
    pub fn reborrow(&mut self) -> ParseOptions<'_>;
}

impl Parser {
    pub fn new() -> Self;
    pub fn set_language(&mut self, language: &Language) -> Result<(), LanguageError>;
    pub fn language(&self) -> Option<&Language>;
    pub fn reset(&mut self);
    pub fn parse(&mut self, source: impl AsRef<[u8]>)
        -> Option<Tree>;
    pub fn parse_with_options<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &mut self, callback: &mut F, options: Option<ParseOptions<'_>>,
    ) -> Option<Tree>;
}
// No has_changes, node/tree edit, changed_ranges, or old-tree input.
impl TreeFellerParser {
    pub fn new(language: &Language) -> Result<Self, ParseError>;
    pub fn parse(&mut self, source: impl AsRef<[u8]>) -> Result<Tree, ParseError>;
    pub fn parse_with_options(&mut self, source: impl AsRef<[u8]>, options: PackOptions)
        -> Result<Tree, ParseError>;
    pub fn trim(&mut self);
}
```

- Match the compatible parser lifecycle using the necessary grammar wrapper.
- Reserve `parse_with_options` for callback input and parsing options. Extend
  squatter `ParseOptions` with `pack: PackOptions` to configure the resulting tree.
- Match `ParseOptions::new`, `Default`, the callback builder, and `reborrow`.
  The callback is borrowed for parsing; `None` disables it. `Continue(())`
  continues and `Break(())` cancels. Reborrowing allows sequential reuse of
  options containing a mutable callback; `reborrow` copies the packing options.
- Use parser-specific `ParseState`, exposing the current byte offset and error
  flag. The offset is source position, not a completed-work count or percentage;
  no total-work estimate is promised. These outlines expand tree-sitter's private
  callback type alias and omit the state's private representation.
- Tree-sitter exposes no polling stride in `ParseOptions`. Any configurable
  stride would be a tree-squatter addition, separate from this shared shape.
- Current `PackOptions` defaults are zero initial group capacity, no repacking,
  symbol presence enabled, and points enabled. Use these defaults for
  `ParseOptions::pack`; packing controls remain grouped in `PackOptions`.
- Keep the restricted direct parser as an explicit additional capability with
  diagnostic errors. The compatible path must recover errors and support the
  corresponding grammars; it may delegate to tree-sitter and pack the result.
- Remove `Node::has_changes()` and exclude `Node::edit`, `Tree::edit`,
  `Tree::changed_ranges`, and edit-registration types such as `InputEdit`.
  Omit old-tree parameters and incremental reuse from the parser proposal;
  parsing produces fresh snapshots. Do not retain no-op change APIs.
- Track included ranges, UTF-16/custom encoding input, parse-state inspection,
  and lookahead support with this work.
- Logging and DOT output are also missing conveniences. Raw pointers, allocator
  hooks, and Wasm integration require backend-specific contracts; a packed tree
  must never be presented as a `TSTree`.

