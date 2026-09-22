# Coverage migrated from the C core

`cargo test -p tree-squatter` runs these checks without the retired C core:

| Former C checks | Current coverage |
| --- | --- |
| `unit.c` | `support/internal.rs`: fixed columns, little-endian bytes, growth/compaction, optional columns, symbol encodings, presence indexes, invalid waste, and exhaustive navigation across group waste |
| `supertypes.c` | `native.c`: synthetic grammar analysis, cycles, aliases, extras, 65 supertypes, dictionary limits; `support/internal.rs`: direct/dictionary mask emission, resizing, cache restoration, and copied/borrowed loads |
| `parser.c` | `native.c` and `support/internal.rs`: speculative lexer fallback, unsupported grammars, concurrent lazy preparation and reuse; `bindings.rs`: direct parsing, recovery, and ownership |
| `query.c` | `query_execution.rs`: synthetic query/input matrix against Tree-sitter, exact completed matches, capture coverage, depth/ranges, cancellation, limits, removal, disabling and optimized/unoptimized execution |
| `context.c` | `storage.rs`, `bindings.rs`, and persistence tests: scratch reuse, ownership, overflow, grammar identity, and restored dictionaries |
| `compare.c`, `seek.c`, `feller-corpus.c` | `squatter-check` and the shared corpus fixtures in `tools/squatter/fixtures`; binding and scanning tests |
| `slab-compatibility.c`, `endian.py` | `examples/slab-compatibility.rs` and `tools/squatter/endian.py` |

Native fixture functions compile into a separate archive object, referenced only
by unit tests. They exercise the retained grammar and Tree-feller support code;
slab construction and loading use Rust.

Allocation-failure injection is intentionally omitted. Null C handles, query
copying, containing-range setters, and variable-width column primitives have no
corresponding Rust API. Timeout tests cover cancellation through the Rust API;
the C callback and mid-execution setters were not exposed by either Rust crate.
