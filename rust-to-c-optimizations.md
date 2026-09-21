**Rust optimizations relevant to the C implementation — source audit, 2026-09-20**

Start with direct repacking, cheaper navigation of known-valid nodes, query cursor reuse, and specialized query deduplication. There are also smaller opportunities in range handling, metadata, and the C-backed Rust scan facade. Most of the larger query and scan algorithms are already shared.

This audit compares the committed implementations at `9800d0c3fdd9`. The subsequent merge of local `main`, `c07d1b2a7`, changes documentation only. The working tree's ongoing typed-ID/API edits are not included in the performance conclusions. No compilation, tests, or benchmarks were run for this audit; no implementation changes were made.

Implementation and subsequent measurements are recorded below under **Direct repacking completed**. They live on the separate `c-optimizations` branch, based on `c07d1b2a7` before the newtypes changes.

The [saved Google Cloud report](build/rust-core-comparison/gcp-medium-20260920-124652/report.md) measures the whole implementations on 264 files and 11 languages. Its ratios help prioritize investigation; they do not measure the benefit of individual proposed C changes.

| Operation | C time / Rust time | Implication |
| --- | ---: | --- |
| Repack | 43.20× | Clear algorithmic difference: C revalidates an existing tree. |
| Cursor traversal | 2.50× | Investigate navigation checks, calls, and cursor representation. |
| Traversal with attributes | 1.53× | Metadata access is another plausible contributor. |
| Query matches / captures | 1.29× / 1.32× | Investigate query loop structure and internal navigation. |
| Byte / point seek | 1.18× / 1.18× | Most search algorithms already agree. |
| Full copied / borrowed loading | 1.08× / 1.09× | Inspect full-validation overhead; safety-only loading favors C slightly. |
| Group scans | 1.18–1.40× median, by profile | Includes Rust code on both sides; some enumeration cases favor C. |
| Initial packing, cold / reuse / trim | 0.89× / 0.88× / 0.88× | Rust takes about 12–13% longer. Do not transplant its packer wholesale. |

Except for group scans, ratios above compare summed representative times. Capture timing excludes two Go validation failures shared by both backend runs. Measurements used the default portable build and default features; the experimental query/seek features were disabled.

**Concrete candidates in the C core**

Priority reflects the clarity and scope of the source difference, not a promised speedup.

