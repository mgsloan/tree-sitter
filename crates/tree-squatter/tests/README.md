# Coverage migrated from the C core

`cargo test -p tree-squatter` runs these checks without the retired C core:

| Former C checks | Current coverage |
| --- | --- |
| `unit.c` | `support/internal.rs`: fixed columns, little-endian bytes, growth/compaction, optional columns, symbol encodings, presence indexes, invalid waste, and exhaustive navigation across group waste |
| `supertypes.c` | `../native/tests.c`: synthetic grammar analysis, cycles, aliases, extras, 65 supertypes, dictionary limits; `support/internal.rs`: direct/dictionary mask emission, resizing, cache restoration, and copied/borrowed loads |
| `parser.c` | `../native/tests.c` and `support/internal.rs`: speculative lexer fallback, unsupported grammars, concurrent lazy preparation and reuse; `bindings.rs`: direct parsing, recovery, and ownership |
| `query.c` | `query_execution.rs`: synthetic query/input matrix against Tree-sitter, exact completed matches, capture coverage, depth/ranges, cancellation, limits, removal, disabling and optimized/unoptimized execution |
| `context.c` | `storage.rs`, `bindings.rs`, and persistence tests: scratch reuse, ownership, overflow, grammar identity, and restored dictionaries |
| `compare.c`, `seek.c`, `feller-corpus.c` | `squatter-check` and the shared corpus fixtures in `tools/squatter/fixtures`; binding and scanning tests |
| `slab-compatibility.c`, `endian.py` | `examples/slab-compatibility.rs` and `tools/squatter/endian.py` |

Native fixture functions compile into a separate archive object, referenced only
by unit tests. They exercise the retained grammar and Tree-feller support code;
slab construction and loading use Rust.

Direct-parser tests compare callback and contiguous input, including split UTF-8,
temporary buffers, replay, errors, and parser reuse.
External-scanner checks cover branch state, token caching, private replay,
zero-width tokens, Unicode columns, and Python output against Tree-sitter.

Allocation-failure injection is intentionally omitted. Null C handles and
variable-width column primitives have no corresponding Rust API. Query tests
also cover metadata and cloning, chunked text providers, streaming result borrows
and removal, persistent iterator ranges, containing ranges, and resumable progress
callbacks in optimized and general execution.

With combined containing and intersecting ranges, Squatter can retain deferred
matches that Tree-sitter drops while skipping hidden nodes in malformed trees.
`containing_ranges_finish_deferred_matches_in_error_subtrees` covers this case.

[`bisim.rs`](bisim.rs) runs repaired operation sequences over pools of documents,
parsers, packers, forests, sidecars, queries, and query cursors. Scoped node and
cursor pools compare navigation with Tree-sitter; scans use reference traversal
and queries compare completed matches in both execution modes. Fixed cases cover
mixed grammars, storage ownership, cancellation, and reuse after partial queries.

Run `cargo test -p tree-squatter --test bisim`. The default is 128 cases;
`PROPTEST_CASES` and `PROPTEST_RNG_SEED` override the count and seed. Failures
shrink and persist in `bisim.proptest-regressions`, with repaired operands
and source/provenance diagnostics. `-- --nocapture` prints executed coverage;
`BISIM_TRACE=1` also prints operations and resolved range probes.

For concurrent cases, use the shared command-line runner:

```sh
PROPTEST_CASES=100000 cargo run --release -p tree-squatter --example bisim -- -j 8
```

`-j N` (or `-jN`) sets the worker count; the default is the available CPU count.
Workers divide the total case count and replay persisted regressions independently.
Each owns its pools and uses the base seed plus its zero-based worker index.
The runner prints those seeds and combined coverage, runs fixed regressions once,
and exits unsuccessfully if any worker fails. Reproduce a worker with its printed
`PROPTEST_RNG_SEED`, case count, and `-j 1`. Fork/timeout options use `cargo test`.
