**Rust optimizations relevant to the C implementation — source audit, 2026-09-20**

Direct repacking and cheaper navigation of known-valid nodes are implemented and measured. Next is specialized query deduplication. There are also smaller opportunities in range handling, metadata, and the C-backed Rust scan facade. Most of the larger query and scan algorithms are already shared. Query cursor reuse was implemented, measured, and reverted after regressions.

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
| Deferred after measurement | Retain the query traversal cursor between executions | Rust retains its parent stack. Two C versions eliminated warmed-execution allocations but slowed general query matches by 1.76% and 3.13% on the 264-file cloud corpus. Neither is retained; see the results below. A different integration would need fresh evidence. |
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

Recommended order after repacking and navigation: deduplication specialization/outlining; range-support caching and metadata access; then the small facade changes. Cursor reuse is deferred after the measured regressions below. Keep each change separable so later measurements can attribute its effect. Leave the experimental kernels and packing redesign for targeted investigation when benchmarking is appropriate.

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

**Navigation and query cursor reuse — 2026-09-20**

Commit `9e3da8376` implements known-live navigation and the constant waste offset. A Google Cloud comparison against `eeadedf53` used all 264 selected files across 11 languages. The baseline already includes encoded symbol comparisons and local child-seek scanning; this run does not isolate those earlier changes.

Times sum one representative operation per file. Each value averages two process medians, each from five samples normalized by iteration count. Speedup is before / after.

| Operation | Before, ms | Navigation, ms | Speedup |
| --- | ---: | ---: | ---: |
| Forward preorder | 9.510 | 2.227 | **4.27×** |
| Reverse preorder | 21.277 | 3.911 | **5.44×** |
| Cursor traversal | 21.446 | 14.551 | **1.47×** |
| Traversal with attributes | 32.911 | 24.510 | **1.34×** |
| Query matches, fresh cursor | 269.829 | 253.319 | 1.07× |
| Query matches, reused cursor | 269.437 | 252.241 | 1.07× |
| Query captures, fresh cursor | 570.635 | 552.609 | 1.03× |
| Query captures, reused cursor | 487.858 | 472.131 | 1.03× |

Query workloads use the general engine with `(_ (_) @child) @parent`. Fresh cases create and destroy the query cursor each iteration; reused cases retain it after warmup. Parsing, packing, query compilation, and correctness checks are outside timing. This isolates traversal and cursor reuse; it does not measure the default direct planner or the grammar highlight-query suite.

The native harness and libraries use GCC 15.3.0 at `-O2 -g`, with assertions enabled. This differs from the older Cargo-release comparison. Runs use the same `e2-standard-4` VM, CPU 1 affinity, benchmark/activity locks, alternating build order, and reversed file order on alternating passes. Calibration targets 5 ms per sample after at least 1 ms of calibration, capped at 10,000 iterations. Fast cases can produce shorter samples.

The first run compared baseline, navigation, and cursor reuse (`646c937d4`) in forward/reverse order: 1,584 successful commands, 12,672 workload rows, and 63,360 samples. All 3,179 downloaded artifact hashes verified. Node counts, slab sizes, and result checksums matched across all builds and repetitions. The [report](build/navigation-query/cloud/report.md) and [formatted tables](build/navigation-query/cloud/report.html) retain the complete results, language breakdowns, repetition checks, and provenance. Raw inputs, binaries, harness, source archive, patches, and test logs are under `build/navigation-query/`.

Native unit, supertype, and parser checks passed, as did the alternate 32-slot/64-byte-alignment build, native comparisons across all 11 languages, and eight Rust binding tests for each change. The JSON query suite covered matches, captures, ranges, limits, removal, and optimization modes. ASan/UBSan passed navigation and cursor lifetime/fault-injection checks. The warmed cursor test observes zero allocation attempts and covers freed previous trees, invalid execution recovery, and direct-plan fallback.

Cursor reuse passed those checks, but did not improve these query workloads. The initial implementation added 1.76% to reused match time. Inspection found that GCC outlined the reset helper; a second version used an explicit inline helper with an early reuse return. A separate navigation / revised / revised / navigation comparison covered the same 264 files:

