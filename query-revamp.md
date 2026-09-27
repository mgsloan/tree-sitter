# Query revamp

The query revamp is implemented: metadata and diagnostics, public index types,
chunked text providers, streaming iterators, and resumable progress callbacks.
This document describes the current API and its differences from the tree-sitter
checkout in this repository. API outlines omit private fields and method bodies.

Ordinary bounded queries work in general and optimized execution, including trees
with parse errors. Containing ranges and strict source ordering remain deferred.
Callback cost measurement and tuning were skipped; the current polling threshold
is unmeasured. Navigation, storage, and scan APIs are outside this revamp.

## Query compilation, metadata, and errors

```rust
impl Query {
    pub fn new(language: &Language, source: &str) -> Result<Self, QueryError>;
    pub fn pattern_count(&self) -> usize;
    pub fn disable_pattern(&mut self, index: PatternIx);
    pub fn disable_capture(&mut self, name: &str);
    pub const fn capture_names(&self) -> &[&str];
    pub fn capture_index_for_name(&self, name: &str) -> Option<CaptureIx>;
    pub const fn capture_quantifiers(&self, index: PatternIx) -> &[CaptureQuantifier];
    pub const fn property_settings(&self, index: PatternIx) -> &[QueryProperty];
    pub const fn property_predicates(&self, index: PatternIx) -> &[(QueryProperty, bool)];
    pub const fn general_predicates(&self, index: PatternIx) -> &[QueryPredicate];
    pub fn start_byte_for_pattern(&self, index: PatternIx) -> usize;
    pub fn end_byte_for_pattern(&self, index: PatternIx) -> usize;
    pub fn is_pattern_rooted(&self, index: PatternIx) -> bool;
    pub fn is_pattern_non_local(&self, index: PatternIx) -> bool;
    pub fn is_pattern_guaranteed_at_step(&self, offset: usize) -> bool;
    pub fn deep_clone(&self) -> Self;
}
pub struct QueryError {
    pub row: usize,
    pub column: usize,
    pub offset: usize,
    pub message: String,
    pub kind: QueryErrorKind,
}
```

- `Query::new` accepts the prepared `Language` wrapper;
  `Language::tree_sitter_language()` exposes the underlying tree-sitter language.
- Capture names are borrowed string slices. Disabling patterns or captures
  does not renumber their indices.
- `CaptureQuantifier` and `QueryErrorKind` are re-exported from tree-sitter.
  Property and predicate types belong to squatter because they carry `CaptureIx`.
- `set!` appears in settings and `is?`/`is-not?` in property predicates. These
  operators are excluded from general predicates; the host evaluates both kinds
  of property metadata and general predicates.
- Compilation diagnostics match tree-sitter's error fields and `Display` output.
  Predicate errors use the pattern's row with column and offset zero, including
  errors in later predicates or patterns. Messages and validation order match
  tree-sitter's Rust binding.
- `deep_clone` copies disabled-pattern/capture state for independent mutation.
  Queries may be shared across cursors and threads without cloning.

## Query indices and result types

`PatternIx(usize)` identifies query-global patterns, `CaptureIx(u32)` identifies
query-global capture names, `MatchId(u32)` identifies matches, and
`MatchCaptureIx(u32)` identifies positions within a match's capture slice.
These are separate domains, even when their values coincide.

A pattern index is the zero-based position of a top-level pattern in the compiled
query. Each match identifies the pattern that produced it; many matches can share
one pattern index.