| Priority | Change | Rust behavior and C opportunity |
| --- | --- | --- |
| First | Copy directly into a compact owned tree | Rust `Tree::repack` allocates the compact destination and calls `copy_compact_into`. C `sq_tree_repack` calls `sq_tree_from_bytes`, including full node/index validation, then `sq_resize`. Reuse C's existing compact-copy machinery without loading the tree again. This also avoids an intermediate oversized copy when capacity must shrink. |
| Done | Use cheaper transitions from known-live slots | C now follows Rust's live-slot predecessor rule, reading waste only across group boundaries. Preorder, first-child, and sibling movement construct established live nodes directly; reverse preorder checks only the resulting tree boundary. Arbitrary-slot normalization and the checked public constructor remain separate. |
| Done | Retain the query traversal cursor between executions | C now resets an existing traversal cursor and retains its parent stack, like Rust. Null-node execution deletes the traversal cursor. Fault injection verifies a warmed general-query execution allocates nothing; lifetime and fallback checks cover freed previous trees, invalid executions, and restoration from planned execution. |
| First | Separate small-state and indexed deduplication loops | Rust dispatches once to `compare_states::<false>` or `::<true>`. C tests `capture_comparison_index.size` within the pairwise loop and carries hash-bucket/bitmap state even on the small-state path. Give C separate specialized comparison kernels. |
| First | Outline the deduplication pass | Rust explicitly marks `deduplicate` as `inline(never)` to keep its large working set out of the per-node matcher. C embeds the pass in `sq_query_cursor__advance`. A separate helper can reduce register pressure and hot-function size; actual generated code still needs later inspection. |
| Done | Use the constant waste-column offset | C layout construction and waste access now share `SQ_WASTE_OFFSET`, derived from the header size and configured column alignment, like Rust's `Layout::WASTE`. Default and 32-slot/64-byte-alignment builds pass navigation boundary tests. |
| Next | Make internal navigation and attribute access visible to the caller | Rust reads packed columns and keeps query position/parents directly in its cursor. C queries call through `QueryTreeCursor` → `SQCursor` → node functions, with `node.c`, `cursor.c`, and `query.c` built separately. Internal inline helpers and an embedded internal cursor can remove calls, indirection, and a cursor-object allocation. Preserve the public API. |
| Next | Cache whether a query supports bounded ranges | Rust computes `Program::supports_ranges` during preparation and updates it after pattern removal. C's `query_execution_supported` scans every step and pattern-map entry on every `next_match`/`next_capture` with an active range. Replace repeated scans with cached query metadata. This particularly targets bounded queries, which the medium benchmark did not isolate. |
| Next | Cache execution-wide range/size facts | Rust stores `unrestricted` and `total_slots` in `QueryExecution`. C repeatedly checks both included/containing ranges and reads the tree's slot count. Cache these once per execution, refreshing range flags in setters. Root error status and root scan masks are already cached in C. |
| Next | Read prepared symbol metadata directly | Rust `GrammarView::named` reads a byte flag table; symbol/field names come from retained tables. C `sq_node_is_named`, query current-status construction, and `sq_attributes_finish` call language accessors. Add internal direct accessors or prepared flags. If adding flags, Rust's native grammar code fills them during the existing metadata pass and shares the allocation with the three existing `u16` tables. |
| Next | Keep encoded IDs throughout presence validation | Rust `validate_presence` directly reads `data.symbol_index(slot)`. C decodes with `sq_node_symbol` and immediately encodes again with `sq_encode_symbol`, once per node. Use `sq_node_symbol_id` after the existing node-validation pass. Retain every index integrity check. |
| Done | Compare encoded IDs in unindexed group membership | C now follows Rust `group_has_symbol`: compare stored IDs against a target remapped once. Public-ID validation rejects invalid IDs before they can match the two encoded error IDs, with and without a presence index. Unit coverage spans combined/separate representations, aliases, errors, sparse/bitmap indexes, and unused lanes. No isolated speedup measured yet. |
| Done | Scan locally before moving a cursor to a matching child | C's byte/point child seeks now follow Rust `goto_child_matching`: scan local nodes, test bytes before decoding points, and push the parent only after finding a match. Failed seeks allocate nothing; allocation failure leaves the cursor unchanged. Binding tests and comparisons against mainline cover child indexes, empty nodes, and omitted points; fault injection checks allocation and ancestor preservation. No isolated speedup measured yet. |
| Later | Avoid a linear supertype-ID search per membership check | Rust `Node::has_supertype` binary-searches sorted supertype IDs; C scans them linearly. A binary search may help larger grammars. C also already owns `grammar->supertype_indexes`, so a bounds-checked lookup is another candidate without a new table. Neither is established as faster for tiny supertype sets. |
| Later | Skip preceding captures in an inner loop | Rust `first_in_progress` walks all out-of-range captures for one state before continuing. C `sq_query_cursor__first_in_progress_capture` advances one capture, decrements the outer index, and re-enters the state/list setup. Keep the current state/list in hand while skipping. This targets capture-heavy bounded queries. |
| Later | Delay progress-only byte decoding until polling | Rust's timeout check performs work at its polling boundary. C's general query advance decodes `current_byte_offset` every iteration whenever a progress callback is installed, though it invokes the callback only periodically. Move progress-specific decoding to the callback site if no other consumer needs it; preserve callback and cancellation behavior. C's scan polling helper already does this. |

Source locations for those comparisons:

| Area | Rust | C |
| --- | --- | --- |
| Repack, waste, validation | [storage.rs](crates/squatter-rust/src/storage.rs): `repack`, `copy_compact_into`, `previous_slot`, `waste`, `validate_presence` | [index.c](lib/squat/index.c): `sq_tree_repack`, `validate_presence`; [slab.c](lib/squat/slab.c): `sq_tree_copy_compact`, `sq_resize`; [internal.h](lib/squat/internal.h): `sq_group_waste` |
| Navigation and metadata | [node.rs](crates/squatter-rust/src/node.rs): `next_preorder`, `next_sibling_including_empty`, `has_supertype`, `Cursor`; [native.rs](crates/squatter-rust/src/native.rs): `GrammarView`; [grammar.c](crates/squatter-rust/native/grammar.c): `grammar_new` | [node.c](lib/squat/node.c), [cursor.c](lib/squat/cursor.c), [attributes.h](lib/squat/attributes.h), [pack.c](lib/squat/pack.c): `grammar_new`; [build.rs](crates/squatter/build.rs) |
| Query execution | [query_exec.rs](crates/squatter-rust/src/query_exec.rs): `execute`, `deduplicate`, `compare_states`, `first_in_progress`, `poll` | [query.c](lib/squat/query.c): `query_tree_cursor_reset`, `sq_query_cursor__advance`, `query_execution_supported`, `sq_query_cursor__first_in_progress_capture` |
| Query preparation | [query_plan.rs](crates/squatter-rust/src/query_plan.rs): `Program::new`, `disable_pattern` | [query.c](lib/squat/query.c): `sq_query_disable_pattern`, range setters; [query_plan.c](lib/squat/query_plan.c) |

For repacking, keep full validation for externally supplied slabs. The fast path must still preserve grammar ownership, optional columns, index bytes, canonical padding, compact capacity, overflow handling, allocation failures, and independent ownership for borrowed/backed input trees. C already has `sq_tree_copy_compact`; the missing piece is using it to construct the owned result directly.

For navigation, distinguish normalization of an arbitrary slot from movement from a known-live node or subtree boundary. The cheaper predecessor rule does not replace the general normalizer. Keep checked public construction and null handling, and preserve empty-node sibling order.

For query changes, preserve stable ordering, longest-match rules, dirty-pattern propagation, tombstone compaction, limits, and capture lifetimes. Retaining a cursor must also handle invalid/empty executions and direct-plan fallback without touching a previous tree that may have been freed. C allows configuration changes during execution, including containing ranges; Rust's exclusive execution borrow removes some of that complexity. Cached flags in C need explicit invalidation.

For metadata, preserve aliases and the two built-in error symbols. Coallocating a new flag table saves an allocation relative to adding it separately; C currently has no such extra flag allocation to eliminate.

**Candidates in the C-backed Rust scan facade**

The full scan suite compares [crates/squatter/src/scan.rs](crates/squatter/src/scan.rs) with [crates/squatter-rust/src/scan.rs](crates/squatter-rust/src/scan.rs). These opportunities benefit the C-backed product, but are not all changes to `lib/squat/*.c`.

| Priority | Difference | Transfer |
| --- | --- | --- |
| Next | Fixed comparison arrays are copied into flat iteration | Rust `FixedKindIds::flat` returns its small `FixedKindValues` by value; the C-backed facade returns `&self.values`. Copying exposes independence from mutable posting-cursor state and can make IDs easier to retain in registers. This is the specific Rust change in `4e67e1080`; both sides already separate flat predicates generally. |
| Next | Dynamic flat-count fallback discards index state | Rust `KindIds::count_flat` passes only `strategy` to the generic fallback. The facade passes `self`, including the owning predicate's index and cursor fields. Pass only the comparison strategy, as the specialized small-set branches already do. |
| Next | Singleton predicates cache column parameters | Rust's internal `KindId` caches target, symbol offset, and shift. The facade stores the target and fetches layout fields through the group on each use. This is particularly relevant if its large copied metadata view is replaced with a borrowed descriptor. Measure independently: singleton enumeration still favors C in the medium run. |
| Larger change | Groups borrow a compact shared descriptor | Rust `Columns` holds a node referring to the existing tree descriptor. The facade copies `ColumnLayout`, slab/table slices, and root into every `GroupRef`, with extra FFI setup for index/point views. Expose a stable immutable scan descriptor once per tree and borrow it. Keep lifetimes valid for owned, borrowed, and externally backed trees; avoid replacing copies with an allocation per scan/group. |
| Conditional | Expose fixed layout facts to specialization | Rust knows `GROUP_SIZE` at compile time and its storage readers use the validated representation directly. The facade carries a runtime group shift and checked slice readers. A dispatch once into layout-specific kernels could expose the same constants. Pure C already knows `SQ_GROUP_SIZE`; any unchecked facade access requires a separately established bounds invariant. |

The facade's dynamic layout is deliberate: alternate group sizes and column alignment remain supported. Do not hardcode the default 16-slot layout into its public bridge. Borrowed metadata also trades copies for dependent loads; the existing singleton regressions are reason to treat this as a measured design change, not an automatic win.

