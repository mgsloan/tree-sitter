# Query revamp

This working document collects the query API comparison, retained behavior,
selection design, and open scope questions. Query work is separate from the
navigation/storage API pass in [API differences to fix](api-differences-to-fix.md).
Outlines describe proposed APIs, not implemented functionality.

**TODO: consolidate, integrate, and clean up this document before implementation.**
The compatibility sketches and selection design are not yet one coherent API.
Resolve their node-versus-selection arguments, persistent cursor setters versus
per-execution options, range semantics, and bounded-query scope. Merge duplicate
behavior requirements, update all signatures/examples together, and distinguish
accepted changes from deferred work and open questions. Preserve the existing
selection design while making these decisions; the discussion notes below do
not silently select a different design.

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

- `Query::new` accepts the prepared `Language` wrapper;
  `tree_sitter_language()` exposes the underlying tree-sitter language.
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
- Bounded branching/rootless query support remains an open scope decision; see
  the discussion below. The current restriction also applies to general execution,
  so disabling specialized plans is not an existing fallback.
- Add progress options and compatible cancellation/resumption behavior. Remove
  `set_timeout`; callers can check a deadline in the progress callback. Poll
  during execution so a long search with no results can still be cancelled.
  Explicit status and optimization control remain additions.
- Preserve current finite-limit execution behavior; no dedicated eviction or
  result-subset compatibility audit is planned.

## Shared scan selection for query execution

Keep the existing `Scan<'forest, S>`, traversal structs, `Restricted<S, P>`,
`Selection<C, R>`, and `Filtered<S, P>` as the selection builders. Direct scans
continue to use their generic layering: store only selected operations, preserve
fixed-size ID-array specialization, and avoid runtime checks for absent filters.
Do not replace these structs with a concrete selection stored in every scan.

Add a trait that describes a supported scan using one uniform struct at query
initialization. Call the descriptor `ScanSelection` to distinguish it from the
existing coordinate/relation `Selection<C, R>`. Illustrative private fields:

```rust
pub struct ScanSelection<'forest, 'filters> {
    scope: SelectionScope<'forest>,
    restrictions: SmallVec<[CandidateRestriction<'filters>; 2]>,
}

enum SelectionScope<'forest> {
    Subtree(Node<'forest>),
    Trees {
        forest: &'forest ForestData,
        trees: Range<TreeIx>,
    },
}

pub trait DescribeSelection<'forest>: sealed::DescribeSelection {
    fn selection(&self) -> ScanSelection<'forest, '_>;
}
```

`CandidateRestriction` describes the supported byte/point relations and node
filters already represented by scan layers: kind IDs, field IDs, supertype IDs,
extra, and missing. Copy small scalar parameters and borrow filter storage where
possible. The descriptor must preserve every restriction, including repeated
filters as intersections. Inline capacity is an implementation choice; longer
compositions may spill. Unsupported combinations must not implement the trait;
do not silently omit a restriction or replace an earlier one.

Implement the trait on supported `Scan` compositions by describing their source
and accumulated restrictions. A `Node` can describe its unrestricted subtree;
forest and region scopes can describe their tree intervals. The forest extension
can therefore use the same query methods without separate region/source variants.
All selected trees must use the query's language. Arbitrary tree sets and unions
of overlapping subtrees are later extensions, not required by the initial scope.

Query entry points borrow the description provider, normalize the selection once,
and retain only the uniform descriptor and prepared execution state. Borrowing
allows the descriptor to refer to ID arrays owned by scan layers without copying
or retaining a generic scan inside the query iterator. For example:

```rust
let selected = root.all()
    .overlapping_bytes(viewport)
    .filter_kind_ids([call_kind]);

{
    let mut matches = cursor.matches(&query, &selected, text_provider);
    while let Some(found) = matches.next() {
        // consume query results
    }
}

// Direct scanning still uses the original specialized pipeline.
for node in selected.nodes() {
    // consume selected nodes
}
```

The query signature is generic only over description construction and the text
provider; its return type does not depend on the scan's layered type:

```rust
pub fn matches<'cursor, 'query, 'forest, 'filters, S, T, I>(
    &'cursor mut self,
    query: &'query Query,
    selection: &'filters S,
    text_provider: T,
) -> QueryMatches<'cursor, 'query, 'forest, 'filters, T, I>
where
    S: DescribeSelection<'forest>,
    T: TextProvider<I>,
    I: AsRef<[u8]>;
```

