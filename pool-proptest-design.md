# Pool-based property testing

Proposed integration test: `crates/squatter/tests/pool_proptest.rs`, run by
`cargo test -p tree-squatter --test pool_proptest`. This document describes the
test; it does not implement it. It targets the APIs at main commit `85451c8cd`.

The reference is
[`shadow_proptest.rs`](../bit-packed/crates/cli/tests/shadow_proptest.rs)
in the bit-packed worktree. Keep its central idea: generate a sequence of
operations over pools of live values, compare results, and shrink the sequence
to a reproducer. Adapt the operations to the current Rust API, especially
packing, side data, storage ownership, scans, and reusable scratch.

## Case structure

A case contains generated documents and a `Vec<Operation>`. Start with one
Tree-sitter parser, one packing context, and a paired JSON tree. Queries and
direct parsers are created by operations. Each tree entry owns its source and
retains a Tree-sitter reference tree, grammar identity, packing options, and
expected side-data state.

The outer pools contain owned values:

| Pool | Values and model facts |
| --- | --- |
| Documents | UTF-8 source and grammar; edits produce new entries |
| Parsers | Tree-sitter parsers and restricted direct parsers; backend and grammar |
| Packing contexts | Reusable `PackContext` values |
| Trees | Owned `Tree` or `BackedTree`, reference tree, source, stable identity, point/cache flags |
| Queries | Squatter and Tree-sitter queries, source, grammar, disabled patterns/captures |
| Query cursors | Reusable optimized, unoptimized, and Tree-sitter cursors plus configuration |

Use fixed grammar fixtures from `tests/support/mod.rs`: JSON first, then C and
C#. Direct parsing gets an explicit subset of supported grammars and documents.
Grammar handles can initially be fixture-owned; testing their destruction is
already covered by `bindings.rs`.

### Borrowed values

`Operation::Explore(Vec<ViewOperation>)` borrows the live trees and runs a second
operation sequence over pools of paired nodes and actual tree cursors. Seed
these pools with each tree's root and root cursor. Navigation results and cursor
clones grow the pools; explicit drops remove entries. A cursor retains its real
ancestor stack across steps.

All nodes, cursors, and temporary borrowed slab views die when `Explore`
returns. The outer sequence can then repack in place, change side data, or drop
trees. Implement this by splitting borrows of `World`'s fields; the view runner
borrows the tree pool immutably and the query pools mutably. No extended
lifetimes, leaked trees, or self-referential container are needed.

This deliberately tests sequences of ownership operations interleaved with
sequences of borrowed operations. It does not retain a cursor while an unrelated
tree is created or dropped. That is a coverage limitation, but avoids adding
unsafe lifetime management to the harness. Parser/context reuse still happens
while their earlier output trees remain alive, and destruction really frees
the selected tree.

For `BorrowedTree`, construct views from existing aligned slabs at entry to an
`Explore` block, before creating node/cursor pools. Keep those views in a local
collection that outlives the pools. Model their missing side data separately
from the source trees. `BackedTree` belongs in the outer pool because it owns
its backing storage.

## Operations

Group pure accessors into one operation. Give navigation, configuration, and
ownership changes distinct variants so they shrink independently.

| Family | Operations | Main checks |
| --- | --- | --- |
| Documents | Add fixture/generated source; splice an existing document | New immutable source; UTF-8 boundaries and bounded size |
| Parsing/packing | Create, reuse, trim, and drop parsers/contexts; parse and pack with varied options | Reference structure; fresh versus reused scratch; earlier trees unchanged |
| Storage | Repack copy/in place; load copied or retained slab; detach retained tree; drop tree | Structure and ownership survive; side-data preservation follows each API |
| Side data | Build, serialize/reload, attach, replace, and drop points/presence | Points match source; presence changes no results |
| Node views | Read attributes/counts/text; parent, children, siblings, fields, descendants by range | Paired answers and node identity |
| Cursor views | Move, seek, clone, reset, reset-to, read node/depth/field, drop | Return values, current node, cursor-root boundary, clone independence |
| Scans | Subtree/order/direction, ID and range filters, partial consumption | Ordered reference results; node/group/count agreement |
| Queries | Compile, disable captures/patterns, configure/reuse/drop cursors, execute | Completed matches; documented errors; reuse after early termination |

