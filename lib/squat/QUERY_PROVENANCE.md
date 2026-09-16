# Query implementation provenance

The human explicitly authorized copying and adapting `../main` after completing
the non-query work. The adaptation started after commit `a024015f4`.

Reference checkout: `c1ce0f4f166dad57cd18aa684ded2f701ec02299`.

| Source in `../main/lib/src/squatter` | SHA-256 at adaptation |
|---|---|
| query_packed.c | 835321a79e1c6355606575ea2786b15f8720848841e9638eba379ce174f1c606 |
| query_exec.c | d18f933dc865f5af7d4d452d97b71bc78935f0fb83e99d90de16e6e113cdbad9 |
| query_exec.h | 24c0d44b1fdec0e41209b8ab778b706c3a352eeac70c8b29a190767940b15e04 |

`query.c` adapts the compiler, grammar analysis, NFA, shared capture buffers,
finished-capture heap, and capture-containment filters from `query_packed.c`.
`query_plan.c` adapts `query_exec.c`'s local and anchored-child plans. Types and
exported functions use the `SQ`/`sq_` namespace; tree access uses slab nodes and
cursors. JIT and the reference's block/spine/offset/edit machinery are omitted.
The existing repository license covers these derivative files.

The slab adapters implement public display and sparse grammar IDs, direct supertype tests,
word-boundary-aware masked SWAR roots, group presence lookups, and exact combined
field/symbol masks. Physical slot indexes include leading waste; all traversal
and child scheduling normalize that waste. Conversion is not performed per query.

Rust uses the same built-in text-predicate semantics as this checkout's mainline
bindings, including their quantified and empty-capture behavior. Unknown host
predicates are exposed as metadata. Predicate evaluation receives source bytes,
not reconstructed text. Query compilation and regex compilation happen once.

C supports matches, provisional capture snapshots, query copying/disabling,
limits, match removal, maximum start depth, progress callbacks, and simple rooted
ranges. Branching/rootless queries with bounded byte/point ranges explicitly
report `SQ_QUERY_UNSUPPORTED_RANGE`. This restriction predates the relaxed capture
contract and remains until range coverage is validated. Capture event order,
snapshot contents, and duplicate counts need not match mainline; completed matches
remain exact. Callback cadence is representation-dependent.

Whole-query plans retain the shared capture coordinator, including finite match
limits. Unsupported plans use the NFA before emitting anything. Match limits do
not disable symbol scans, whole-query plans, or NFA state staging; because these
change discovery and eviction order, a limited execution may retain a different
valid subset than mainline. Other setters during planned execution defer NFA
restoration until advancement, preserving borrowed capture storage. Progress
callbacks run at bounded event intervals in both paths; timeout-enabled corpus
runs can therefore exercise plans too.

Initial validation passed all eleven grammars in the focused C matrix, with
optimization enabled and disabled, including supertype and multi-symbol roots.
Four grammars passed ASan/UBSan. A source-snapshotted corpus run passed 44 files
(original and mutated, two repeats) using grammar and Zed queries. Larger generated
TypeScript/JavaScript inputs reached mainline's timeout or the harness's snapshot
budget; those runs are recorded as failures and are not used for speed claims.
