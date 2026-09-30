# Pool-based property testing

Proposed integration test: `crates/squatter/tests/pool_proptest.rs`, run by
`cargo test -p tree-squatter --test pool_proptest`. This document describes the
test; it does not implement it. It targets the APIs at main commit `6a6e117a6`.

The reference is
[`shadow_proptest.rs`](../bit-packed/crates/cli/tests/shadow_proptest.rs)
in the bit-packed worktree. Keep its central idea: generate a sequence of
operations over pools of live values, compare results, and shrink the sequence
to a reproducer. Adapt the operations to the current Rust API, especially
packing, side data, storage ownership, scans, and reusable scratch.

## Case structure

A case contains generated documents and a `Vec<Operation>`. Start with one
Tree-sitter parser, one `Packer`, and a paired single-tree JSON forest. Queries
and packed parsers are created by operations. Each forest entry retains source
and Tree-sitter reference trees for every packed root, ordered region grammars,
packing options, and expected side-data state.

The outer pools contain owned values:

| Pool | Values and model facts |
| --- | --- |
| Documents | UTF-8 source and grammar; edits produce new entries |
| Parsers | Tree-sitter parsers, compatible `Parser` values, and restricted `TreeFellerParser` values; backend and grammar |
| Packers | Reusable `Packer` values |
| Forests | `Forest` values with owned or retained core storage; reference roots/sources, stable identity, region order, point state, presence coverage |
| Saved sidecars | Serialized `PointsData`/`PresenceCache` bytes and the layout provenance needed to restore them |
| Queries | Squatter and Tree-sitter queries, source, grammar, disabled patterns/captures |
| Query cursors | Reusable optimized, unoptimized, and Tree-sitter cursors plus configuration |

Use fixed grammar fixtures from `tests/support/mod.rs`: JSON first, then C and
C#. Direct parsing gets an explicit subset of supported grammars and documents;
include bounded Python/Rust fixtures from `tests/parser.rs` and `bindings.rs`
for external scanners and non-terminal extras. Grammar handles can initially be
fixture-owned; testing their destruction is already covered by `bindings.rs`.

`Forest` owns storage; `Tree<'forest>` and `ForestRegion<'forest>` are borrowed
views. `Forest::from_retained` also returns a `Forest`. Start with single-tree
forests, then generate ordered `PackRegion` lists with multiple roots, mixed
grammars, and repeated grammars in separate regions. Check the returned
`Vec<TreeIx>` against flattened input order. Empty forests are valid; empty
regions are explicit negative cases. Use `trees()`/`tree()` for general forests:
the `root_node()`, `walk()`, and `language()` conveniences require one tree.

### Borrowed values

`Operation::Explore(Vec<ViewOperation>)` borrows the live forests and runs a second
operation sequence over pools of paired nodes and actual tree cursors. Seed
these pools with each tree's root and root cursor. Navigation results and cursor
clones grow the pools; explicit drops remove entries. A cursor retains its real
ancestor stack across steps.

All nodes, cursors, and temporary borrowed slab views die when `Explore`
returns. The outer sequence can then compact in place, change side data, or drop
forests. Implement this by splitting borrows of `World`'s fields; the view runner
borrows the forest pool immutably and the query pools mutably. No extended
lifetimes, leaked trees, or self-referential container are needed.

This deliberately tests sequences of ownership operations interleaved with
sequences of borrowed operations. It does not retain a cursor while an unrelated
forest is created or dropped. That is a coverage limitation, but avoids adding
unsafe lifetime management to the harness. Parser/packer reuse still happens
while their earlier output forests remain alive, and destruction really drops
the selected owner.

For `BorrowedForest`, construct views from existing aligned slabs at entry to an
`Explore` block, before creating node/cursor pools. Keep those views in a local
collection that outlives the pools. Model their missing side data separately
from the source forests. Retained forests belong in the outer pool because they
retain their storage owners.

