## 1. Capture iteration exposes prefixes that complete-match iteration discards

**Confirmed behavior; possible streaming/deduplication bug or API clarification.**

For JSON `[1,2,3,4]`, run:

```query
(array (number) @n ("," (number) @n)*)
```

`ts_query_cursor_next_match` returns one match, with captures `[1,2,3,4]`.
On a fresh cursor, `ts_query_cursor_next_capture` returns:

| Match ID | Entire capture list in the returned match | Returned capture index |
|---|---|---|
| 0 | `[1,2]` | 0 |
| 1 | `[1,2,3]` | 0 |
| 2 | `[1,2,3,4]` | 0 |
| 2 | `[1,2,3,4]` | 1 |
| 2 | `[1,2,3,4]` | 2 |
| 2 | `[1,2,3,4]` | 3 |

Thus capture `1` is returned three times, under three match IDs, although the
completed-match API retains only the longest match. This was reproduced with
a standalone mainline consumer with default cursor options.

`next_capture` can publish captures from unfinished states whose next step is
considered guaranteed. Later longest-match pruning can supersede those states.
The API describes returning a capture and its match, but does not explain these
superseded matches or how a consumer should reconcile them.

This matters beyond duplicate callbacks. Exact compatibility exposes the timing
and contents of partial capture lists. A replacement executor cannot simply
find completed matches and sort their captures. Materializing every growing
snapshot can require quadratic total copying even when there are only linearly
many final captures; borrowing or sharing storage can avoid some internal work.

Upstream question: should capture iteration enumerate captures of the matches
that `next_match` would return, or are these provisional matches intentional?
If intentional, document IDs, duplicate emissions, and the partial-list contract.

Evidence: [`ts_query_cursor_next_capture`](main/lib/src/query.c),
[public API wording](main/lib/include/tree_sitter/api.h),
[ordered snapshot comparisons](main/conformance/dual_scan_test.c), and
[query execution design](main/query-execution-design.md).

## 2. Hidden-subtree reuse changes error recovery relative to a fresh parse

**Measured divergence; whether every instance is a bug remains open.**

Mainline can reuse a hidden composite such as JavaScript's `statement` around
`return_statement`. Reusing the wrapper skips a reduction that a fresh parse
would reconsider with the new lookahead. In recovery regions this can preserve
a keyword token (`return`, `else`, or `if`) where a fresh parse produces an
`identifier`.

The recorded 1,000-file sample found 47 files where mainline's incremental
result differed from its own fresh result in this way. Of those, 42 had both
results accepted by this project's experimental parse-validity checker. That
does not establish that either parse is uniquely correct, and the remaining
five require investigation rather than being declared upstream failures.

This is particularly expensive behavior to reproduce in a flattened tree:
retaining hidden reuse candidates was estimated to add typically 1–4 bytes per
visible node, roughly 15–40% more tree storage for affected grammars, plus
chunk-aware candidate traversal and breakdown. Those are historical estimates,
not measurements of the current hybrid implementation.

Upstream question: can reuse validation prevent stale recovery decisions, or
can the contract explicitly permit a fresh-equivalent/otherwise valid parse
without reproducing mainline's hidden-candidate choices? A report needs a
minimal original text, edit, grammar revision, and both mainline trees.

Evidence and measurement scope:
[planned differences](main/planned-differences.md),
[incremental reuse design](main/incremental-reparse.md), and
[`corpus_test --reparse`](main/conformance/corpus_test.c).
The large sweep was not rerun for this document.

## 3. Large generated lexers still disable most optimization in release builds

**Confirmed generator policy; partially improved upstream, with the remaining
performance tradeoff requiring controlled measurements.**

