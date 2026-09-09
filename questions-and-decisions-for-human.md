# Questions and decisions for human review

## Scope

- Implement `design.md`, completing the non-query work first. The human subsequently authorized the query engine and reuse of `../main` (see the scope updates below).
- Keep the mainline runtime and its bindings unchanged. The C implementation lives in `lib/squat/`; a separate Rust crate builds and wraps it.
- The existing untracked `todo.md` belongs to the human and is left untouched.

## Decisions

- Start with the representation and a differential testable API, then build corpus and benchmark tooling on that API.
- Version 1 uses native endianness and the exact matching grammar. Cross-endian persistence and a durable grammar fingerprint remain open design questions. The slab header is explicitly padded to 32 bytes, with all padding initialized to zero.
- Dictionary overflow is a reported conversion error, rather than the crash provisionally described in the design. Allocation and 32-bit layout overflow are reported as errors too.
- Navigation may scan siblings or ancestors when the format has no direct index. Cursors maintain an ancestor stack for efficient repeated tree walks.

## Questions

- What grammar identity should a persistent envelope record: generated parser hash, grammar source revision, or both? Version 1 callers must provide the exact matching grammar.
- Should the supertype dictionary grow beyond 256 entries by widening its node column in a later representation version?
- Should packed trees preserve included ranges as a separate optional section? The current design preserves node coordinates, but has no included-range section.

## Validation and remaining work

Updated alongside implementation commits below.

### C runtime baseline

- Implemented aligned slab columns, checked 32-bit layout arithmetic, lane-aware growth/compaction, reverse raw-subtree traversal, navigation/cursors, symbol presence entries, supertype dictionaries, and validated deserialization.
- Reverse traversal stages each open parent's child positions. Subtracting a multiline child's point extent cannot recover its preceding column. This avoids repeated forward rescanning and does not stage a full array of visible node attributes.
- Supertype bit order is ascending raw grammar ID, including metadata on ABI-14 grammars whose public supertype enumeration is unavailable. A visible node receives its incoming mask, as specified in the design; its own bit applies to children.
- Mainline's next-sibling node accessor skips empty nodes ending at the original node's end byte; cursors and child enumeration retain them. The public API reproduces this distinction. A separate structural sibling helper supports complete iteration.
- Mainline field lookup stops at ERROR productions even though child field enumeration may expose fields from hidden children. Squat preserves this distinction.
- `child_with_descendant` requires a strict descendant in squat. Mainline can return surprising children when passed the parent itself; that out-of-contract case is not reproduced.
- The cached code-corpora build image works. Its grammar runtime image was absent and the registry pull failed authentication. Tests compile local grammar sources inside the existing offline build image instead.
- Initial JSON/Python differential checks and standalone lane relocation checks pass. Broader grammar/error-recovery and sanitizer coverage is in progress.

### Scope and compatibility updates from the human

- Query implementation is now authorized, after the non-query implementation is finished. Reference `../main` and reuse/adapt its optimized query code where useful. Do not start that phase before the preceding work is complete.
- The human already established that padded, non-straddling u64 SWAR wins for symbol equality. Retain it as the baseline; experiments should investigate useful remaining tradeoffs rather than rediscover the format choice.
- The human asked for readable control flow, meaningful names, and concise explanations of non-obvious invariants. Added local formatting rules and expanded the initial compact C implementation accordingly.
- The human explicitly requested programmatically ignoring seek differences for now, suspecting an upstream bug. They are counted and reported, but do not fail default comparison runs. `--strict-seeks` restores strict behavior. No hidden-node section or seek-barrier metadata was added.
- Minimal valid CSS repro is `a b {}` in `lib/squat/tests/fixtures/hidden-seek.css`: an empty hidden node can make mainline return the visible ancestor where the packed tree finds the child at the same position. This also occurs in malformed inputs in other grammars. Treating this as an upstream bug remains a hypothesis, not a confirmed diagnosis.

### Equality and layout experiments

- Added exact lane-wise SWAR equality, with a physical-slot group mask that removes leading waste. Tests compare every encoded column against scalar reads and exercise widths 2–32, including adjacent-lane carry/borrow cases.
- Added scalar, portable SWAR, hardware-popcount SWAR, compiler-targeted AVX2, explicit SSE2, and explicit AVX2 microbenchmarks. Hardware-popcount and compiler-targeted baselines prevent attributing compiler flag differences to SIMD alone.
- Experimental builds can use 32 or 64 slots per group; leading-waste width and magic flags change with the group size. The default public format remains 16 slots. These builds are for measurement, not an unversioned change to persisted data.
- Rust bindings retain the grammar, borrow nodes/cursors from the owning slab, and expose shared statically dispatched tree/node/cursor traits. A container integration executable verifies FFI layout, independence from the mainline tree, traversal, and serialization.