```rust
pub struct PatternIx(pub usize);
pub struct CaptureIx(pub u32);
pub struct MatchId(pub u32);
pub struct MatchCaptureIx(pub u32);

pub struct QueryCapture<'tree> {
    pub node: Node<'tree>,
    pub index: CaptureIx,
}
pub struct QueryMatch<'cursor, 'tree> {
    pub pattern_index: PatternIx,
    // private captures, match ID, and removal state
}
impl<'tree> QueryMatch<'_, 'tree> {
    pub const fn id(&self) -> MatchId;
    pub const fn captures(&self) -> &[QueryCapture<'tree>];
    pub fn remove(&self);
    pub fn nodes_for_capture_index(&self, capture_ix: CaptureIx)
        -> impl Iterator<Item = Node<'tree>> + '_;
}
pub struct QueryProperty {
    pub key: Box<str>,
    pub value: Option<Box<str>>,
    pub capture_id: Option<CaptureIx>,
}
impl QueryProperty {
    pub fn new(key: &str, value: Option<&str>, capture_id: Option<CaptureIx>) -> Self;
}
pub enum QueryPredicateArg {
    Capture(CaptureIx),
    String(Box<str>),
}
pub struct QueryPredicate {
    pub operator: Box<str>,
    pub args: Box<[QueryPredicateArg]>,
}
// QueryMatches::Item = QueryMatch<'cursor, 'tree>
// QueryCaptures::Item = (QueryMatch<'cursor, 'tree>, MatchCaptureIx)
impl<'tree, Provider, Chunk> QueryExecution<'_, '_, 'tree, '_, Provider, Chunk>
where
    Provider: TextProvider<Chunk>,
    Chunk: AsRef<[u8]>,
{
    pub fn next_match(&mut self) -> Option<QueryMatch<'_, 'tree>>;
    pub fn next_capture(&mut self) -> Option<(QueryMatch<'_, 'tree>, MatchCaptureIx)>;
    pub fn remove_match(&mut self, id: MatchId);
    pub fn error(&self) -> Option<QueryExecutionError>;
}
pub enum QueryExecutionError {
    InvalidExecution,
}
```

- `CaptureIx` is used consistently for `QueryCapture::index`,
  `Query::capture_index_for_name`, `QueryMatch::nodes_for_capture_index`,
  `QueryPredicateArg::Capture`, and `QueryProperty::capture_id` (including its
  constructor). The last currently uses `usize` in tree-sitter despite referring
  to the same capture-name domain.
- Index capture-name and quantifier slices with `capture_ix.0 as usize`:
  `query.capture_names()[capture_ix.0 as usize]` and
  `query.capture_quantifiers(pattern_ix)[capture_ix.0 as usize]`. Pattern
  arguments use `PatternIx` directly; slice positions use the inner integer.
- `PatternIx` is used for `QueryMatch::pattern_index` and all pattern-index
  arguments on `Query`, including `disable_pattern` and per-pattern metadata accessors.
  Its backing type is `usize`. Counts and query-source byte offsets remain
  `usize`; `is_pattern_guaranteed_at_step` takes a byte offset, not a pattern index.
- `MatchCaptureIx` identifies capture-event positions in both iteration APIs.
  Convert its value to `usize` when indexing the match's capture slice. Repeated
  captures can have the same name ID and different positions;
  `nodes_for_capture_index` returns all occurrences of that name.
- The capture and match wrappers use `u32`. `CaptureIx` preserves zero-based
  indices without offset encoding. Squatter does not adopt tree-sitter's internal
  `u16` match capture-count limit; repetitions can yield many occurrences of one
  name. `MatchId` is shared by `QueryMatch::id()` and `remove_match()`.

## Query iteration and match access

```rust
use tree_squatter::StreamingIterator;

let mut matches = cursor.matches(&query, root, source_bytes);
while let Some(found) = matches.next() {
    let captures = found.captures();
    found.remove();
}
drop(matches);

let mut captures = cursor.captures(&query, root, source_bytes);
while let Some((found, index)) = captures.next() {
    let capture = found.captures()[index.0 as usize];
}
drop(captures);

let mut execution = cursor.execute(&query, root, source_bytes);
while let Some(found) = execution.next_match() {
    let id = found.id();
    execution.remove_match(id);
}
let error = execution.error();
```

- `matches`, `captures`, `execute`, and their options variants accept a root `Node`
  by value and use the same text-provider interface.