| Query operation | Initial reuse: time increase | Revised reuse: time increase |
| --- | ---: | ---: |
| Matches, fresh cursor | 1.60% | 3.08% |
| Matches, reused cursor | 1.76% | 3.13% |
| Captures, fresh cursor | 0.71% | 1.78% |
| Captures, reused cursor | 0.33% | 1.30% |

Each percentage compares against the navigation-only build within its own run. The follow-up completed 1,056 commands, producing 8,448 workload rows and 42,240 samples; all downloaded hashes and cross-build checksums verified. Same-build median repetition ratios ranged from 0.997 to 1.002. Its binaries, patch, raw results, and analysis are in `build/navigation-query/reset-followup/`.

Fresh-cursor cases also regressed, so allocation retention alone does not explain the timing. Code generation/layout remains a hypothesis, not an established cause. The final branch restores the navigation-only query implementation; the experiment and its allocation-specific test remain in commit `646c937d4`. These narrow `-O2` results do not establish how another integration, workload, or release build would behave.

Next: deduplication specialization/outlining, then range-support caching and metadata access.

**Merged newtypes work**

Merge `cb39d71f1` brings in `rust-core` through `48126eec5` without conflicts. The final `lib/squat` sources match the measured navigation commit exactly. Post-merge checks passed: 16 binding tests across both implementations, the Rust navigation comparison, four Rust/C query comparisons, native unit/supertype/parser checks, and the native JSON query suite. Logs are under `build/navigation-query/`. The separate `rust-core` worktree was not modified.

**Range support and metadata access — local experiments, 2026-09-22**

Commits `72a2e388f` and `1e00caee2` implement two changes:

- Cache range eligibility in `SQQuery`, copy it with the query, and refresh it
  after pattern removal. Range setters remain effective during execution.
  Removing rootless entries can enable ranges; alternatives in retained steps
  still block them, preserving the existing behavior.
- Read symbol names and named flags through internal inline accessors. Validated
  node symbols index the language's existing tables, including aliases. The two
  built-in error symbols retain their special handling. No new table or public
  API change is needed.

The baseline is `721c5ff8e`, adding small-state deduplication to `9d7a17168`.
Range caching was measured against that baseline; metadata access was measured
against range caching. These runs do not establish a deduplication gain.

Local pilot: Core Ultra 7 165U, CPU 2 affinity, GCC 15.3.0, `-O2 -g`, assertions
enabled. One JSON and one C corpus file, before/after/after/before order, five
samples targeting 30 ms each. Timings exclude parsing, packing, and query
compilation. Checksums agree across binaries and repetitions.

The range experiment repeats `(_ (_) @child) @parent` once or 16 times, with
general/planned execution, fresh/reused cursors, and matches/captures. Bounded
byte ranges exclude the first and last source byte. Aggregating the four query
operations per file and execution mode, 16-pattern bounded queries improve
1.12–1.17×; single-pattern queries improve 1.00–1.04×. Unrestricted query totals
across both files stay within 1%. This isolates repeated eligibility checks;
it is not a representative highlight-query benchmark.

The metadata experiment uses the same files with one unrestricted pattern.
Attribute traversal improves 1.26× in aggregate. Query totals improve 1–4%,
but traversal controls also vary by a few percent; those small gains are
inconclusive. Broader corpus and release-build confirmation remain outstanding.

Native unit/supertype/parser checks, native JSON/C query checks, and all four
Rust/C query comparisons pass with both changes. Added tests cover query copying,
repeated pattern removal, capture removal, byte/point/containing ranges, range
changes during execution, aliases, and both built-in error symbols.

Harness, binaries, raw samples, and summaries are in `build/query-range/`.
`run.py` reproduces the range comparison; `metadata.py` compares metadata access.
The next candidates are the small C-backed scan predicate changes, followed by
internal cursor/navigation work if profiles justify it.

**Flat scan predicates — local experiments, 2026-09-22**

Commit `4e185fdc5` copies `FixedKindValues` from `FixedKindIds::flat`, matching the
Rust core. Flat comparisons no longer borrow the predicate containing mutable
posting cursors. The public API and indexed traversal are unchanged.

Two changes were measured independently against `1e00caee2`, including range
caching and direct metadata access. Passing only `KindStrategy`
to the dynamic count fallback did not establish a useful gain and was reverted.