## Operations

Group pure accessors into one operation. Give navigation, configuration, and
ownership changes distinct variants so they shrink independently.

| Family | Operations | Main checks |
| --- | --- | --- |
| Documents | Add fixture/generated source; splice an existing document | New immutable source; UTF-8 boundaries and bounded size |
| Parsing/packing | Create, reuse, `drop_scratch`, and drop parsers/packers; reset compatible parsers; parse and pack forests with varied options | Reference structure; fresh versus reused scratch; earlier forests unchanged |
| Storage | `to_compacted`/`compact`; load copied or retained slab; `detach`; drop forest | Structure and ownership survive; side-data preservation follows each API |
| Side data | Save/reload points created during packing; build selective presence; attach, replace, and drop either | Points match reference coordinates; presence changes no results |
| Node views | Read attributes/counts/text; parent, children, siblings, fields, descendants by range | Paired answers and node identity |
| Cursor views | Move, seek, clone, reset, reset-to, read node/depth/field, drop | Return values, current node, cursor-root boundary, clone independence |
| Scans | Subtree/order/direction, ID and range filters, partial consumption | Ordered reference results; node/group/count agreement |
| Queries | Compile/clone, inspect metadata, disable captures/patterns, configure/reuse/drop cursors, execute over nodes/trees/regions | Completed matches; documented errors; pause/resume and reuse after early termination |

Initially implement the ownership and navigation families, then scans and
queries in the same harness. Packing options vary `points`, `compact`, and
`symbol_presence` selection: none, all, selected region indices, and the default
heuristic. Store a selection recipe and construct the callback for each call.
Treat default coverage as unspecified in the metadata model; explicit selection
recipes determine expected coverage without access to physical group counts.
Capacity controls and physical group metrics are private; exercise growth by
varying document sizes and reusing scratch. `drop_scratch` applies to packers
and packed parsers; the Tree-sitter parser has no corresponding method.

Source edits use a small insertion table and produce a new document parsed from
scratch. Squatter currently has no edit registration or incremental parsing
API. Exercise both contiguous input and bounded chunk callbacks through the
implemented `Parse` trait and `PackedParseOptions`. Chunks may split UTF-8 and
reads may go backward; each callback exposes one immutable document. Compare
outputs, not callback event sequences. Keep database persistence, threading,
arbitrary slab corruption, and callback panics outside this test.

### Results and predictable pool sizes

Like the old test, each successful value-producing operation appends exactly
one entry. Compare optional navigation answers first; if both are `None`,
append the input node again. This keeps pool effects predictable without
discarding useful out-of-range child/field/range calls.

Use separate operations for expected failures: reject a known malformed query,
reject a known-invalid direct-parser input or empty packing region, or execute
a query against a different grammar. These append no value. An unexpected
parse/pack/load error fails the case; it never silently truncates execution or
substitutes a forest.

Malformed documents still receive full coverage through Tree-sitter parsing
and packing. Direct parsing must succeed on its supported valid-input subset,
and must return the expected error class on its invalid fixtures. After a
failure, reuse that parser on valid input and compare the result.

Use deterministic cancellation recipes for compatible parsing and presence
construction, followed by successful reuse. A canceled presence build leaves
the forest unchanged. `TreeFellerParser` ignores parse progress callbacks;
presence construction still honors `PackOptions::cancellation_callback`.
Cancellation probes append no value: validate and discard output if a short
operation finishes without invoking the callback. Use fixed cases large enough
to exercise cancellation as well.

## Generation and shrinking

Generate raw operations with weighted `prop_oneof!` strategies and small scalar
arguments. Map the case through `repair`, as the old test does. Repair runs a
metadata model, without invoking the library:

1. Resolve pool selectors to actual indices among compatible live entries.
2. Drop an operation only when required inputs are absent or incompatible.
3. Apply its pool/metadata effects, including nested view operations.
4. Store the resolved indices in the repaired case.