### Corpus-driven navigation fixes

- Backward packed cursors now cache u32 sibling slots within open frames. The initial implementation rescanned a wide parent on every previous-sibling step, which was quadratic on a multi-megabyte JSON corpus file. The cache is cursor-owned and is freed on ascent; it adds no persistent per-node storage.
- Valid TypeScript `type Example = typeof object.property;` exposes a distinct field-lookup case: mainline can inherit a field through a hidden grammar node made visible by aliasing, returning a grandchild. A single cursor field ID cannot reconstruct that API result.
- Added a sparse field-lookup exception section, computed bottom-up without a full-tree pointer map. Each entry is `(parent slot, field ID, result slot)` in three u32s; a null result uses `UINT32_MAX`. The header grows from 32 to 40 bytes and the format version changes. This is a necessary amendment to preserve field lookup, independent of the ignored seek differences. Most nodes need no entry. Please review this extension to the original layout.
- Mainline's reverse cursor also sometimes reports different fields than its forward cursor for the same node: the implementation updates a structural child index only when an alias sequence exists. The benchmark now uses the same forward-enumerate/reverse-consume sibling adapter for both backends during `walk-backward`. Every attribute is still read in reverse preorder, and both sides pay the same adapter cost. Mainline source files remain unchanged.
- Corpus inventory must avoid `training -> train`, an alias present in this corpus checkout. Directory symlinks are skipped, preventing duplicate train/training inputs and preserving path-based seeds.

### Corpus and benchmark tooling

- Implemented deterministic inventory, mutation, per-domain selection, ten sample lists, novelty selection, and the existing memory Pareto model as a `corpus-analysis` subcommand.
- Implemented paired non-query benchmarks with batch order alternation, repeat medians, paired-ratio percentiles, Linux counters when available, provenance, and partial-output handling. The current container denies performance counters; they are reported as null with the kernel error.
- A source-snapshotted offline run passed all 40 selected original files with three repeats; mutated inputs and layout checks are being completed. See `tools/squatter/README.md` for commands and measurement boundaries.
- Cache-line layout experiments align the allocation itself as well as column offsets. They use an aligned allocate/copy path because ordinary realloc does not preserve 64-byte alignment.
- Sparse grammar symbols, variable-width supertypes, and interleaved field/symbol representations currently have byte estimates only. Their access-speed tradeoffs are not represented as measured results.

### Measured experiment results

- The 27-file, eleven-grammar layout run covered 2.81 million visible nodes. Compact slabs used 16.20, 15.24, and 17.10 bytes/node for 16/32/64 slots respectively. Keep 16 as the default: larger groups regress several grammars, especially TypeScript, and this bounded sample does not supersede the design's wider Pareto data. Full inputs, hashes, numbers, and caveats are in `lib/squat/experiments/`.
- Explicit AVX2 bulk equality counts were about 1.5 times faster than the hardware-popcount SWAR baseline at widths 8, 9, and 12. The explicit implementation is retained. Production group scans still use exact SWAR masks; bulk counting does not establish a query speedup.
- Field exceptions cost only 48 bytes over this experiment sample. Sparse grammar IDs and variable-width supertypes show modeled savings worth revisiting, but access/packing costs have not been measured. Interleaving symbol and field lanes increased their modeled column size by 2.5%.
- All four layout variants passed unit and small-input grammar comparisons, including mutated inputs and serialization. Six grammars passed ASan/UBSan after the field/cursor changes.
- Strict CSS repro verified short-circuit output: one recorded failure, zero completed files, and flushed partial summaries. This also caught and fixed absolute file paths being treated as sampling lists.
- Indexed child-accessor comparisons now sample very wide parents; full cursor traversal still checks every transition. Repeated indexed access was making the test harness quadratic on malformed large arrays.
- Benchmark identity accounts for explicit ELF-loader invocation: argv[0] identifies the benchmark executable, while `/proc/self/exe` can identify the loader instead.

### Non-query completion checkpoint

- All non-query implementation is complete before starting the query engine. The holdout run passed 20 files, original and mutated, with two repeats. The wider 40-file run continues as an additional stress check.
- Added a permanent CLI-control integration script that verifies absolute paths, ignored/strict seek policy, partial-output flushing, and byte-identical sampling lists. It passed against the source-snapshotted tool build.
- Parent lookup now searches backward for the nearest enclosing subtree. Group span bases plus the maximum u8 delta reject distant groups without reading their nodes. This adds no index and avoids restarting from the root for nearby ancestors.
- The convenience runner now samples training and holdout repositories separately, so one split cannot crowd the other out.

### Query engine adaptation

