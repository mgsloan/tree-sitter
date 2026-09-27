# Query revamp

This document describes the proposed query API and retained behavior. Follow
tree-sitter's API with the additions and differences specified below. Query work
is separate from the navigation/storage API pass. Proposed API outlines describe
planned functionality; current behavior and completed work are identified below.

The planned work covers metadata and diagnostics, public index types, text
providers, streaming iterators, and resumable progress callbacks. Ordinary
bounded queries are already supported in general and optimized execution,
including trees with parse errors. Query containing-range setters remain deferred;
existing scan APIs are outside this work.

## Query compilation, metadata, and errors

**Current tree-sitter**

```rust
impl Query {
    pub fn pattern_count(&self) -> usize;
    pub fn disable_pattern(&mut self, index: usize);
    pub fn disable_capture(&mut self, name: &str);
    pub const fn capture_names(&self) -> &[&str];
    pub fn capture_index_for_name(&self, name: &str) -> Option<u32>;
    pub const fn capture_quantifiers(&self, index: usize) -> &[CaptureQuantifier];
    pub const fn property_settings(&self, index: usize) -> &[QueryProperty];
    pub const fn property_predicates(&self, index: usize) -> &[(QueryProperty, bool)];
    pub const fn general_predicates(&self, index: usize) -> &[QueryPredicate];
    pub fn start_byte_for_pattern(&self, index: usize) -> usize;
    pub fn end_byte_for_pattern(&self, index: usize) -> usize;
    pub fn is_pattern_rooted(&self, index: usize) -> bool;
    pub fn is_pattern_non_local(&self, index: usize) -> bool;
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

**Current tree-squatter**

```rust
impl Query {
    pub fn pattern_count(&self) -> usize;
    pub fn disable_pattern(&mut self, index: usize);
    pub fn disable_capture(&mut self, name: &str);
    pub fn capture_names(&self) -> &[String];
    pub fn general_predicates(&self, pattern: usize) -> &[tree_sitter::QueryPredicate];
    // other inspection methods above and deep_clone are absent
}
pub struct QueryError {
    pub offset: usize,
    pub message: String,
}
```

**Proposed tree-squatter**

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
- Return borrowed string slices rather than exposing internal string ownership.
- Add capture, pattern, and property inspection; reuse tree-sitter metadata types
  where their shape is unchanged. Property/predicate types need `CaptureIx`.
- Put `set!` in settings and `is?`/`is-not?` in property predicates. Exclude those
  operators from general predicates, preserving host evaluation responsibilities.
- Restore full compilation diagnostics and clone enabled-pattern/capture state.

## Query indices and result types

Use `PatternIx(usize)` for query-global pattern indices, `CaptureIx(u32)` for
query-global capture-name indices, `MatchId(u32)` for match identity, and
`MatchCaptureIx(u32)` for positions within a match's capture slice. These are
separate domains, even when their values coincide.

A pattern index is the zero-based position of a top-level pattern in the compiled
query. Each match identifies the pattern that produced it; many matches can share
one pattern index.

**Current tree-sitter**

```rust
pub struct QueryCapture<'tree> {
    pub node: Node<'tree>,
    pub index: u32, // query-wide capture-name ID
}
pub struct QueryMatch<'cursor, 'tree> {
    pub pattern_index: usize,
    // private captures, match ID, and cursor
}
impl<'tree> QueryMatch<'_, 'tree> {
    pub const fn id(&self) -> u32;
    pub const fn captures(&self) -> &[QueryCapture<'tree>];
    pub fn remove(&self);
    pub fn nodes_for_capture_index(&self, capture_ix: u32)
        -> impl Iterator<Item = Node<'tree>> + '_;
}
pub struct QueryProperty {
    pub key: Box<str>,
    pub value: Option<Box<str>>,
    pub capture_id: Option<usize>, // also a query-wide capture-name ID
}
impl QueryProperty {
    pub fn new(key: &str, value: Option<&str>, capture_id: Option<usize>) -> Self;
}
pub enum QueryPredicateArg {
    Capture(u32), // query-wide capture-name ID
    String(Box<str>),
}
pub struct QueryPredicate {
    pub operator: Box<str>,
    pub args: Box<[QueryPredicateArg]>,
}
// StreamingIterator associated item types, with generics omitted:
// QueryMatches::Item = QueryMatch<'cursor, 'tree>
// QueryCaptures::Item = (QueryMatch<'cursor, 'tree>, usize)
// The tuple index selects an occurrence in found.captures().
```

**Current tree-squatter**

```rust
pub struct QueryCapture<'tree> {
    pub node: Node<'tree>,
    pub index: u32,
}
pub struct QueryMatch<'cursor, 'tree> {
    pub id: u32,
    pub pattern_index: usize,
    pub captures: &'cursor [QueryCapture<'tree>],
}
impl<'tree> QueryMatch<'_, 'tree> {
    pub fn nodes_for_capture_index(&self, index: u32)
        -> impl Iterator<Item = Node<'tree>> + '_;
}
impl<'tree> QueryExecution<'_, '_, 'tree, '_> {
    pub fn next_match(&mut self) -> Option<QueryMatch<'_, 'tree>>;
    pub fn next_capture(&mut self) -> Option<(QueryMatch<'_, 'tree>, usize)>;
    pub fn remove_match(&mut self, id: u32);
}
// general_predicates exposes tree_sitter::QueryPredicate and QueryPredicateArg.
// No property metadata API; internal CaptureId(u32) is private.
```

**Proposed tree-squatter**

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

- Add match accessors and removal through the match, retaining the explicit
  executor interface. A match ID identifies an execution result; it is neither
  a pattern index nor a capture-name ID.
- Use `CaptureIx` consistently for `QueryCapture::index`,
  `Query::capture_index_for_name`, `QueryMatch::nodes_for_capture_index`,
  `QueryPredicateArg::Capture`, and `QueryProperty::capture_id` (including its
  constructor). The last currently uses `usize` in tree-sitter despite referring
  to the same capture-name domain.
- Index capture-name and quantifier slices with `capture_ix.0 as usize`:
  `query.capture_names()[capture_ix.0 as usize]` and
  `query.capture_quantifiers(pattern_ix)[capture_ix.0 as usize]`. Pattern
  arguments use `PatternIx` directly; slice positions use the inner integer.
- Use `PatternIx` for `QueryMatch::pattern_index` and all pattern-index arguments
  on `Query`, including `disable_pattern` and per-pattern metadata accessors.
  Keep its backing type `usize`. Counts and query-source byte offsets remain
  `usize`; `is_pattern_guaranteed_at_step` takes a byte offset, not a pattern index.
- Use `MatchCaptureIx` for capture-event positions in both iteration APIs.
  Convert its value to `usize` when indexing the match's capture slice. Repeated
  captures can have the same name ID and different positions;
  `nodes_for_capture_index` returns all occurrences of that name.
- Keep the capture and match wrappers backed by `u32`. `CaptureIx` preserves
  zero-based query-global indices without offset encoding. Tree-sitter returns
  match-local positions as `u32`, although its match capture count is `u16`;
  do not adopt that narrower count limit. Repetitions can yield many occurrences
  of one name.
- Rename the private `CaptureId` to `CaptureIx`; use one type for that domain.
  Expose `MatchId` consistently through `QueryMatch::id()` and
  `QueryExecution::remove_match()`.
- Define squatter-owned property/predicate types to carry `CaptureIx`;
  tree-sitter's types cannot carry it. Keep their remaining shape unchanged.

## Query iteration and match access

**Current tree-sitter**

```rust
use tree_sitter::StreamingIterator;