- `matches` and `captures` implement the re-exported `StreamingIterator` trait.
  `execute` retains explicit `next_match` and `next_capture` methods.
- Capture events retain provisional snapshots, unspecified event order, and
  duplicates. They cover completed-match captures but can expose snapshots that
  later gain captures or lose longest-match filtering. Event order, provisional
  contents, and multiplicity can differ from tree-sitter. Use completed matches
  when provisional events are unsuitable.
- `QueryExecution::error()` reports query/node language mismatches.
  Unavailable optimizations fall back to general execution.
- The snippets use `source_bytes: &[u8]`, which can be copied into each provider
  argument. A provider that is not `Copy` needs a separate value for each execution.

### Borrowing and match removal

Each execution exclusively borrows its cursor and shares borrows of the query
and tree. It owns its text provider and holds any callback borrow until dropped.
Dropping the execution releases those borrows and leaves the cursor reusable.

`QueryExecution::next_match` and `next_capture` return matches tied to the current
mutable borrow of the execution. Streaming iterators return references to their
current item. In both APIs, advancing requires those result borrows to end.
`captures()` borrows from the match; copying a captured `Node` retains only its
tree lifetime and does not prevent advancement.

`QueryMatch::remove(&self)` suppresses subsequent results for that match ID in
this execution. It leaves the current match and any borrowed capture slice
readable. Removal is recorded through interior-mutable state and applied before
the next advancement can emit results or reuse capture storage. Repeated removal
is a no-op; already returned results remain valid. Dropping a match without
calling `remove` does not suppress its remaining captures.

`QueryExecution::remove_match(MatchId)` provides the same suppression by ID after
the result borrow ends. Match IDs belong to one execution and may be reused by a
fresh execution. Capture storage and removal state are private; iterator adapters
expose references only for the current result borrow.

## Text providers and predicate evaluation

Squatter's `TextProvider` has tree-sitter's trait shape, accepting packed `Node`
values and retaining the associated iterator name `I`.

```rust
pub trait TextProvider<Chunk: AsRef<[u8]>> {
    type I: Iterator<Item = Chunk>;
    fn text(&mut self, node: Node<'_>) -> Self::I;
}

impl<'text> TextProvider<&'text [u8]> for &'text [u8] {
    type I = std::iter::Once<&'text [u8]>;

    fn text(&mut self, node: Node<'_>) -> Self::I {
        std::iter::once(&self[node.byte_range()])
    }
}
```

Closures returning chunk iterators are also supported. Chunks may be borrowed or
owned. Each call supplies the node's complete text in source order;
chunk boundaries have no semantic significance. The byte-slice implementation
indexes source bytes using the node's byte range, without copying.

All query entry points take the provider by value and retain it for execution.
The same bounds and lifetime order apply to `QueryMatches`, `QueryCaptures`, and
`QueryExecution`: `<'cursor, 'query, 'tree, 'options, Provider, Chunk>`.
The options lifetime represents the mutable callback borrow. Entry points
without options use `'static` for that parameter because they hold no callback.
For example, these are `QueryCursor` methods:

```rust
pub fn execute<'cursor, 'query, 'tree, Provider, Chunk>(
    &'cursor mut self,
    query: &'query Query,
    root: Node<'tree>,
    text_provider: Provider,
) -> QueryExecution<'cursor, 'query, 'tree, 'static, Provider, Chunk>
where
    Provider: TextProvider<Chunk>,
    Chunk: AsRef<[u8]>;

pub fn execute_with_options<'cursor, 'query, 'tree, 'options, Provider, Chunk>(
    &'cursor mut self,
    query: &'query Query,
    root: Node<'tree>,
    text_provider: Provider,
    options: QueryCursorOptions<'options>,
) -> QueryExecution<'cursor, 'query, 'tree, 'options, Provider, Chunk>
where
    Provider: TextProvider<Chunk>,
    Chunk: AsRef<[u8]>;
```