**Smaller ideas and disabled experiments**

| Idea | Assessment |
| --- | --- |
| Packed point keys in query ranges | Rust stores row/column bounds as ordered `u64` keys. C query ranges compare `TSPoint` components, although C seeks already use packed keys. This is a small code-generation candidate; preserve strict/inclusive endpoints and sentinel values. No isolated gain is established. |
| Commit packing bounds inside the helper | Rust `Builder::extend` stages extrema locally and commits them on success, rather than returning two large value records. C `group_fits` writes caller-provided temporary bounds, then the caller assigns them. The Rust change reduced its own metadata traffic, but C's output-pointer form may already compile efficiently. Keep rejected candidates from changing accepted bounds. Low priority given Rust's packing regression. |
| `typed-query-scan` | Small query root unions use typed kind masks instead of the control scan. Potential model for C SIMD root matching, but prior Rust results are mixed. Default off. |
| `typed-presence-scan` | Reuses kind/field scan predicates for bounded descendant presence checks. Default off with mixed results. Preserve the 256-position budget, negative-interval cache, cooldown, error bypass, and cancellation. Kind and field must match the same descendant. |
| Query-only predicate preparation | Rust's committed `FixedKindIds::prepare_columns` encodes IDs without sampling the presence index or initializing posting cursors. Relevant if C query execution adopts shared predicates; the existing C specialized query masks do not need that extra setup today. |
| `typed-seek` | Uses shared coordinate masks inside singular descendant lookup. Some point cases improved, but byte seeks consistently regressed in saved experiments. Default off. Retain indexed search, early candidate return, empty-node tie handling, and the long-distance point fallback. |
| Direct-plan fusion or sibling-mask execution | Discussed in the design, not implemented Rust optimizations with demonstrated benefit. Do not count them as transferable wins from this branch. |

The experiment assessments come from [rust-core-results.md](rust-core-results.md), particularly its query, scan, and packing discussion; design-only proposals are in [rust-core-design.md](rust-core-design.md). These are older, narrower measurements than the cloud comparison.

**Already shared, or specific to the Rust implementation**

| Apparent opportunity | Audit result |
| --- | --- |
| Borrow query steps instead of copying them | Rust was corrected to borrow native records. C already accesses its owned arrays through pointers; no analogous conversion pass is needed. |
| Compact query records and finished-state key storage | Both retain compact step/state records and reuse finished states' NFA-position bytes for ordering keys. Rust's `NonNull` shrank its own `Option<Node>` to 16 bytes; C already encodes absence using the node's null tree pointer. |
| Capture pooling, copy-on-write, direct buffer pointers | Already in C: free lists, shared buffers, pointer refresh after growth, prefix/hash/set rejection, and exact containment checks. |
| Lazy capture heaps | Already in C: finished states get ordering keys when capture streaming needs the heap. Completed-match iteration does not eagerly pay for those keys. |
| Cheap state sorting and compaction | Already in C: compare depth/pattern before capture metadata, stable insertion sorting, dirty-pattern skips, depth exit bounds, and avoid rewriting unmoved survivors. |
| Large-state dedup indexes and staged branching | Already in C: prefix/hash buckets, capture-set blocks, group/disjointness exits, and staging a large pending suffix. Specializing and outlining the loop remain additional candidates. |
| Query root skipping and direct execution plans | Already in C, including merged root filters, word-wide comparisons prepared at execution start, root error caching, bounded presence checks, cache/cooldown, and local/anchored direct plans. |
| Incremental capture/pattern disabling | Already in C. Capture removal retains plans; pattern removal clears root masks and rebuilds affected pattern-map slices. |
| Defer unsupported direct-plan root allocation | Already in C: root masks are allocated after support checks. Rust's quantifier-view reservation only optimizes its native-to-Rust adapter; C has no equivalent extra view array. |
| Seek algorithms | Both have group binary search, byte SIMD start masks, an immediate candidate check, empty-boundary fallback, and the long-distance point fallback. Rust's const-generic byte/point split is not a new algorithm; C has separate functions. |
| Scan range pruning and index traversal | Both Rust scan implementations have two-sided range clipping, subtree pruning, bounded sparse/bitmap traversal, posting hints, density gating, SIMD ID matching, and scalar handling of sparse candidates. |
| Flat predicates, specialized counts, set intersection | Already ported/shared, including the direct `Filtered::count` forwarding fix at the measured revision. The narrower flat-state differences above remain. |
| Colocated tree descriptor/slab; avoid zero-before-copy | Already in C's `allocate_tree` and copied-load path. Compact copying already initializes padding without zeroing and then overwriting the entire payload. |
| Packing scratch reuse, optional columns, small tail shrink | Already in C. Rust's extra 32-entry inline boundary stack compensates for its split traversal/encoder; C stores subtree boundaries in traversal frames and has no equivalent extra boundary vector. C also already passes emit records by pointer. |
| Direct-parser preparation and reuse | Both retain native tree-feller preparation and parse scratch. The Rust branch provides no established direct-parser speedup to port. |
| Typed IDs, ownership wrappers, compiler trust boundary | Useful Rust correctness/interface choices, not evidence of faster C execution. Do not remove C's recoverable errors or external-input validation to imitate Rust's allocation or ownership model. |