Rerun repair after every shrink. Shrinking can remove operations, shorten
nested sequences and documents, and reduce arguments. Print the repaired case:
its indices must be exactly those used by the interpreter. Runtime indexing
uses direct checked access; an invalid repaired index is a harness failure.

The model tracks ordered region grammars, roots and source coordinate frames,
forest provenance, storage ownership, saved sidecar compatibility, point state,
presence selection, and query configuration. It does not predict node shape or
library answers. Use stable forest and reference-root identities for provenance;
vector removal shifts operand indices, not those identities. `TreeIx`,
`RegionIx`, and `NodeId` are forest-local, not cross-pool identities. Assert
model/runtime pool sizes and provenance after each step. Ordinary operations
select compatible operands; negative operations explicitly select the mismatch
they intend to test.

Do not repair semantic arguments such as child indices into guaranteed success.
Generate boundary-oriented values: zero, last valid, first invalid, large values,
node endpoints, empty/reversed ranges, and EOF. Represent node-relative choices
symbolically and log their resolved numbers at failure. UTF-8 source splice
offsets are snapped to character boundaries; range probes need not be.

Start with small recursive JSON values and C/C# fixtures. Include empty input,
recovery nodes, extras, aliases, Unicode, CRLF, long tokens, and alternating
wide/deep trees. Bias sizes around group boundaries (31/32/33 and 63/64/65) and
coordinate deltas (255/256/257). These are source-generation biases, not claims
about the resulting node count: record actual node counts and slab byte sizes.
Keep group/waste assertions in the existing internal tests. Forest cases also
vary root order, overlapping spans, and repeated roots. Bounded region queries
use roots from a shared source and coordinate frame.

## Oracles

### Structure and navigation

At forest creation, walk each packed and reference root through cursors and
compare their complete ordered structure and attributes. Assign canonical node
identities from structural position, such as preorder ordinal, and retain maps
from each backend's node identity to that ordinal. Build correspondence only
after checking matching child structure.

Use those identities in later navigation and query comparisons, qualified by
the forest and packed root. Repeated input roots need distinct identities.
Native IDs and packed `NodeId` values are representation-specific.
`(kind, start_byte, end_byte)` is not a unique node key: missing nodes and nested
equal-span nodes can collide.
Compare fields through parent child positions or cursors where the APIs differ.
Rebuild correspondence after fresh packing. Compaction, detachment, core
round trips, and side-data changes preserve forest-local IDs; assert this
separately from semantic equality. Do not decode private slots from `NodeId`.
Use domain newtypes for child, kind, field, pattern, and capture arguments,
converting at the Tree-sitter boundary.

Navigation must stay within each packed tree. For a subtree packed as an
independent root, project the reference root's parent, siblings, and field to
absent while preserving its descendants and source coordinates. Exercise cursor
reset/reset-to across trees and grammars to check cached grammar metadata.

With point data attached, compare Tree-sitter coordinates. Without it, use
`Point::new(0, byte_offset)` in the oracle, including point navigation and range
filters. For those operations use the reference tree's topology with projected
coordinates rather than calling Tree-sitter's ordinary point lookup unchanged.
Generate and check the documented coordinate narrowing rules separately.

Compare cursor state after successful and failed moves, and test resets from
non-root positions. Respect the cursor's chosen root when predicting depth and
navigation. Partial child iteration compares returned nodes; its final cursor
state follows each API's documented behavior, not an assumed shared contract.

### Storage and side data

Call `Forest::validate()` on new/transformed forests and after side-data changes,
and recheck all surviving forests at case end, so parser/packer reuse cannot
quietly corrupt earlier outputs. Safe loading checks memory-safety invariants;
full content validation is explicit in both debug and release builds. After
dropping a source forest, continue using its copies and detached forests.