`matches` and `captures` take the same arguments and return `QueryMatches` and
`QueryCaptures`, respectively; their `_with_options` variants add the same options
argument and lifetime. Source borrows are represented by the provider and chunk
types, independently of the tree and callback borrows. Captures borrow nodes from
the tree, not text from the provider.

Built-in text predicates evaluate the concatenation of all chunks for each
capture. Equality, membership, and regex results are independent of chunking,
including regex matches spanning chunk boundaries. Negation and repeated-capture
quantifiers apply to complete capture texts. General predicates remain available
for host evaluation.

A single chunk is borrowed directly. Multiple chunks are assembled in reusable
execution buffers when contiguous text is needed. An empty chunk iterator
represents empty text; empty chunks do not alter the result. Callers need not
flatten a noncontiguous source before executing a query.

## Query ranges and limits

```rust
impl QueryCursor {
    pub fn set_match_limit(&mut self, limit: u32);
    pub fn did_exceed_match_limit(&self) -> bool;
    pub fn match_limit(&self) -> u32;
    pub fn set_byte_range(&mut self, range: Range<usize>) -> &mut Self;
    pub fn set_point_range(&mut self, range: Range<Point>) -> &mut Self;
    pub fn set_max_start_depth(&mut self, depth: Option<u32>) -> &mut Self;

    pub fn set_optimized(&mut self, enabled: bool);
}
let options = QueryCursorOptions::new().progress_callback(&mut progress);
let matches = cursor.matches_with_options(&query, root, text_provider, options);
// captures_with_options and execute_with_options use the same options interface
```

- Ranges, maximum start depth, and match limits persist on the cursor, following
  tree-sitter. Progress callbacks belong to per-execution `QueryCursorOptions`.
- Cursor range setters return `&mut Self` for chaining. A zero end means
  unbounded; reversed ranges leave the stored range unchanged. Coordinates
  narrow with `as u32` before validation, matching tree-sitter's Rust wrapper.
  Squatter-only scan APIs retain their wider-coordinate behavior.
- `None` removes the maximum start depth. Match limits bound in-progress-match
  capacity, not the number of results. Discovery and eviction order can retain
  a different valid subset from tree-sitter.
- `QueryMatches` and `QueryCaptures` provide `set_byte_range(Range<usize>)` and
  `set_point_range(Range<Point>)`. These methods return `()`, as tree-sitter's
  iterator setters do, and update the borrowed cursor's stored ranges. Their
  validation and narrowing rules match the cursor setters.
- Ordinary ranges select intersecting nodes. Optimized root seeking respects
  range traversal boundaries while active matches may finish outside the range.
  Branching, rootless patterns, and parse errors are supported.
- Without point data, nodes use row zero and byte offsets as columns, including
  in point-dependent queries. Missing presence caches do not change results.

## Progress and cancellation

Progress uses query-specific state and tree-sitter's Rust callback shape:

```rust
pub struct QueryCursorState { /* private representation */ }
impl QueryCursorState {
    pub const fn current_byte_offset(&self) -> usize;
}

#[derive(Default)]
pub struct QueryCursorOptions<'options> {
    pub progress_callback:
        Option<&'options mut dyn FnMut(&QueryCursorState) -> std::ops::ControlFlow<()>>,
}
impl<'options> QueryCursorOptions<'options> {
    pub fn new() -> Self;
    pub fn progress_callback<Callback>(self, callback: &'options mut Callback) -> Self
    where
        Callback: FnMut(&QueryCursorState) -> std::ops::ControlFlow<()>;
    pub fn reborrow(&mut self) -> QueryCursorOptions<'_>;
}
```

`Continue(())` continues, `Break(())` requests a pause, and `None` disables
callbacks. `set_timeout` has been removed; callbacks can capture a deadline or
atomic flag. An execution retains the options and callback borrow through
cancellation and resumption. `reborrow()` permits sequential reuse of options; the earlier
execution must be dropped before reborrowing them.