When the main lexer has more than 300 states, `Generator::add_pragmas` still
emits compiler directives disabling most optimization. Upstream commit
[`867aa6d14`](https://github.com/tree-sitter/tree-sitter/commit/867aa6d14418163560bf89f12c48107098b1ec8f),
`fix(generate): keep GCC jump tables enabled for large lexers`, changes the GCC
directive to:

```c
#pragma GCC optimize ("O0", "jump-tables")
```

GCC 15 disables jump tables at `O0`; explicitly restoring them addresses that
lexer-dispatch slowdown. This commit is newer than the pinned `v0.27.0` base of
`mgsloan-bugfixes` and is not included in the aggregate. That version still emits
the older `O0`-only GCC pragma. Upstream with the improvement still emits
optimization-disabling directives for Clang and MSVC. For GCC, its pragma still
overrides command-line `-O3` for subsequent function definitions in the generated
grammar translation unit, while enabling jump tables. The Tree-sitter runtime
is compiled separately and retains its optimization settings.

The rationale is that optimizing large lexer functions can make compilation
very slow. The claim that lexing contributes a negligible fraction of parse
time was removed from the upstream generator's comment by the improvement;
it remains in the pinned `v0.27.0` version.
Existing generated parsers need regeneration to receive the new directive:
`tree-sitter-c` 0.24.2's generated source uses the older `O0`-only pragma, and
its Rust build script compiles that source unchanged. Ordinary Tree-sitter
parsing and tree-feller both use these generated lexer functions, so the policy
affects both.

An exploratory tree-feller profile of C inputs put roughly 70% of samples in
the generated main and keyword lexers. That observation predates the jump-table
change and does not establish the remaining cost with the updated directive,
or the cost in upstream Tree-sitter. Local timings were affected by battery
power and other sessions, so they should not be used to claim an upstream
speedup. Query-only execution on existing trees should be unaffected.

Upstream question: after restoring GCC jump tables, does the compile-time saving
still justify disabling most other optimization? Compare grammars regenerated
with the updated directive against versions with the directives removed under
`-O1`, `-O2`, and `-O3`, keeping runtime optimization fixed. Measure full and
incremental parse time, grammar compilation time and peak memory, and binary
size across several large grammars. A configurable policy or lower optimization
level may offer a better tradeoff than forced `O0` with jump tables.

Evidence: [`add_pragmas` and its rationale](crates/generate/src/render.rs),
the upstream commit above, plus `tree-sitter-c` 0.24.2's `src/parser.c` and
`bindings/rust/build.rs`. The updated directive has not been benchmarked here.

## Lower-priority questions

These should not be reported as confirmed upstream defects without more evidence.

- **Inline leaves lose `depends_on_column`.**
  [`summary_test.c`](main/test/packed/summary_test.c) avoids comparing this flag
  on inline leaves: [`ts_subtree_new_leaf`](main/lib/src/subtree.c) can inline a
  non-external leaf with the flag set, but the inline representation has no such
  bit and its accessor returns false. However, the inspected
  [parser path](main/lib/src/parser.c) records the flag after a successful
  external scan, whose token is already ineligible for inlining. This is a real
  constructor inconsistency, not yet a reachable public parsing bug. Establish
  reachability before adding storage or reproducing the loss in another format.

- **Parse states on extras.** The shadow proptest found mainline state 20 versus
  squatter state 465 for a JavaScript comment at bytes 43–54. A token's cached
  lexing state can differ from the state where it is shifted; this alone does
  not establish a mainline defect. Clarify what the public parse-state APIs
  promise for extras and whether consumers can use those states for lookahead.
  See [the finding](main/todo.md) and [`ts_node_parse_state`](main/lib/src/node.c).

- **Repetition treats anonymous separators differently from anchors.** On JSON
  `[1,2,3,4]`, `(array (number)* @n)` and the `+` variant each return four
  one-capture matches, not one four-capture match. Explicitly repeating the
  comma-number group produces the latter. This review confirmed the behavior,
  already preserved by `test_quantifier_star` in
  [query scan tests](main/test/packed/query_scan_test.c). It may be intended
  immediate-sibling semantics: the
  [query documentation](main/docs/src/using-parsers/queries/2-operators.md)
  itself demonstrates comma-separated groups. Clarifying this distinction would
  help prevent treating repetition as an ordinary greedy scan of named children.