Initially implement the ownership and navigation families, then scans and
queries in the same harness. Packing options vary `points`, `symbol_presence`,
`repack`, and small `initial_group_capacity` values including 0 and 1.

Source edits use a small insertion table and produce a new document parsed from
scratch. Squatter currently has no edit registration or incremental parsing
API. Keep database persistence, threading, arbitrary slab corruption, parser
callbacks, and proposed parser APIs outside this test.

### Results and predictable pool sizes

Like the old test, each successful value-producing operation appends exactly
one entry. Compare optional navigation answers first; if both are `None`,
append the input node again. This keeps pool effects predictable without
discarding useful out-of-range child/field/range calls.

Use separate operations for expected failures: reject a known malformed query,
reject a known-invalid direct-parser input, or attempt an explicitly unsupported
query execution. These append no value. An unexpected parse/pack/load error
fails the case; it never silently truncates execution or substitutes a tree.

Malformed documents still receive full coverage through Tree-sitter parsing
and packing. Direct parsing must succeed on its supported valid-input subset,
and must return the expected error class on its invalid fixtures. After a
failure, reuse that parser on valid input and compare the result.

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

The model tracks grammar, tree provenance, ownership variant, side-data flags,
and query configuration. It does not predict node shape or library answers.
Use stable tree identities for provenance; vector removal shifts operand
indices, not those identities. Assert model/runtime pool sizes and provenance
after each step. Ordinary operations select compatible operands; negative
operations explicitly select the mismatch they intend to test.

Do not repair semantic arguments such as child indices into guaranteed success.
Generate boundary-oriented values: zero, last valid, first invalid, large values,
node endpoints, empty/reversed ranges, and EOF. Represent node-relative choices
symbolically and log their resolved numbers at failure. UTF-8 source splice
offsets are snapped to character boundaries; range probes need not be.

Start with small recursive JSON values and C/C# fixtures. Include empty input,
recovery nodes, extras, aliases, Unicode, CRLF, long tokens, and alternating
wide/deep trees. Bias sizes around group boundaries (31/32/33 and 63/64/65) and
coordinate deltas (255/256/257). These are source-generation biases, not claims
about the resulting node count: record actual groups and waste.

## Oracles

### Structure and navigation

At tree creation, walk the packed and reference trees through cursors and
compare their complete ordered structure and attributes. Assign canonical node
identities from structural position, such as preorder ordinal, and retain maps
from each backend's node identity to that ordinal. Build correspondence only
after checking matching child structure.

Use those identities in later navigation and query comparisons. Native IDs and
packed slots are representation-specific. `(kind, start_byte, end_byte)` is not
a unique node key: missing nodes and nested equal-span nodes can collide.
Compare fields through parent child positions or cursors where the APIs differ.
Rebuild slot mappings after storage changes; never assume IDs survive repacking.

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

Immediately validate new/transformed trees and recheck all surviving trees at
case end, so parser/context reuse cannot quietly corrupt earlier outputs.
After dropping a source tree, continue using its copies and detached trees.

Core slab reloads start without side data. Repacking preserves attached side
data. Rebuild points from the retained exact source and restore serialized
sidecars only to a compatible slab. Compare a reused context with fresh packing
under identical options; semantic equality remains the cross-representation
requirement, including direct parsing and loaded trees.

Retained-slab tests use an aligned immutable owner with a drop counter, following
the existing storage fixtures. Release all external owner handles, exercise
the tree, then assert that dropping its final owner releases the backing once.
After `detach`, drop the backed tree and use the detached result. Check borrowed
and retained loading preserve the expected slab address. A byte vector alone
does not guarantee the required eight-byte alignment.

### Scans