let mut matches = cursor.matches(&query, root, source_bytes);
while let Some(found) = matches.next() {
    let captures = found.captures();
    found.remove(); // optional: suppress subsequent results for this match
}
drop(matches);

let mut captures = cursor.captures(&query, root, source_bytes);
while let Some((found, index)) = captures.next() {
    let capture = found.captures()[*index];
}
```

**Current tree-squatter**

```rust
let mut execution = cursor.execute(&query, root, source_bytes);
while let Some(found) = execution.next_match() {
    let captures = found.captures;
    let id = found.id;
    execution.remove_match(id);
}
drop(execution);

let mut execution = cursor.execute(&query, root, source_bytes);
while let Some((found, index)) = execution.next_capture() {
    let capture = found.captures[index]; // provisional, unspecified event order
}
let error = execution.error();
```

**Proposed tree-squatter**

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
// execute and explicit execution errors remain additional capabilities
```

- Accept a root `Node` by value in `matches`, `captures`, `execute`, and their
  options variants, following tree-sitter's query entry points.
- Add `matches`/`captures` streaming iterators using the text-provider interface
  below, shared with `execute`.
- Preserve current capture ordering, provisional snapshot contents, and duplicate
  behavior in `captures`, adapting `next_capture` without changing its execution
  semantics. Document the differences from tree-sitter; no separately named
  provisional-event API or strict source-order guarantee is required. Preserve
  coverage of completed-match captures.
- Retain `QueryExecution::error()` for query/node language mismatches.
  Unavailable optimizations must not make valid queries fail.
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
this execution. It must leave the current match and any borrowed capture slice
readable. Record removal through interior-mutable state and apply it before the
next advancement can emit results or reuse capture storage. Repeated removal is
a no-op; already returned results remain valid. Dropping a match without calling
`remove` does not suppress its remaining captures.