Recommended order: direct repack; known-live navigation plus constant waste offset; query cursor reuse; deduplication specialization/outlining; range-support caching and metadata access; then the small facade changes. Keep each change separable so later measurements can attribute its effect. Leave the experimental kernels and packing redesign for targeted investigation when benchmarking is appropriate.

**Direct repacking completed — 2026-09-20**

Commit `820a3d577` implements the first candidate. [slab.c](lib/squat/slab.c) now allocates the compact owned tree directly and shares the serialization writer with `sq_tree_copy_compact`. This removes full validation and the intermediate oversized copy from repacking. External loading retains its validation checks; grammar retention, borrowed-input independence, optional columns/indexes, padding, and allocation errors are preserved.

The Google Cloud before/after run used the same 264 inputs and 11 languages as the original comparison. Each binary ran twice in before/after/after/before order, with five samples per operation and CPU affinity. Times below sum one representative operation per input, including destruction. Speedup is before / after. These results compare C before and after this change.

| Operation | Files | Before, ms | After, ms | Speedup | Median file speedup |
| --- | ---: | ---: | ---: | ---: | ---: |
| Repack | 264 | 34.898 | 0.819 | **42.60×** | **36.89×** |
| Compact copy | 264 | 0.792 | 0.798 | 0.99× | 0.99× |
| Full copied loading | 264 | 31.464 | 31.570 | 1.00× | 0.99× |

All 264 files improved on repacking, from 1.81× to 73.11×. The two control totals increased by 0.73% and 0.34%, respectively. Smaller profiles used the median-sized and largest input per language:

| Repack source | Files | Before, ms | After, ms | Speedup |
| --- | ---: | ---: | ---: | ---: |
| Without points | 22 | 7.115 | 0.130 | 54.53× |
| Without presence indexes | 22 | 5.876 | 0.156 | 37.55× |
| Already compact | 22 | 7.009 | 0.171 | 41.10× |

Machine: `squatter-benchmark`, `e2-standard-4`, `us-central1-a`, Intel Xeon at 2.20 GHz. Both binaries use portable release builds with Rust 1.95.0 and GCC 15.3.0. The baseline is `9800d0c3fdd9`; its code and dependency manifests match the implementation commit's parent. All 176 commands succeeded, yielding 3,432 result rows; all 360 downloaded artifact hashes verified. Source, grammar, query hashes, and slab sizes matched between binaries.

The [full report](build/repack/cloud/report.md) and [formatted tables](build/repack/cloud/report.html) include language breakdowns, repetition checks, methodology, and limitations. Raw results and both binaries are in `build/repack/cloud/`; the source archive, patch, analysis scripts, and local test logs are in `build/repack/`. This run isolates repacking and its controls; the earlier report remains the comparison for other operations and Rust versus C.

Local validation passed: native unit/supertype/parser checks, allocation-failure recovery, ASan/UBSan checks, a 32-slot/64-byte-alignment layout, 52 Rust binding/persistence tests (3 ignored), and native comparisons on 22 files across all 11 languages. The broad Rust suite was stopped during compilation; only the focused suites are counted. All benchmarking ran on Google Cloud.

Known-live navigation, the constant waste-column offset, and query cursor reuse are implemented. Next: deduplication specialization/outlining, then range-support caching and metadata access.