- After the non-query checkpoint, adapted the optimized compiler, NFA, shared capture histories, deduplication filters, local shortcuts, and whole-query structural plans from `../main` at `c1ce0f4f166dad57cd18aa684ded2f701ec02299`. Slab access and SWAR root/presence filtering are new adapters. No mainline runtime files are changed.
- Completed both the 40-file training stress run (three repeats, original and mutated) and 20-file holdout run (two repeats, original and mutated) with zero comparison failures.
- Query cancellation callback cadence is representation-dependent because mainline visits hidden nodes; cancellation must terminate both engines, but identical stopping captures are not promised.
- Bounded queries with branching or rootless patterns can depend on hidden traversal barriers: malformed CSS with `(_ (_)+ @children) @parent` and byte range 1..12 changes ordered partial captures. Such executions explicitly report `SQ_QUERY_UNSUPPORTED_RANGE` rather than silently returning different results. Unrestricted queries and simple rooted range queries remain supported. This is distinct from the human's instruction to ignore seek differences.
- Question: should a future slab extension preserve hidden traversal barriers for full range-query compatibility, or should that optional API continue to report unsupported combinations?

- The first real-query matrix passed 44 files across eleven grammars, original and mutated, with two repeats. It uses the actual grammar/Zed sources and compares complete ordered capture snapshots after identical built-in text predicates.
- Two large generated TypeScript/JavaScript files exceeded mainline's 30-second query timeout or the harness's four-million captured-node snapshot budget. These remain explicit failed stress cases, not ignored comparisons. The runner now accepts `--max-file-bytes` for bounded correctness runs.
- Timeout callbacks now poll within column scans as well as NFA/plan events. Timeouts therefore do not silently disable SWAR root or mandatory symbol/field filtering. Callback cancellation ends a scan; callers should start a new execution to restart it.
- Found another concrete mainline issue while testing query mutation: `ts_query_disable_pattern` removes wildcard patterns without decrementing `wildcard_root_pattern_count`, then query execution asserts in `lib/src/query.c`. The slab adaptation already maintains that count. Its regression checks the intended remaining-pattern behavior directly; mainline files remain unchanged. Capture disabling still receives differential coverage.
- Expanded query validation passed 88 files across eleven grammars, original and mutated, with three repeats: 120 query sources and 1,407 patterns, all accepted by both compilers. The same 88 originals also passed with scan/plan shortcuts disabled. Fresh ASan/UBSan checks passed JSON, TypeScript, CSS, and YAML after enabling callback-aware scans.
- Rust query captures borrow the execution cursor and nodes borrow the tree. A compile-fail doctest enforces the capture borrow, while the container integration check covers text predicates, capture disabling, cursor reuse after query/tree/options destruction, and recovery from unsupported-range errors.
- The query group-size experiment exposed an allocation-estimate oversight: initial capacity assumed 12 nodes/group even in 32/64-slot builds. The 16-slot default was correct, and earlier compact layout results were unaffected. Scale the 75%-occupancy estimate with group size, then rerun query layout measurements to avoid comparing needless spare capacity.
- The external corpus image lock changed during this session to an image that is not cached locally. Final verification explicitly selects the previously recorded offline image; the runner now checks image availability early and reports how to select an installed image.
- Made the native grammar loader's unsafe boundary explicit. A cloned Tree-sitter `Language` does not retain its dynamic library, so callers must keep the loader owner alive through all derived parsers, trees, and queries. Both tools already enforce this drop order; the public loader now documents and requires the contract.
- Final integrated verification passed all seven benchmarks on 88 original and 88 mutated files, with two repeats. It counted 12 original and 28 mutated seek discrepancies without failing them, exactly as requested. Strict-seek/partial-output and deterministic-sampling control checks also passed.
- Corrected query-layout matrix: all ten configurations passed 88 files each, with three repeats, across original/mutated, 16/32/64-slot, disabled-shortcut, and repacked variants. The retained 16-slot default had median squat/mainline time ratios of 0.557 for matches and 0.625 for captures on originals (0.544/0.593 mutated). Disabling scan/plan shortcuts increased median query time by 28.4%/22.3%. Larger groups showed no clear query-speed advantage.
- Repacking cut this bounded original query sample from 21.66 to 18.03 bytes/node without a meaningful median query-time change. Keep repacking optional because it adds conversion work. Full query/source/grammar identities, per-language quantiles, compiler flags, and ablations are committed in `lib/squat/experiments/query-results-2026-09-09.json`.

### Optional unpacking cursor