Core slab reloads start without side data and take grammar bindings in region
order, including repeated grammars. Compaction and detachment preserve attached
side data. Points affect grouping and cannot be built for an existing forest:
save `PointsData` from point-enabled packing, drop it, then restore it only to
the same layout or a layout-preserving copy. A fresh parse/pack with points
creates a new forest and new correspondence. Matching source alone does not
establish sidecar compatibility.

Build presence for all, none, or selected regions and check that coverage changes
no navigation, scan, or query results. Loading/attachment checks layout and
dimensions, not complete contents; use `validate_for` or `Forest::validate`
explicitly. Points validation does not establish agreement with source text,
which remains the reference-tree oracle's job. Known dimension-mismatch
replacements must fail without changing the previous attachment. Compare a
reused packer with fresh packing under identical options; semantic equality
remains the cross-representation requirement, including direct parsing and
loaded forests.

Retained-slab tests use an aligned immutable owner with a drop counter, following
the existing storage fixtures. Release all external owner handles, exercise
the forest, then assert that dropping its final owner releases the storage once.
After `detach`, drop the retained forest and use the detached result. Apply the
same ownership checks to retained sidecars and their replacement/drop operations.
Compacting a retained forest copies its core into owned storage and releases
that retained owner; attached sidecars keep their storage.
Check borrowed and retained loading preserve the expected slab address. A byte
vector alone does not guarantee the required eight-byte alignment. Trusted
loader variants may use only unmodified library-produced bytes with the same
grammar bindings; arbitrary bytes never reach unchecked loaders.

### Scans

Build expected preorder/postorder sequences through reference-tree navigation,
then apply simple scalar predicates. Do not use a squatter scan to produce its
own expected answer. Encode the documented range semantics, especially empty
ranges and zero-width nodes, using `tests/scanning.rs` as the starting point.

Compare node iteration, flattened group iteration, `count`, reversal, and
`next`/`nth` followed by `count` or `fold`. Public `GroupMatches` exposes `len`,
`is_empty`, and `nodes`, not masks or slots. Check each batch is nonempty and its
length matches enumeration; emitted nodes must stay within the chosen subtree
and tree. Do not assume physical group boundaries.

Generate a bounded scan recipe (order, direction, optional range and filters)
and dispatch to concrete generic pipelines. Scans can run within one view
operation; a short consumption script varies how each iterator is exhausted or
abandoned without requiring a heterogeneous pool of iterator types.

### Queries

Run supported queries against Tree-sitter, optimized squatter, and unoptimized
squatter, including trees with errors. Compare completed matches as a sorted
multiset of pattern index and ordered `(capture index, canonical node identity)`
lists. Preserve duplicate matches and repeated captures. Do not compare
backend-specific match IDs. Compare compilation metadata and diagnostics with
Tree-sitter; clone queries with `deep_clone` and vary disabling independently.

`QueryScope` accepts nodes, trees, and homogeneous regions. Compare region
execution with per-tree reference results, preserving input-tree order before
normalizing each tree's matches. No match may combine captures from different
trees. Whole forests are not query scopes. Resolve text through the selected
forest's `NodeId::tree()` and source mapping; equal byte ranges in independent
documents need not contain the same text. Exercise contiguous and chunked
`TextProvider` implementations, including splits within UTF-8.

Current `next_capture` returns provisional snapshots with unspecified event
order. Do not equate those snapshots with completed matches or demand identical
event sequences. Check valid capture indices/nodes, eventual exhaustion, and
completed-capture coverage under the existing test contract. Keep exact
completed-match equality as the stronger semantic check.

An execution operation contains a short script: consume results, remove a
match using `QueryMatch::remove` or that execution's own ID, change a streaming
iterator's range, pause/resume, stop early, or drain. Exercise both
`QueryExecution` and the `StreamingIterator` matches/captures entry points.
Copy result descriptions before advancing; removal must leave the current
capture borrow readable. Run removal/partial-capture scripts as contract and
reuse checks; their event positions need not refer to corresponding matches
across engines. Drop the
execution before mutating its query or reusing its cursor. Then execute again
to completion and compare with fresh cursors carrying identical settings,
including ranges changed through an iterator.