`QueryExecution::remove_match(MatchId)` provides the same suppression by ID after
the result borrow ends. Match IDs belong to one execution and may be reused by a
fresh execution. Keep capture storage and removal state private; iterator
adapters must not expose references that outlive the current result borrow.

## Text providers and predicate evaluation

Define a squatter-owned `TextProvider` with tree-sitter's trait shape, accepting
packed `Node` values. Retain the associated iterator name `I` for compatibility.

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

Also support closures returning chunk iterators, as tree-sitter does. Chunks may
be borrowed or owned. Each call supplies the node's complete text in source order;
chunk boundaries have no semantic significance. The byte-slice implementation
indexes source bytes using the node's byte range, without copying.

All query entry points take the provider by value and retain it for execution.
Use the same bounds and lifetime order for `QueryMatches`, `QueryCaptures`, and
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
capture. Equality, membership, and regex results must be independent of chunking,
including regex matches spanning chunk boundaries. Apply negation and repeated-
capture quantifiers to complete capture texts, not individual chunks. Continue
to expose general predicates for host evaluation.

Borrow a single chunk directly when possible. When contiguous text is needed
for multiple chunks, assemble it in reusable execution buffers. An empty chunk
iterator represents empty text; empty chunks do not alter the result. Do not
require callers to flatten a noncontiguous source before executing a query.

## Query ranges, limits, and cancellation

**Current tree-sitter**

```rust
impl QueryCursor {
    pub fn set_match_limit(&mut self, limit: u32);
    pub fn did_exceed_match_limit(&self) -> bool;
    pub fn match_limit(&self) -> u32;
    pub fn set_byte_range(&mut self, range: Range<usize>) -> &mut Self;
    pub fn set_point_range(&mut self, range: Range<Point>) -> &mut Self;
    pub fn set_containing_byte_range(&mut self, range: Range<usize>) -> &mut Self;
    pub fn set_containing_point_range(&mut self, range: Range<Point>) -> &mut Self;
    pub fn set_max_start_depth(&mut self, depth: Option<u32>) -> &mut Self;
}
let options = QueryCursorOptions::new().progress_callback(&mut progress);
let matches = cursor.matches_with_options(&query, root, text_provider, options);
// captures_with_options is also available
```

**Current tree-squatter**

```rust
impl QueryCursor {
    pub fn set_match_limit(&mut self, limit: u32);
    pub fn did_exceed_match_limit(&self) -> bool;
    pub fn set_byte_range(&mut self, range: Range<usize>) -> bool;
    pub fn set_point_range(&mut self, range: Range<Point>) -> bool;
    pub fn set_max_start_depth(&mut self, depth: u32);
    pub fn set_timeout(&mut self, timeout: Option<Duration>);
    pub fn set_optimized(&mut self, enabled: bool);
}
// no match_limit getter, containing-range setters, or progress options
// ordinary ranges support branching and rootless queries
```

**Proposed tree-squatter**

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

- Keep ranges, maximum start depth, and match limits as persistent cursor
  settings, following tree-sitter. Progress callbacks belong to per-execution
  `QueryCursorOptions`.
- Follow tree-sitter's Rust wrapper: range setters return `&mut Self` for chaining
  and discard the internal acceptance result. A zero end means unbounded;
  reversed ranges leave the stored range unchanged. Do not add separate
  `try_set_*` methods. Use `None` to remove the depth limit. Narrow coordinates
  with `as u32`, matching tree-sitter's Rust wrapper rather than rejecting values
  that do not fit. Validate ranges after conversion. Squatter-only scan APIs
  retain their wider-coordinate behavior.
- Add the missing `match_limit` getter on the cursor. Retain `set_match_limit`
  and `did_exceed_match_limit`: the limit bounds in-progress-match capacity,
  not the number of results.
- Add `set_byte_range(Range<usize>)` and `set_point_range(Range<Point>)` to
  `QueryMatches` and `QueryCaptures`. These methods return `()`, as tree-sitter's
  iterator setters do, and update the borrowed cursor's stored ranges. Their
  validation and narrowing rules match the cursor setters.
- Query containing-range setters remain deferred. They require every matched
  node to be wholly inside a supplied range, independently of the ordinary
  intersection range.
- Preserve the implemented bounded-query behavior: optimized root seeking
  respects range traversal boundaries while active matches may finish outside
  the range. Branching, rootless patterns, and parse errors are supported.
- Replace `set_timeout` with the resumable progress-callback interface below.
  Retain explicit execution errors through `QueryExecution::error()` and
  optimization control through `QueryCursor::set_optimized`.
- Preserve and document current finite-limit execution behavior. Discovery and
  eviction order can retain a different valid subset from tree-sitter; no
  dedicated eviction or result-subset compatibility audit is planned.