Apply the same input shape to `captures`, `execute`, and their options variants.
These are follow-on selection APIs; reconcile the borrowed argument with the
by-value node argument in the compatibility sketches above when implementing
this extension. Keep cancellation and execution limits in execution options.
Selection restrictions belong to the individual execution rather than persistent
cursor state. Existing compatibility range setters, if retained, need an explicit
composition rule; they must not silently override restrictions supplied by a scan.

The trait describes the original selection, not a partially consumed iterator.
Do not implement it for `Nodes`, `Groups`, or partially advanced traversal state.
Traversal direction controls direct scan enumeration, not query result ordering;
initially omit implementations for reversed scans rather than silently ignoring
the requested direction. Preserve the query result behavior described above.

Structural scope and candidate restrictions have different meanings. A subtree
scope bounds structural matching; a tree range supplies independent tree scopes.
Restrictions select eligible query-start nodes, while structural matching may
inspect other nodes within the same scope. Filtering candidates to call nodes
must still allow matching their identifier and argument children. Define the
start-node rule for sibling-sequence and rootless patterns before supporting
those combinations; use a correct fallback when specialized scanning cannot
implement it. Candidate restrictions do not implicitly acquire Tree-sitter's
query-range semantics or require every capture to satisfy the restriction.

For source-sorted injections, the client can use its source index to select a
contiguous tree interval before applying viewport restrictions. Queries and direct
scans should both use those bounds to skip irrelevant trees/groups. Sorted starts
alone do not justify excluding earlier trees when injections overlap or nest;
the index must account for ends. No shared coordinate frame or source ordering
is inferred from region membership.

Generic description methods should inline well, but do not rely on the concrete
descriptor disappearing. A shared query engine may retain optional/tag checks,
and storing the descriptor retains its full representation. Perform conversion
once per execution and prepare scan state outside the per-node matching loop.
Keep the structural matcher independent of the builder type to limit code growth.

Verify that description preserves scopes and all supported restrictions, including
empty selections and repeated filters. Compare direct-scan candidate sets with
those used by query execution, and verify structural context remains available
outside the candidate set. Cover subtree/tree boundaries, borrowed filter
lifetimes, and source-index-selected injection ranges.

## Behavior and verification carried from the API plan

- **Capture events:** order, provisional snapshot contents, and duplicate counts
  can differ. Preserve these differences in the new `captures` API and document
  them; retain the current completed-capture coverage contract.
- **Bounded queries:** branching/rootless patterns can report `UnsupportedRange`
  even with optimization disabled. Whether to expand support in this revamp is
  unresolved; the earlier proposal understated this as an optimizer fallback.
- **Property metadata:** `set!` and `is?`/`is-not?` currently appear among general
  predicates. Move them to the corresponding dedicated metadata interfaces.
- **Range validation:** both implementations reject reversed ranges without
  changing the stored range, after interpreting a zero end as unbounded.
  Currently tree-squatter also rejects values outside `u32`. Change shared
  query coordinate-taking APIs to
  cast byte offsets and point components with `as u32` wherever tree-sitter's
  Rust wrapper does. Do not check narrowing conversions. Range validity checks
  apply after conversion; setters discard the internal success flag. Retain
  existing wider-coordinate behavior in squatter-only scan APIs.
- **Finite match limits:** discovery/eviction order can retain a different valid
  subset. Retain and document this behavior; no dedicated compatibility audit
  is planned.
- **Cancellation:** the current API uses timeout/status instead of Rust callbacks.
  Replace `set_timeout` with callback support and compatible stopping/resumption
  behavior; exact callback cadence and work completed before cancellation may
  differ. Deadline cancellation belongs in the callback.

Optional point data keeps its existing row-zero, byte-as-column behavior when
absent, including in point-dependent queries. No separate attached-point parity
audit is planned. Missing presence caches must not change results.

Use existing query differential tests, normalizing newtypes and representation-
specific identities. Preserve contractual ordering and duplicates for completed
results. For capture events, retain coverage checks without requiring
Tree-sitter's event order, snapshots, or multiplicity. Cover range boundaries,
optional side data, cancellation, and optimized/unoptimized execution as relevant
to changes. No dedicated finite-limit eviction/subset audit is planned.

Apply the documentation-copy and annotation approach from the API plan to APIs
actually implemented here. Do not copy Tree-sitter ordering guarantees that the
retained capture contract does not provide.

## Questions from the API scope discussion

### Capture iteration: settled behavior, separate ordering feature