The fixed-value confirmation uses 11 files/languages and 28,299 nodes on the same
Core Ultra 7 165U, CPU 2 affinity, portable Cargo release, Rust 1.95.0. Each profile
runs baseline/candidate/candidate/baseline with seven samples targeting 40 ms.
Parsing and packing are excluded; the harness validates scans against scalar
traversal and verifies input hashes and output counts. Speedups are baseline time
divided by candidate time, averaging the two process medians per binary.

| Workload | No index, frequent IDs | Normal index, frequent IDs | Sparse IDs |
| --- | ---: | ---: | ---: |
| Four kinds, forward nodes | 1.14× | 1.11× | 0.99× |
| Four kinds, reverse nodes | 1.13× | 1.17× | 0.99× |
| Eight kinds, fold | 0.97× | 1.01× | 0.98× |
| Sixteen kinds, forward nodes | 1.20× | 1.21× | 1.03× |
| Sixteen kinds, reverse nodes | 1.21× | 1.21× | 0.97× |
| Sixteen kinds, fold | 1.14× | 1.10× | 0.99× |

The larger enumeration gains repeated after the initial shorter screen. Sparse
results are mixed, and the eight-kind no-index fold remains about 3% slower.
Some controls and repetitions also vary by a few percent; small differences
are not conclusive. These are local repeated-corpus results, not cloud or broad
holdout measurements.

All 18 release scan tests pass, including fixed and dynamic kind sets, indexed
filters, mixed-end consumption, ranges, and storage variants. Formatting checks
also pass.

Artifacts are in `build/scan-predicates/`: isolated patches, binaries, raw samples,
and summaries. `run.py fixed confirm` reproduces the longer comparison;
`run.py dynamic` reproduces the rejected dynamic-count screen.

**Field scans, cursor access, and loading — local experiments, 2026-09-22**

Baseline: `b4b0cd2d2`. These are targeted before/after experiments, not a fresh
Rust/C comparison. Retained only the C cursor-access change; Rust scan and loader
implementations are unchanged.

C queries now read cursor node, parent, and depth through shared internal inline
accessors. The cursor definition moves to the private header; public accessors
use the same helpers. Null handling, allocation, traversal, and ownership remain
unchanged. This does not reintroduce the rejected cursor-reuse experiment.

The existing two-file JSON/C query pilot uses one unrestricted parent/child
pattern, general/planned execution, and fresh/reused query cursors. Speedups below
aggregate both files and execution modes. Two batches reverse process order:

| Operation | Initial | Reversed-order confirmation |
| --- | ---: | ---: |
| Matches, fresh cursor | 1.052× | 1.040× |
| Matches, reused cursor | 1.048× | 1.036× |
| Captures, fresh cursor | 1.125× | 1.018× |
| Captures, reused cursor | 1.143× | 1.016× |

The match improvement repeats. The larger capture gain does not. Unchanged
preorder traversal moved from 0.94× initially to 1.01× on confirmation; other
confirmation traversal controls are within 1%. Broader confirmation is pending.

Rust field trials use the 11-file, 28,299-node scan corpus:

- Composing single-field comparisons for arrays of up to four fields improves
  the targeted two/four-field operations by roughly 1.38–2.02×. Plain preorder
  enumeration loses 18–22% throughput in those binaries, including a second build
  without the loading change. The cause of that unrelated regression is not
  isolated, so this version is rejected.
- Copying fixed-field comparison values into flat scans is essentially neutral
  on the targeted two/four-field operations and is also rejected.

Rust loading trials use the same 11 files, including full/borrowed loading and
compact copying as controls:

- Omitting leaf entries from the validation stack gives 1.03–1.06× for safety
  and backed loading initially, but only 1.01–1.02× on confirmation while full
  and borrowed loads regress. Not retained.
- A 64-entry inline validation stack with a reusable heap fallback gives
  0.92–0.93× across loading operations. Not retained. All validation checks and
  leaf entries were preserved in this separate trial.

Runs use the Core Ultra 7 165U pinned to CPU 2. Rust binaries use portable Cargo
release with Rust 1.95.0; the native C harness uses GCC 15.3.0 at `-O2 -g`.
Scan/loading runs use seven samples targeting 30 ms; cursor runs use five.
Parsing, conversion, and query compilation are outside timing. Raw results,
binaries, scripts, and the last trial patch are under `build/nonconversion/`.