Searches poll even without results, including in long state-work loops. Byte
positions are decoded only when invoking the callback. Optimized scans report
their local position; shared capture bookkeeping uses the node being processed.
The position is not a monotonic work counter.

Polling uses an internal threshold of 100 operations. Bookkeeping remains when
callbacks are absent. This threshold has not been benchmarked or tuned; callback
cost measurements were skipped. There is no public `progress_stride`.

Callback cancellation is resumable, matching this tree-sitter checkout:

- Callback cancellation preserves traversal state, in-progress matches, and
  capture consumption state. It does not mark execution permanently halted.
- After a cancellation returns `None`, calling `next()` again on the same
  `QueryMatches` or `QueryCaptures` resumes execution. The same rule applies to
  `QueryExecution::next_match` and `next_capture`; these streams are not fused.
- A stop request lets the current node transition or capture bookkeeping finish
  before yielding. Callback cadence and work completed before cancellation can
  differ from tree-sitter.
- The callback remains installed. Advancement may do work and return results
  before polling it again, even if it continues to return `Break(())`. Ready
  captures can also be returned before iteration reports the stop.
- Creating a new iterator through `matches`/`captures` or their options variants
  starts a fresh execution. The explicit `execute` API likewise starts afresh.
- There is no query cancellation-status accessor, as in tree-sitter. `None`
  alone does not distinguish cancellation from exhaustion; callers that need
  this distinction can track whether their progress callback requested a stop.

Tree-sitter implements this resumption behavior in
[`ts_query_cursor__advance`](lib/src/query.c) and the Rust iterators'
[`advance` methods](lib/binding_rust/lib.rs), though its API documentation does
not explicitly guarantee resumption.

## Verification

The [query execution tests](crates/squatter/tests/query_execution.rs) cover:

- Metadata, disabled patterns/captures, independent clones, and compilation
  diagnostics. Predicate diagnostics compare every error field and `Display`
  output across operator variants, invalid arguments, and multiline patterns.
- General and optimized execution, including parse errors, branching, rootless
  patterns, and missing optional side data. Differential tests compare completed
  matches and duplicates while normalizing representation-specific identities.
  Capture tests check completed-capture coverage without requiring tree-sitter's
  event order, provisional snapshots, or multiplicity.
- Range boundaries, zero-end, empty, reversed, and wider-coordinate inputs,
  structural context outside the query range, and persistent iterator settings.
- Byte-slice and chunked providers across all entry points: equality, membership,
  regex matches across chunk boundaries, splits inside UTF-8 sequences, empty
  text/chunks, and repeated captures.
- Removal with readable borrowed captures, repeated removal, explicit removal by
  ID, copied nodes, provider ownership, and cursor reuse after execution drops.
- Resuming the same execution after repeated stops, preserving in-progress
  matches, capture consumption, and result sequence; searches without results;
  fresh cursor reuse; and progress offsets after optimized capture traversal
  enters later subtrees.

[Boundary tests](crates/squatter/tests/boundary.rs) also check compiler metadata
and mutation, and [storage tests](crates/squatter/tests/storage.rs) cover point attachment.
Compile-fail examples in [query.rs](crates/squatter/src/query.rs) and
[query_exec.rs](crates/squatter/src/query_exec.rs) check result, provider, and
callback borrows. Live results prevent advancement and cursor reuse; copied
nodes retain only their tree lifetime.

Run the suite, including borrowing doctests, with `cargo test -p tree-squatter`.

## Deferred work

- Containing-range setters, which require every matched node to be wholly inside
  a supplied range independently of ordinary intersection ranges.
- Strict source ordering, full provisional snapshots, and source-sorted flattened
  completed matches. The retained capture-event contract does not promise these.
- Callback cost measurements and threshold tuning. No performance benefit is
  claimed for the current polling threshold.

Dedicated finite-limit eviction/result-subset and attached-point parity audits
remain outside this revamp.