- Added `SQCachedCursor` / Rust `CachedCursor`, selected by `Node::walk_cached()`, while retaining `SQCursor` / `Cursor` as the default. The reference is `../main/lib/src/squatter/api_packed.c`'s batched size/extent cache; this implementation lazily decodes one group per column, independently. No serialized-format or query-engine policy changes.
- Navigation is instantiated from one shared C implementation so cached and uncached behavior cannot drift; the ordinary cursor has no cache-mode branch. A full-group decode loads each packed word once and accounts for non-straddling tail bits, spare-capacity prefixes, and non-word-aligned group boundaries.
- Both cursor types expose bulk attribute snapshots with the same FFI call count. Ordinary node handles returned by a cursor do not carry its cache. Counts still use node scans, preventing descendant/child reads from evicting the current group's attributes. On x86-64 the 16-slot handles occupy 32 bytes uncached and 1,088 bytes cached, plus the same traversal-frame allocations.
- No decision from the human is required: both types remain available. The initial measurement supports keeping the default unchanged; caching helps forward attribute walks but adds overhead to navigation-only walks and to the reverse compatibility adapter's short-lived cursors.

- Completed eight cursor workloads on 88 bounded files and a second 53-file sample, each original and mutated with five repeats, without comparison failures. The second sample includes nine files over 1 MiB and 4.74 million original nodes. These samples overlap; they are not 141 distinct inputs.
- Forward attribute-walk median cached/uncached ratios were 0.865/0.863 on the bounded originals/mutations. For the nine large inputs they were 0.855/0.865, with native forward navigation at 0.900/0.898. Every large input benefited in both forward workloads. Small navigation-only walks often favored the uncached cursor, while the reverse attribute adapter's fresh cursor per node penalized caching substantially. Keep both types and the existing default.
- Benchmark walks now use the same bulk attribute FFI call for both squat cursor types. Do not attribute differences from the previous per-node-accessor benchmark to caching alone. Separate `cursor-*` workloads measure native navigation without attributes or the reverse adapter; `walk-*-cached` selects the cached cursor in attribute walks. All are enabled by default, and detailed results are in `lib/squat/experiments/`.
- Native mainline previous-sibling movement can restore positions by summing earlier siblings after line breaks. Keep those timings distinct from the reverse compatibility adapter and from direct cached/uncached ratios. No mainline change was made.
- Validation passed scalar-versus-bulk decoding across supported widths and spare capacities; forward/reverse/mixed/subtree-rooted cursor checks across eleven grammars at 16/32/64 slots; ASan/UBSan on four grammars; Rust ownership/attribute/persistence integration; the JSON query regression; workspace package tests and strict Clippy. Query execution retains its existing cursor policy; this experiment establishes tree-cursor results, not a query speedup.

### Review of ../main's inherited field handling

- Rebuilt both engines from `../main` commit `c1ce0f4f166dad57cd18aa684ded2f701ec02299` and ran the exact `type Example = typeof object.property;` input with the pinned TypeScript grammar. Its packed engine returns null for `object` and `property` on `type_query`; upstream returns the corresponding grandchildren. Squat's sparse exceptions reproduce both upstream results. The packed engine's comment claiming inherited fields are reconstructed identically has a gap at alias-visible boundaries.
- `../main` resolves direct fields and ambient hidden-splice inheritance during construction, then searches only direct visible children at lookup. It does not have an equivalent sparse lookup section. Blind descendant search is not a replacement: for `object.property;`, the enclosing expression statement correctly has neither field.
- Keep the sparse extension. Clarification to the earlier wording: preserving additional lookup information is necessary for compatibility, but the 40-byte header and three-u32 encoding are design choices, not uniquely necessary. The exact repro uses two records (24 bytes), plus the default layout's eight-byte per-tree header increase. The earlier 48-byte sample total counted exception records only.
- The detailed comparison and reproducible public-API probe are in `lib/squat/experiments/field-lookup-review.md` and `field-lookup.c`. No runtime or format change was made as part of this review.

### Potential upstream classification: fields and empty hidden leaves

- Added entries 7 and 8 to `../potential-upstream-bugs.md` at the human's request. Field lookup is a confirmed runtime/metadata inconsistency with unresolved intended semantics: the pinned TypeScript `node-types.json` declares no fields for `type_query`, while lookup crosses its alias-visible child and exposes two grandchildren. Our earlier finding establishes exact-compatibility differences, not that upstream's semantics are necessarily correct.
- Strengthened the hidden-leaf seek diagnosis with a fresh vendored mainline build. Valid CSS `a b {}` has a hidden `_descendant_operator [2,2)` before the visible `b` node. All four byte/point, named/unnamed empty-range seeks at 2 return `descendant_selector [0,3)` instead of `tag_name [2,3)`. Nonempty range 2..3 returns the tag correctly.
- A diagnostic-only patch in an isolated build skips hidden zero-width raw leaves during descent and fixes those four results without changing the other sampled positions. The source patch and probe are preserved under `lib/squat/experiments/`; neither runtime was modified. Empty hidden tokens themselves can be legitimate; stopping a smallest-visible-node search at one is the suspected bug. The user's policy of counting and ignoring seek differences remains in force.