Native unit/supertype/parser checks, JSON/C query comparisons, and 12 release
Rust boundary/query/storage tests pass. A new corruption test perturbs node
columns and compares safety-loader acceptance with C for shallow, wide, deep,
and deep-sibling trees, with and without points. Cursor tests also cover null
access and parent reads. Formatting and diff checks pass.

## Rust scan inlining and fold dispatch — 2026-09-22

Baseline: `a411b40558ea4`. The preceding [Rust/C comparison and profiling](build/rust-vs-c-current/investigation.md)
identified fixed-field scans and dynamic-kind folds as targets.

Two changes are retained:

- Inline the three `Id::raw` implementations across crate boundaries. The
  four-field count kernel previously called an identity accessor four times per
  full group, spilling SIMD intermediates around the calls. Its targets are now
  broadcast once before the loop; normal group processing contains no calls.
- Let a source specialize folding after `Nodes` drains its current fragment.
  Flat dynamic-kind folds select empty, single-, two-, or four-kind kernels once
  per scan. Indexed scans preserve their existing cursors and traversal; larger
  sets keep the generic strategy. Other sources retain the existing slot loop.

The final comparison uses all 264 files and 11 languages. Each profile runs
original Rust / C / final Rust / final Rust / C / original Rust, with seven
samples targeting 30 ms for each of 31 selected workloads. Workload order
reverses on alternate processes. The table reports means of process medians;
speedups above 1 favor the new Rust build. Full 267-workload frequent-selection
comparisons were also used to evaluate each candidate independently.

| Operation | Rust before / after | Without indexes | C / final Rust |
| --- | --- | --- | --- |
| fixed_field_1.nodes | 1.092× | 1.101× | 1.046× |
| fixed_field_2.nodes | 1.244× | 1.242× | 1.106× |
| fixed_field_2.count | 1.268× | 1.260× | 1.185× |
| fixed_field_4.nodes | 1.389× | 1.364× | 1.120× |
| fixed_field_4.count | 1.779× | 1.753× | 1.078× |
| kind.fold | 1.261× | 1.218× | 1.162× |
| dynamic_1.fold | 1.271× | 1.221× | 1.177× |
| dynamic_2.fold | 1.193× | 1.174× | 1.141× |
| dynamic_4.fold | 1.119× | 1.108× | 1.083× |
| dynamic_8.fold | 1.111× | 1.101× | 1.047× |
| dynamic_16.fold | 1.100× | 1.088× | 1.054× |

Dynamic-kind `next` iteration is essentially unchanged and remains about 3–6%
behind C in this run. Copying its prepared strategy into flat iterations was
tested separately and rejected: multiple dynamic-kind cases lost 10–15%, with
larger losses in some reverse cases.

### Control regressions and assembly

This is not an across-the-board benchmark improvement. Final/original throughput
is 0.805× for `preorder.nodes`, 0.703× for `preorder.rev.fold`, 0.847× for
`range.fold`, and 0.930× for `range_field.dynamic_4.count`. The no-index run
repeats these losses. Unfiltered preorder count is neutral; forward folding
improves by 1.126×.

Assembly inspection found identical normalized instruction sequences in the
unfiltered preorder-node control before/after accessor inlining, and in the
reverse-fold control before/after fold specialization. Their addresses changed;
the reverse-fold hot loop moved across a 64-byte boundary. Code placement is a
plausible contributor, not proof of the entire cause. These are real losses in
the measured binaries, even though the inspected loops do no additional work.
No benchmark-specific padding or compiler flags were added. Other control losses
have not been isolated; cross-machine or consumer-specific confirmation is still
needed before claiming a general improvement.

The full benchmark executable's text grows by about 3.2 KiB versus the baseline.
All 20 release scan/pattern tests pass, including partial consumption, both
directions, subtree/range restrictions, composed filters, indexed scans, and
storage variants. Timed scan output counts match across the measured binaries;
formatting and diff checks pass.

Measurements use the local Core Ultra 7 165U, CPU 2, portable Cargo release,
Rust 1.95.0 and GCC 15.3.0, without concurrent builds. They are not cloud timings.
`perf` user-cycle samples and annotated assembly verify the changed hot paths.
Sources, binary hashes, raw samples, repeat ratios, rejected-candidate results,
and scripts are under `build/rust-scan-optimizations/`; the final tables are
`final-frequent/summary.json` and `final-no-index/summary.json`.