Build expected preorder/postorder sequences through reference-tree navigation,
then apply simple scalar predicates. Do not use a squatter scan to produce its
own expected answer. Encode the documented range semantics, especially empty
ranges and zero-width nodes, using `tests/scanning.rs` as the starting point.

Compare node iteration, flattened group iteration, `count`, reversal, and
`next`/`nth` followed by `count` or `fold`. Check that group masks contain only
valid slots and that emitted nodes stay within the chosen subtree.

Generate a bounded scan recipe (order, direction, optional range and filters)
and dispatch to concrete generic pipelines. Scans can run within one view
operation; a short consumption script varies how each iterator is exhausted or
abandoned without requiring a heterogeneous pool of iterator types.

### Queries

Run supported queries against Tree-sitter, optimized squatter, and unoptimized
squatter. Compare completed matches as a sorted multiset of pattern index and
ordered `(capture index, canonical node identity)` lists. Preserve duplicate
matches and repeated captures. Do not compare backend-specific match IDs.

Current `next_capture` returns provisional snapshots with unspecified event
order. Do not equate those snapshots with completed matches or demand identical
event sequences. Check valid capture indices/nodes, eventual exhaustion, and
completed-capture coverage under the existing test contract. Keep exact
completed-match equality as the stronger semantic check.

An execution operation contains a short script: consume results, remove a
previously returned match using that execution's own ID, stop early, or drain.
Run removal/partial-capture scripts as contract and reuse checks; their event
positions need not refer to corresponding matches across engines. Drop the
execution before mutating its query or reusing its cursor. Then execute again
to completion and compare with fresh cursors carrying identical settings.

Unrestricted supported executions require exact completed results. Explicit
negative cases assert `UnsupportedRange` or `InvalidExecution` and then verify
cursor reuse. Invalid setters must preserve prior configuration. Match limits
and cancellation check termination, flags, and subsequent reuse; partial result
sets need not agree across different engines. Avoid timing-dependent equality
or requiring a short query to hit a wall-clock timeout.

There is no switch that disables all comparisons after a mismatch. A newly
found discrepancy fails and shrinks. Any necessary exclusion must name a
specific unsupported contract or tracked defect and leave other comparisons
active.

## Cost and diagnostics

Start with 128 cases, at most 24 outer operations and 32 steps per `Explore`,
with a total budget of 256 interpreted steps per case. Bound document bytes,
tree pool size, query complexity, and collected results as well. Apply limits
in generation/repair, not by silently stopping a case at runtime. Budget
violations during execution are failures with the offending operation recorded.

Honor `PROPTEST_CASES` and the runner's seed override. Cap shrinking at 4096
iterations, and persist failures beside the test in
`pool_proptest.proptest-regressions`. Start with the old test's `TestRunner`
pattern so one summary can report generated/admitted/executed operations,
repairs by reason, successful/failed navigation, parser failures, storage modes,
scan results, and completed query matches. Exclude shrink attempts from normal
coverage counts. Tune weights using actual executed coverage.

Failure output includes the repaired case, operation and nested-step indices,
source/query text, grammar/options, tree provenance, resolved arguments, and
both answers. Persist the seed and print a readable operation trace; promote
useful minimized failures to focused regression tests.

## Implementation sequence

1. Add `proptest` as a squatter dev-dependency and the single integration test.
   Implement repair, paired tree validation, ownership operations, and scoped
   node/cursor pools. Reuse existing fixture helpers.
2. Add storage variants, side-data transitions, boundary-focused documents, and
   scan recipes. Include fixed cases for parse/pack, reuse/trim/drop, and
   copy/drop/read lifecycles.
3. Add paired queries and execution scripts, including cursor reuse after
   partial consumption, failure, and query changes. Link the finished test from
   `tests/README.md`.

Validate the harness itself with small deterministic cases: repair after
deleting a producer, an empty pool, a failed navigation result, and a removed
tree that shifts other indices. Deliberately perturb one compared answer to
confirm failure shrinking and replay, then remove that perturbation. Run the
targeted property test first; increase case counts once it reaches meaningful
operations within the normal test runtime budget.