Add `captures(...)` while preserving current ordering, provisional snapshots,
and duplicate behavior. Do not require exact Tree-sitter capture-event parity or
add a separately named provisional-event API. Retain completed-capture coverage.

Strict source order would be a separate feature. A focused probe found decreasing
capture start positions in both squatter and this checkout's Tree-sitter for
captured wildcard parents discovered through later children. Tree-sitter's docs
promise ordered captures, but its implementation did not meet that promise in
this case. See item 6 in
[potential upstream bugs](/home/mgsloan/oss/tree-sitter/potential-upstream-bugs.md).
Tree-sitter also emits snapshots from unfinished matches; full snapshots are not
a prerequisite for compatibility. Global capture ordering does not apply to
flattening the results of completed-match iteration.

### Ordinary bounded queries versus containing ranges

Current query range eligibility requires rooted patterns and compiled steps
without alternative branches. Optional parts, repetitions, and alternations can
introduce branches; sibling-sequence patterns can be non-rooted. The gate rejects
both match and capture execution, including the unoptimized path. The provenance
notes say the restriction remains pending range validation; actual fixes required
are not yet established.

Do we retain that restriction initially or validate and extend general execution
as part of this revamp? Deferral was suggested but not decided. The selection
design's start-node restrictions are a distinct contract and do not answer the
ordinary Tree-sitter-range compatibility question.

Query containing-range setters are explicitly deferred. They require matched
nodes to be wholly inside a supplied range and can be combined with ordinary
intersection ranges. Existing scan `within_*` selects nodes inside a range;
scan `containing_*` selects nodes enclosing it. Neither by itself supplies
Tree-sitter's whole-match containment semantics. Keep the existing scan APIs.

### Metadata, diagnostics, and cloning scope

Which proposed additions belong in this revamp: capture quantifiers, pattern byte
bounds and classification, guarantee inspection, richer compilation diagnostics,
property metadata, and `Query::deep_clone`? The comparison lists targets, but the
selection design does not settle their priority or implementation cost. Separating
`set!` and `is?`/`is-not?` from general predicates changes observable metadata.
Deep cloning must preserve disabled patterns and captures; it is more than an
accessor rename. Pattern-index types remain an explicit open question above.

### Text-provider scope

Is noncontiguous text support included initially, or should the first iteration
retain byte-slice input? The sketches assume Tree-sitter-shaped `TextProvider`
support, while the current executor evaluates predicates against source bytes.
The selection design retains a text-provider parameter but does not specify its
implementation. Work out predicate chunk handling and node/source lifetimes;
do not describe a byte-slice-only implementation as full provider compatibility.

### Execution options, limits, and progress

The current cursor already has `set_match_limit(u32)` and
`did_exceed_match_limit() -> bool`; the comparison adds the missing getter.
The flag reports exhausted in-progress-match capacity, not a result-count limit.
Retain current eviction/result-subset behavior. Decide how the setter, getter,
and status map to the selection design's per-execution limits without adding a
new behavioral audit.

Use query-specific progress state, not generic cross-API progress types.
Tree-sitter's Rust shape is:

```rust
pub struct QueryCursorState { /* private representation */ }
impl QueryCursorState {
    pub const fn current_byte_offset(&self) -> usize;
}

#[derive(Default)]
pub struct QueryCursorOptions<'a> {
    pub progress_callback:
        Option<&'a mut dyn FnMut(&QueryCursorState) -> std::ops::ControlFlow<()>>,
}
```

`Continue(())` continues; `Break(())` cancels. `None` disables callbacks.
Remove the dedicated timeout setting; callers can capture a deadline or atomic
flag. Byte position is cheap to obtain from packed storage. Decode it only when
invoking the callback, and pass optimized scans' local positions to polling rather
than reading a stale general cursor. Position is not a monotonic work counter.

Internal throttling is useful; making `progress_stride` public and choosing its
units/default remain open. Tree-sitter uses internal thresholds of 100 operations,
not a public stride. An optional callback already represents disabled callbacks;
no special `u32::MAX` meaning is needed. Bookkeeping need not disappear when the
callback is absent. Measure costs before choosing defaults or claiming a benefit.

Specify cancellation and resumption separately: can advancement resume after a
callback stops execution, or must the caller begin another execution? Existing
cancellation tests cover stopping and fresh reuse, not a complete resumption
contract. Poll during searches without results and account for long state-work
loops. Exact callback cadence and partial results need not match Tree-sitter.