## Progress and cancellation

Use query-specific progress state and tree-sitter's Rust callback shape:

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

`Continue(())` continues, `Break(())` cancels, and `None` disables callbacks.
Callers can capture a deadline or atomic flag in place of `set_timeout`.
An execution retains the options and callback borrow through cancellation and
resumption. `reborrow()` permits sequential reuse of options; the earlier
execution must be dropped before reborrowing them.

Poll during searches without results, including long state-work loops. Decode
byte positions only when invoking the callback; optimized scans must supply
their local position rather than a stale general cursor position. This position
is not a monotonic work counter.

Exact callback cadence and work completed before cancellation may differ from
tree-sitter. Internal throttling is allowed, and bookkeeping need not disappear
when callbacks are absent. Measure costs before choosing thresholds or claiming
a benefit. A public `progress_stride` is not required by this API; tree-sitter
uses an internal threshold of 100 operations.

Adopt resumable callback cancellation, matching this tree-sitter checkout:

- Callback cancellation preserves traversal state, in-progress matches, and
  capture consumption state. It does not mark execution permanently halted.
- After a cancellation returns `None`, calling `next()` again on the same
  `QueryMatches` or `QueryCaptures` resumes execution. Apply the same rule to
  `QueryExecution::next_match` and `next_capture`. Iterator wrappers must preserve
  this behavior rather than treating the first `None` as terminal.
- The callback remains installed. Advancement may do work and return results
  before polling it again, even if it continues to return `Break(())`. Ready
  captures can also be returned before iteration reports the stop.
- Creating a new iterator through `matches`/`captures` or their options variants
  starts a fresh execution. The explicit `execute` API likewise starts afresh.
- Follow tree-sitter in providing no query cancellation-status accessor. `None`
  alone does not distinguish cancellation from exhaustion; callers that need
  this distinction can track whether their progress callback requested a stop.

Tree-sitter implements this resumption behavior in
[`ts_query_cursor__advance`](lib/src/query.c) and the Rust iterators'
[`advance` methods](lib/binding_rust/lib.rs), though its API documentation does
not explicitly guarantee resumption. Callback cancellation must preserve live
state rather than take squatter's current timeout path, which sets `halted`.

## Verification and documentation

Use existing query differential tests, normalizing newtypes and representation-
specific identities. Preserve contractual ordering and duplicates for completed
matches. For capture events, check completed-capture coverage without requiring
tree-sitter's event order, provisional snapshots, or multiplicity.

Cover range boundaries, cancellation, optional side data, and optimized and
unoptimized execution as relevant to the changes. Absent point data retains the
existing row-zero, byte-as-column behavior, including in point-dependent queries.
Missing presence caches must not change results. No separate attached-point
parity audit is planned.

Check that live result borrows prevent advancement and cursor reuse, copied
nodes retain their tree lifetime, and `remove()` leaves borrowed captures readable
while suppressing subsequent events. Cover repeated removal, explicit removal by
ID, callback/provider lifetimes, and dropping an execution before cursor reuse.

Range tests cover zero-end, empty, reversed, and wider-coordinate inputs,
structural context outside the query range, and subtree boundaries. Cancellation
tests cover resuming the same iterator, preserving in-progress matches and
capture consumption, repeated stops, searches without results, and fresh reuse.

Compare byte-slice and chunked providers across all query entry points. Cover
equality, membership, and regex predicates, with splits inside matching text and
UTF-8 sequences, empty text/chunks, and repeated captures. Predicate results must
remain identical across chunkings.

Copy and annotate tree-sitter API documentation where behavior is shared;
explicitly document retained differences. Do not copy capture-order guarantees
that the retained event contract does not provide. Strict source ordering is
outside this revamp. See item 6 in
[potential upstream bugs](/home/mgsloan/oss/tree-sitter/potential-upstream-bugs.md)
for a counterexample in both implementations. Full provisional snapshots and
source-sorted flattened completed matches are not requirements.

## Implementation order

1. **Metadata and indices:** add the public newtypes, metadata types/accessors,
   compilation diagnostics, and cloning. Update callers and existing tests with
   each API change.
2. **Text providers:** generalize `QueryExecution` and built-in predicates, retain
   byte-slice convenience, and verify chunk-independent results.
3. **Iteration and borrowing:** add match accessors/removal and streaming adapters;
   align cursor and iterator settings while preserving current execution behavior.
4. **Progress callbacks:** add options to all entry points, replace timeouts, and
   verify cancellation, same-execution resumption, and fresh cursor reuse in each
   execution path.

Apply the verification and API documentation requirements as each step lands.
Ordinary bounded-query support is complete; containing-range support remains
outside this implementation sequence.