Supported executions require exact completed results, including intersecting
and containing byte/point ranges and maximum start depth. Bounded region
executions require an established shared source and coordinate frame. For
point-free trees, generate query point ranges in row zero and apply equivalent
byte bounds to the Tree-sitter oracle. Use fixed expected-result cases for other
rows. Invalid setters must preserve prior configuration after documented
narrowing; zero range ends are unbounded.
Wrong-grammar executions assert `InvalidExecution` and then verify cursor reuse.

Keep the documented containing/intersecting-range difference on malformed trees
explicit: `containing_ranges_finish_deferred_matches_in_error_subtrees` in
`tests/query_execution.rs` covers a deferred match that Tree-sitter drops.
Retain this as a fixed expected-result case checked in both Squatter modes;
initial generated combinations of both range types use error-free trees.
Other malformed-tree comparisons remain active.

Match limits check `did_exceed_match_limit` and subsequent reuse; partial result
sets need not agree across engines. Query progress callbacks use deterministic
call budgets, not wall-clock timeouts. `Break(())` pauses; a later advancement
resumes the same execution. Track stop requests in the callback to distinguish
temporary `None` from exhaustion, including when ready results precede a pause.
After a bounded number of pauses, drain and compare completed results with an
uninterrupted execution. There is no cancellation-status accessor, and progress
byte offsets need not be monotonic. Small cases need not invoke the callback;
retain a bounded fixed case large enough to exercise pause/resume.

There is no switch that disables all comparisons after a mismatch. A newly
found discrepancy fails and shrinks. Any necessary exclusion must name a
specific unsupported contract or tracked defect and leave other comparisons
active.

## Cost and diagnostics

Start with 128 cases, at most 24 outer operations and 32 steps per `Explore`,
with a total budget of 256 interpreted steps per case. Bound document bytes,
forest count, total roots/regions, saved sidecar bytes, callback chunks/pauses,
query complexity, and collected results as well. Apply limits in
generation/repair, not by silently stopping a case at runtime. Budget
violations during execution are failures with the offending operation recorded.

Honor `PROPTEST_CASES` and the runner's seed override. Cap shrinking at 4096
iterations, and persist failures beside the test in
`pool_proptest.proptest-regressions`. Start with the old test's `TestRunner`
pattern so one summary can report generated/admitted/executed operations,
repairs by reason, successful/failed navigation, parser failures, storage modes,
scan results, and completed query matches. Exclude shrink attempts from normal
coverage counts. Tune weights using actual executed coverage.

Failure output includes the repaired case, operation and nested-step indices,
source/query text, region grammars/options, forest/root provenance, resolved
arguments, and both answers. Persist the seed and print a readable operation
trace; promote useful minimized failures to focused regression tests.

## Implementation sequence

1. Add `proptest` as a squatter dev-dependency and the single integration test.
   Implement repair, paired single-tree forest validation, parser/packer
   ownership operations, and scoped node/cursor pools. Reuse existing fixtures.
2. Add multiple roots/regions, storage variants, saved sidecars, selective
   presence, boundary-focused documents, callback parsing, and scan recipes.
   Include fixed cases for parse/pack, reuse/`drop_scratch`/drop, and copy/drop/read
   lifecycles.
3. Add paired queries and execution scripts, including cursor reuse after
   partial consumption, pause/resume, failure, query changes, and region scopes.
   Link the finished test from `tests/README.md`.

Validate the harness itself with small deterministic cases: repair after
deleting a producer, an empty pool, a failed navigation result, and a removed
forest that shifts other indices. Deliberately perturb one compared answer to
confirm failure shrinking and replay, then remove that perturbation. Run the
targeted property test first; increase case counts once it reaches meaningful
operations within the normal test runtime budget.
