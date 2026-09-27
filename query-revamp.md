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
    pub fn pattern_count(&self) -> usize;
    pub fn disable_pattern(&mut self, index: usize);
    pub fn disable_capture(&mut self, name: &str);
    pub const fn capture_names(&self) -> &[&str];
    pub fn capture_index_for_name(&self, name: &str) -> Option<CaptureIx>;
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

- Return borrowed string slices rather than exposing internal string ownership.
- Add capture, pattern, and property inspection; reuse tree-sitter metadata types
  where their shape is unchanged. Property/predicate types need `CaptureIx`.
- Put `set!` in settings and `is?`/`is-not?` in property predicates. Exclude those
  operators from general predicates, preserving host evaluation responsibilities.
- Restore full compilation diagnostics and clone enabled-pattern/capture state.

## Query capture indices and result types

Use `CaptureIx(u32)` for query-global capture-name indices, `MatchId(u32)`
for match identity, and `MatchCaptureIx(u32)` for positions within a match's
capture slice. These are separate domains, even when their values coincide.

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
impl QueryExecution<'_, '_, '_, '_> {
    pub fn next_match(&mut self) -> Option<QueryMatch<'_, 'tree>>;
    pub fn next_capture(&mut self) -> Option<(QueryMatch<'_, 'tree>, usize)>;
    pub fn remove_match(&mut self, id: u32);
}
// general_predicates exposes tree_sitter::QueryPredicate and QueryPredicateArg.
// No property metadata API; internal CaptureId(u32) is private.
```

**Proposed tree-squatter**

```rust
pub struct CaptureIx(pub u32);
pub struct MatchId(pub u32);
pub struct MatchCaptureIx(pub u32);

pub struct QueryCapture<'tree> {
    pub node: Node<'tree>,
    pub index: CaptureIx,
}
pub struct QueryMatch<'cursor, 'tree> {
    pub pattern_index: usize,
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
impl<'tree> QueryExecution<'_, '_, 'tree, '_> {
    pub fn next_match(&mut self) -> Option<QueryMatch<'_, 'tree>>;
    pub fn next_capture(&mut self) -> Option<(QueryMatch<'_, 'tree>, MatchCaptureIx)>;
    pub fn remove_match(&mut self, id: MatchId);
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
- `capture_names()[capture_id]` and
  `capture_quantifiers(pattern_index)[capture_id]` use that same domain. A
  newtype needs explicit conversion for slice indexing or typed accessors.
  Pattern-index arguments and `pattern_index` remain a separate decision.
- Use `MatchCaptureIx` for capture-event positions in both iteration APIs.
  Convert its value to `usize` when indexing the match's capture slice. Repeated captures can have the same name ID and different
  positions; `nodes_for_capture_index` returns all occurrences of that name.
- Keep all three wrappers backed by `u32`. `CaptureIx` preserves zero-based
  query-global indices without offset encoding. Tree-sitter returns match-local
  positions as `u32`, although its match capture count is `u16`; do not adopt
  that narrower count limit. Repetitions can yield many occurrences of one name.
- Rename/unify the private `CaptureId` with `CaptureIx`; do not introduce two
  types for the same domain. Expose `MatchId` consistently through
  `QueryMatch::id()` and `QueryExecution::remove_match()`.
- Define squatter-owned property/predicate types to carry `CaptureIx`;
  tree-sitter's types cannot carry it. Keep their remaining shape unchanged.

## Query iteration and match access

**Current tree-sitter**

```rust
use tree_sitter::StreamingIterator;

let mut matches = cursor.matches(&query, root, text_provider);
while let Some(found) = matches.next() {
    let captures = found.captures();
    found.remove(); // optional: suppress subsequent results for this match
}
let mut captures = cursor.captures(&query, root, text_provider);
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
// alternative advancement on an execution
while let Some((found, index)) = execution.next_capture() {
    let capture = found.captures[index]; // provisional, unspecified event order
}
let error = execution.error();
let cancelled = execution.did_cancel();
```

**Proposed tree-squatter**

```rust
use tree_squatter::StreamingIterator;

let mut matches = cursor.matches(&query, root, text_provider);
while let Some(found) = matches.next() {
    let captures = found.captures();
    found.remove();
}
let mut captures = cursor.captures(&query, root, text_provider);
while let Some((found, index)) = captures.next() {
    let capture = found.captures()[index.0 as usize];
}
// execute and explicit status reporting remain additional capabilities
```

- Add `matches`/`captures` streaming iterators and a text-provider abstraction
  with the same shape, accepting packed nodes. Providers for noncontiguous text
  must remain possible; byte slices remain a convenient implementation.
- Add `QueryMatch::captures()` and `remove()` with compatible borrowing and
  removal behavior. Preserve capture/node lifetimes while adapting the executor.
- Preserve current capture ordering, provisional snapshot contents, and duplicate
  behavior in `captures`, adapting `next_capture` without changing its execution
  semantics. Document the differences from tree-sitter; no separately named
  provisional-event API or strict source-order guarantee is required. Preserve
  coverage of completed-match captures.
- Keep explicit execution diagnostics without making valid queries fail because
  an optimization is unavailable.
- The snippets show independent iteration modes. Drop an iterator before borrowing
  its cursor again, and supply each iterator with its own text-provider value.

## Query ranges, limits, and cancellation

**Current tree-sitter**

```rust
impl QueryCursor {
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
    pub fn set_byte_range(&mut self, range: Range<usize>) -> bool;
    pub fn set_point_range(&mut self, range: Range<Point>) -> bool;
    pub fn set_max_start_depth(&mut self, depth: u32);
    pub fn set_timeout(&mut self, timeout: Option<Duration>);
    pub fn set_optimized(&mut self, enabled: bool);
}
// no match_limit getter, containing-range setters, or progress options
// bounded branching/rootless queries can report UnsupportedRange
```

**Proposed tree-squatter**

```rust
impl QueryCursor {
    pub fn match_limit(&self) -> u32;
    pub fn set_byte_range(&mut self, range: Range<usize>) -> &mut Self;
    pub fn set_point_range(&mut self, range: Range<Point>) -> &mut Self;
    pub fn set_max_start_depth(&mut self, depth: Option<u32>) -> &mut Self;

    pub fn set_optimized(&mut self, enabled: bool);
}
let options = QueryCursorOptions::new().progress_callback(&mut progress);
let matches = cursor.matches_with_options(&query, root, text_provider, options);
// captures_with_options uses the same options interface
```

- Follow tree-sitter's Rust wrapper: range setters return `&mut Self` for chaining
  and discard the internal acceptance result. Rejected ranges leave the stored
  range unchanged. Do not add separate `try_set_*` methods. Use `None` to remove
  the depth limit. Narrow coordinates with `as u32`, matching tree-sitter's Rust
  wrapper rather than rejecting values that do not fit.
- Add the limit getter and range setters on result iterators.
- Defer query containing-range setters. They require new query filtering behavior
  beyond this API-matching pass. Retain existing scan containment APIs unchanged.
- Support bounded branching/rootless queries through a compatible fallback when
  a specialized plan is ineligible.
- Add progress options and compatible cancellation/resumption behavior. Remove
  `set_timeout`; callers can check a deadline in the progress callback. Poll
  during execution so a long search with no results can still be cancelled.
  Explicit status and optimization control remain additions.
- Preserve current finite-limit execution behavior; no dedicated eviction or
  result-subset compatibility audit is planned.
