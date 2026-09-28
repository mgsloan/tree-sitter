# Squatter test and benchmark review

- Scope: source review of `crates/squatter`, `crates/squatter-bench`, and their dedicated runners as of 2026-09-28. Excludes `crates/persistence`. Core slab loading, language caches, and side-data attachment remain included, even where a test name says “persistence.” No tests or benchmarks were changed or run for this review.

- Inventory totals: 112 Rust tests and 15 doctests in `squatter`, five native test entry points called by those Rust tests, seven tests in `squatter-bench`, and one dedicated runner test. Benchmarks comprise eight corpus workloads, 16 lifecycle workloads, and 267 scan workloads (291 total).

- Inventory conventions:

  - Paths are relative to the repository root. Each Rust `#[test]`, doctest, native test entry point, and named benchmark workload is listed below. Generated input matrices are described under their owning test.

  - **Share** means extract repeated setup or assertions; **merge** means preserve the assertions in a related test; **remove** identifies coverage with a concrete replacement; **keep** calls out superficially similar coverage that exercises a different failure mode.

  - Recommendations are proposals, not claims established by mutation testing. Removing a standalone test after moving its assertions saves setup, not the assertions themselves.

- Project-wide opportunities:

  - **Share:** use the existing `tests/support/mod.rs` language constructors and `parse_native` throughout integration tests. `boundary.rs`, `navigation.rs`, `storage.rs`, and `query_execution.rs` repeat the unsafe language conversion and parser initialization.

  - **Share:** move `parser.rs::assert_same_tree` into support for core/point/presence byte comparisons. Use it in parser equivalence and packing-context tests. Keep pointer identity, ownership, and semantic navigation checks explicit at their call sites.

  - **Share:** a small `pack_native(language, source, options)` helper returning native and packed trees can replace `scanning.rs::parse` and similar setup. Keep sources, options, mutations, and expectations visible in each test; do not introduce a configurable fixture framework.

  - **Share:** consolidate query snapshots and result collection. Preserve the distinction between ordered events, sorted completed matches, and deduplicated capture coverage; a universal “sort everything” helper would weaken several tests.

  - **Reduce:** ownership/loading tests often repeat full navigation and predicate suites after byte equality has already been established. Keep one comprehensive loaded-tree comparison plus focused address, lifetime, side-data, and rejection assertions elsewhere.

  - **Keep:** real-grammar integration, synthetic boundary cases, SIMD kernels, and corpus checks have different oracles and reach different paths. Similar terminology is insufficient reason to delete one layer.

  - Benchmark consolidation should share setup and registration while retaining separate timed operations. Combining operations inside one timer loses attribution. Preserve static specialization, `black_box` placement, setup/destruction boundaries, and raw samples.

  - For any timing comparison after refactoring, use identical fixed LLD section-shuffle seeds for baseline and candidate, alternate runs, and compare across seeds as required by `AGENTS.md`. This review makes no performance claims.

- `crates/squatter/`

  - `Cargo.toml` — test grammar dependencies; integration tests use Cargo discovery. No local `benches/` directory or Cargo bench targets.

  - `build.rs` — compiles native fixtures and generates subtree bindings and query flags.

    - Generated `OUT_DIR/subtree.rs` — bindgen ABI size/alignment/offset assertions. The inspected generated file uses compile-time assertions, not additional `#[test]` functions. Keep: an FFI layout mismatch can evade semantic tests on ordinary inputs.

  - `src/`

    - `lib.rs`, `native.rs`, `parser.rs`, `query_plan.rs`, `side_data.rs`, `packing/traversal.rs` — implementation files without handwritten tests or doctests.

    - `simd.rs` — `test_levels` supplies supported SIMD levels to kernel tests; no standalone test.

    - `packing.rs` — includes `tests/support/internal.rs` as its private unit-test module; those tests are inventoried under that file, not counted twice.

    - `node.rs`

      - `start_masks_match_scalar` — compares start-byte SIMD masks with scalar results at every byte threshold and 32 alignments, across supported SIMD levels.

        - **Keep:** public navigation tests use normal allocations and runtime dispatch, not this alignment/ISA matrix.

    - `storage.rs`

      - `column_copies_initialize_gaps_and_unused_capacity` — checks copied columns, padding, spare capacity, and resize output across optional-column and ID-width layouts.

      - `shrinking_respects_absolute_and_relative_thresholds` — checks exact allocation-shrink thresholds and unchanged logical layout.

        - **Keep:** wrong shrink decisions need not break any semantic tree test.

      - Doctest on `RetainedTree` — rejects replacing the inner tree independently of its retained owner.

    - `scan.rs`

      - `simd_kernels_match_scalar` — checks word-ID equality, supertype absence, and byte/word range masks across alignments and supported SIMD levels.

      - `byte_id_masks_match_scalar` — checks byte-ID equality for all wrapping byte bases and empty/partial target lists.

        - **Share:** could join the kernel test's helper organization, but retain its byte-equality cases; the existing kernel matrix does not cover them.

      - `query_masks_match_indexed_masks_without_preparing_traversal` — checks exact per-group masks before/after presence-index preparation and asserts which path is prepared.

      - `direct_supertype_masks_match_scalar_membership` — checks direct supertype bits for empty, sparse, and dense candidate masks at several widths.

      - `byte_delta_bounds_match_decoded_positions` — checks byte-coordinate filters against decoded values across delta widths, bases, and inclusive/exclusive bounds.

      - `point_delta_bounds_match_decoded_positions` — checks both point-delta directions against decoded lexicographic point bounds.

        - **Keep:** already shares `check_column` with the byte test; merging the fixtures would obscure different encodings.

      - Module example doctest — compiles a range/kind scan and reverse postorder traversal.

      - Group-lifetime compile-fail doctest — rejects using a group after dropping its tree.

    - `query_exec.rs`

      - **Share:** `error_plans_match_general_execution` and `bounded_plans_match_general_execution` repeat collection, sorting, cursor setup, and direct-path assertions. Extract a local collector taking root, configured cursor, and stream choice; retain each test's normalization policy.

      - `error_plans_match_general_execution` — compares optimized/general matches and captures on malformed JSON, every subtree, and byte bounds; verifies direct execution is selected.

      - `bounded_plans_match_general_execution` — compares direct/general query plans across subtree roots, range types, presence settings, and forced group waste; checks plan eligibility.

      - `root_search_respects_ranges_and_group_waste` — exhaustively compares internal symbol searches with a scalar oracle for physical start/end positions and target-set sizes.

        - **Keep:** public range tests cannot assert the internal search's exact stopping position.

      - `QueryExecution` live-result doctest — rejects cursor reuse while a result remains borrowed.

      - `QueryExecution` provider-borrow doctest — rejects dropping source text while execution still uses it.

      - `QueryExecution` callback-borrow doctest — rejects reborrowing callback options while execution remains live.

      - `QueryCaptures` doctest — rejects advancing a capture stream while its previous capture slice remains borrowed.

        - **Keep separately:** merging compile-fail snippets can hide an accidentally accepted operation behind a different compiler error.

    - `query.rs`

      - Module example doctest — compiles match/capture iteration, capture indexing, and removal.

      - `QueryMatch` compile-fail doctest — rejects advancing execution while its previous match remains borrowed.

    - `traits.rs`

      - Module example doctest — compiles generic `NodeLike` kind filtering and byte-length accumulation.

    - `types.rs`

      - `KindId` compile-fail doctest — rejects grammar IDs as displayed-kind filters.

      - `FieldId` compile-fail doctest — rejects a kind ID as a field filter.

      - `SquatterKindId` compile-fail doctest — rejects compact IDs as public-kind filters.

      - `ChildIx` compile-fail doctest — rejects a named-child index for ordinary child access.

      - `NamedChildIx` compile-fail doctest — rejects an ordinary-child index for named-child access.

        - **Optional removal:** this reverse-direction example adds little beyond `ChildIx`: confusing these nominal types would normally break both. Keep both if their value as API documentation outweighs five lines of duplicate negative coverage.

  - `tests/`

    - `README.md` — migrated C coverage map and query semantics notes.

    - `REVIEW.md` — this inventory and reduction review.

    - `support/`

      - `mod.rs` — language constructors, native parsing/traversal/query snapshots, and iterator-consumption checks; no standalone tests.

        - **Keep:** `check_consumption` covers partial `next` followed by specialized `count`/`fold`, not merely repeated full iteration.

      - `internal.rs` — private packing tests using synthetic grammars and direct slab access.

        - **Share:** repeated leaf emission and `Builder::finish(..., Progress::default())` can use a small local helper where options are unimportant. Keep bespoke grouping, corruption, and cancellation construction explicit.

        - **Share:** repeated full/safety/borrowed rejection assertions can use a local `assert_invalid_slab` helper, with explicit loader coverage when it differs.

        - `synthetic_grammar_dictionaries_aliases_and_limits` — invokes native dictionary, grammar-limit, and unsupported-parser tests; checks slab-format encoding.

          - **Remove assertion:** the literal `slab_format` check duplicates the C static assertion and exact header checks in `storage.rs`; incorrect format encoding should break those as well.

        - `compatible_parser_preserves_language_after_failed_selection` — checks failed language selection before and after a valid selection, then parses with the retained language.

        - `cancellation_during_packing_finalization` — cancels every reported finalization step while repacking and constructing side data.

          - **Keep:** ordinary parser cancellation samples do not guarantee these late phases are reached.

        - `callback_input_during_ambiguity_replay` — forces callback rereads under ambiguity, checks chunked output, and verifies recovery from callback panic during replay.

        - `lexer_fallback_and_concurrent_parser_preparation` — invokes native lexer checks and races four threads preparing/reusing the same grammar, including failures and scratch drops.

          - **Share:** replace the two identical parse-with-options blocks with a local closure; preserve the scratch drop between them.

        - `point_delta_limits_control_grouping` — exercises each point component at deltas 255/256 with points enabled/disabled and checks decoded endpoints.

        - `compressed_points_preserve_maximum_coordinates` — checks maximum row/column roundtrip and zero-width point selection.

          - **Merge candidate:** add a separate maximum-coordinate case to the preceding point-encoding test, sharing the fixture/finalization helper. Retain the scan assertion; ordinary 255/256 cases do not imply it.

        - `synthetic_supertype_emission_and_persistence` — checks 0–9 supertype bits, direct/dictionary encodings, resize, copied/borrowed/cache-restored loads, and corrupt headers.

          - **Reduce:** deduplicate `[1 << bits, (1 << bits).min(257)]`; both entries are identical for bits 0–8. Move generic header corruption to `slab_headers_reject_incompatible_formats`, retaining any structural corruption it does not cover.

        - `id_width_covers_all_grammars_and_reserved_errors` — checks width selection across grammars and reserved error IDs at the 254/255 boundary.

        - `compact_domains_preserve_aliases_with_independent_widths` — checks independent public/grammar compact domains, byte versus word columns, aliases, cache restoration, and invalid compact IDs.

        - `symbol_ids_use_byte_columns` — checks byte-column offsets, alias roundtrip, layout size, and corrupted symbols/grammar flags.

          - **Merge:** fold byte-layout and corruption assertions into the small-grammar case of `synthetic_symbol_ids_and_optional_columns`; both emit the same repeating alias pattern. Preserve exact offsets and the separate-grammar flag rejection.

        - `matching_ids_omit_grammar_before_flag_columns` — checks omitted grammar columns with matching IDs and combinations of extra/missing/error flags, before/after repack.

          - **Keep:** the alias matrix below requires a separate grammar column and cannot cover omission.

        - `synthetic_symbol_ids_and_optional_columns` — checks width boundaries, sparse flags, points, group growth, repack/load/resize, and invalid IDs across synthetic grammars.

        - `maximum_spans_roundtrip_and_reject_delta_underflow` — checks trees exceeding 16-bit slot spans and rejection of an underflowing span delta.

        - `navigation_across_every_waste_boundary` — enumerates group-waste combinations and checks preorder, sibling/parent cursor navigation, and invalid slots.

        - `column_growth_compaction_and_little_endian_encoding` — checks all column values/bytes through growth, compaction, and overflow failure.

          - **Keep:** complements `column_copies_initialize_gaps_and_unused_capacity`; one checks values and endian encoding, the other initializes padding and unused capacity.

        - `invalid_waste_and_absent_fields` — checks absent-field scans for every group occupancy and rejects excessive waste through three loaders.

        - `presence_ignores_waste_and_invalid_symbols` — checks symbol presence with poisoned waste lanes, error IDs, invalid symbols/groups, and index on/off.

        - `packing_rejects_wrong_grammar_and_recovers_after_overflow` — rejects an equivalent-but-distinct grammar and overflow, then checks scratch reuse and retained-tree lifetime.

    - `native.c` — synthetic grammars and five C test entry points, invoked through `support/internal.rs`; not a separate test executable.

      - `sq_test_dictionaries` — checks dictionary determinism/cache validation, 65-supertype masks, maximum dictionary size, aliases, cycles, extras, and overflow.

        - **Keep:** Rust emission tests consume these dictionaries but do not independently check their construction rules.

      - `sq_test_grammar_limits` — rejects oversized symbol, alias, and field domains.

      - `sq_test_unsupported_parsers` — rejects unsupported ABI/external-scanner grammar variants with diagnostics.

      - `sq_test_lexer_fallback` — checks speculative lexing fallback for contiguous and callback input.

      - `sq_test_chunked_lexer` — compares chunked/contiguous UTF-8 decoding, positions, EOF, oversized-input rejection, and ambiguity replay.

        - **Reduce:** the final ambiguity replay smoke check overlaps the stronger Rust `callback_input_during_ambiguity_replay`. Remove that block if direct C callback replay is not an independently maintained contract; keep decoding and overflow checks.

    - `bindings.rs`

      - **Reduce:** this broad suite duplicates newer parser/query/navigation suites. Move unique public-wrapper and ownership assertions to their corresponding suites before deleting broad helper calls.

      - `error_flags_match_each_native_node` — compares every node's `has_error` with Tree-sitter across malformed inputs, point settings, repack, and copied/borrowed loads.

      - `shared_navigation` — checks trait navigation, attributes, filtering, cursor root boundaries, child fields, and seek behavior on native and packed JSON.

      - `group_boundaries_and_optional_columns` — runs shared navigation on a wide JSON tree with points/presence on/off.

        - **Merge:** make the small and wide inputs explicit cases of `shared_navigation`, preserving the native run and the wide option matrix. Also replace `fixture()` here with `json_language()`; the two discarded trees provide no coverage.

      - `streaming_queries_and_cursor_reuse` — compares query/predicate results with native execution and checks cursor reuse after dropping prior queries/trees, disabled patterns, and range reset.

        - **Share/merge:** move the predicate matrix into the native query comparison suite and keep a short cursor-lifetime/reuse case. Preserve the wildcard-disable workaround and exact event comparisons for the cases where agreement is expected.

      - `owned_and_borrowed_storage` — checks zero-copy borrowing, independent copied-tree lifetime, compact capacity, loaded queries/navigation, and corrupt-header rejection.

        - **Reduce:** remove the corrupt-header assertion; `slab_headers_reject_incompatible_formats` flips that same bit and all other fixed header bits. Replace full `check_queries`/`check_shared_navigation` reruns with a focused loaded-tree smoke check once storage/navigation suites own that coverage.

      - `direct_parser_matches_mainline_packing` — checks direct parsing against packed Tree-sitter bytes/sidecars for empty, aliased, long, deep, and wide C inputs and packing options.

        - **Share:** use the common tree-byte assertion. Remove repeated `check_shared_navigation` after exact byte equivalence, retaining a representative direct-output load assertion; this cannot reasonably reveal a navigation bug absent from equivalent packed input.

      - `direct_parser_reuses_after_failure_and_owns_grammar` — checks error location, reuse, scratch release, grammar ownership, and trees surviving parser destruction.

        - **Merge:** combine with `parser.rs::direct_callback_failures_and_reuse`, preserving explicit error coordinates and drops. That test currently compares against contiguous results rather than independently checking those coordinates.

      - `direct_parser_rejects_unsupported_grammar` — checks ABI-14 rejection through the public parser and convenience API.

        - **Keep:** native rejection alone does not prove public wrappers avoid fallback.

      - `mainline_parse_keeps_error_recovery` — checks recovered compatible parsing versus direct rejection on invalid C.

        - **Remove after moving two wrapper assertions:** `parser.rs::callback_input_and_error_recovery` already checks the same invalid C behavior; add the `Tree::parse`/`Tree::parse_direct` calls there to retain convenience-API coverage.

      - `language_inspection_matches_native` — compares metadata, symbol/field lookup, visibility, supertypes, and invalid IDs with native languages.

      - `compact_ids_roundtrip_native_kinds_and_scans` — checks compact/public ID conversions and filtered scans against native JSON/C nodes, including errors.

      - `tree_views_and_text_access` — compares UTF-16 text, range/trait access, language references, and point fallback with native nodes.

    - `boundary.rs`

      - `invalid_kinds_are_rejected` — checks error-kind scans and group presence while rejecting several invalid public IDs, with presence indexing on/off.

        - **Merge:** move invalid scan cases into `scanning.rs::empty_missing_and_error_nodes`, and presence cases into `presence_ignores_waste_and_invalid_symbols`. Preserve `ERROR` name lookup in language inspection, then remove this setup.

      - `typed_fields_and_slot_lookup` — checks field lookup, slot roundtrip, invalid/absent IDs, and single/set field filtering.

        - **Merge:** add typed lookup assertions to `language_inspection_matches_native`/`shared_navigation`; field-filter expectations already belong to `field_sets`. Preserve `FieldId::new(0)` and slot identity checks.

      - `grammar_kind_lookup_ignores_aliases` — verifies displayed C aliases differ from grammar IDs and checks invalid/error grammar-name lookup.

        - **Merge:** extend `compact_ids_roundtrip_native_kinds_and_scans` with this typedef input and lookup assertions; keep both displayed/original identities explicit.

      - `compiler_metadata_and_mutation_match_tree_sitter` — compares query metadata and compile error offsets across a syntax matrix, then calls disable APIs.

        - **Merge/remove:** retain its diverse patterns in a shared metadata checker used by `metadata_diagnostics_and_independent_clones`. Delete the final disable calls: they assert no postcondition, while execution tests already verify mutations.

      - `language_cache_round_trips_and_outlives_tree_sitter_language` — checks C# cache byte roundtrip and clone survival after dropping wrappers.

        - **Merge candidate:** add this ownership sequence to the cache-restoration cases in `synthetic_supertype_emission_and_persistence`; preserve C# as a real dictionary grammar if removing the standalone test.

      - `shared_coordinates_narrow_like_tree_sitter` — on 64-bit targets, compares overflowing byte/point node and cursor lookup arguments with native narrowing; checks invalid UTF-8 field name.

        - **Keep:** scans intentionally use different wide-coordinate semantics.

    - `storage.rs`

      - `slab_headers_reject_incompatible_formats` — checks exact core/presence/point headers and rejects every flipped fixed-format bit.

      - `packing_context_matches_fresh_packing_and_loading` — compares reused/fresh packing across three grammars, shape/size boundaries, and all packing options; checks loading, repack, side data, and presence.

        - **Reduce:** after exact core and presence-cache byte equality, the per-group/per-symbol comparison adds little: the same accessors on identical representations should agree. Remove that loop; dedicated presence tests use independent expected membership.

        - **Share:** flatten the option matrix into an iterator only if it improves readability. Keep source cases named; shrinking matrix dimensions mainly saves runtime, not much code.

      - `side_data_changes_only_attached_coordinates` — drops/reattaches points and builds presence on another thread; checks coordinates, idempotent drops, and stable core bytes/address/slots.

      - `sidecar_mapping_copy_and_failed_replacement` — checks retained/copied sidecars, alignment and corruption rejection, failed replacement preserving state, and exact owner destruction.

        - **Keep:** ownership and failed-replacement behavior can regress while ordinary scans still work. Keep the local slab-owner fixture local unless another ownership test actually needs it.

      - `point_bounded_queries_follow_attachment` — reuses a cursor while dropping/reattaching points and checks intersecting/containing query results.

        - **Keep:** coordinate accessors can remain correct while query execution retains stale attachment assumptions.

      - `presence_creation_does_not_change_core_layout` — checks presence-on/off core-byte identity on a multigroup tree, with points on/off.

        - **Merge:** add cross-presence core equality to `packing_context_matches_fresh_packing_and_loading`, which already varies the same options. Its current equality only compares fresh versus reused packing at the same options, so do not delete this test without adding that comparison.

      - `repacking_in_place_preserves_nodes_and_side_data` — checks compact capacity, unchanged sidecar addresses, node values, loadability, and repeated in-place repack.

        - **Merge candidate:** add an in-place branch to an existing packing/storage fixture, but preserve sidecar address checks; out-of-place repack equivalence does not cover them.

    - `navigation.rs`

      - `navigation_and_indexed_ranges_survive_loading` — compares packed versus reloaded nodes, relationships, cursors, and byte/point seeks on empty/error/deep/wide C trees.

        - **Reduce:** this mostly compares the same implementation on equivalent storage. Once core bytes and attached point bytes agree, use one rich fixture for detailed loaded navigation and rely on native-oracle tests for the broader input matrix. Keep absent-point fallback and copied-storage ownership checks.

      - `indexed_points_follow_attachment_across_wide_trees` — compares point seeks with native byte seeks on 20,000 multiline Unicode elements while dropping/reattaching point data.

        - **Keep:** crosses indexed-search scale thresholds and attachment transitions absent from small side-data tests.

      - `child_iterators_preserve_cursor_state` — compares partial/full child iterators, fields, cursor state, clone/reset, invalid fields, and index boundaries with native C trees.

        - **Reduce:** remove three repeated `(0, None)` size-hint assertions if that conservative implementation detail is not an API promise; iteration/state behavior is the useful contract.

    - `parser.rs`

      - **Share:** `callback_tree` duplicates `check_point`'s byte-to-point calculation; call that existing helper.

      - `shared_traits_and_no_language` — checks missing-language errors and generic parse traits across compatible, native, and direct parsers.

      - `direct_traits_ignore_progress_callbacks` — checks that direct parsing ignores cancellation callbacks while preserving packing options for contiguous/chunked input.

      - `callback_input_and_error_recovery` — checks chunked Unicode input, error progress, compatible recovery versus direct rejection, language switching, reset, and tree lifetime.

      - `direct_callback_chunks_match_contiguous` — compares direct callback chunk sizes with contiguous/direct and compatible-packed output across UTF-8, long tokens, ambiguity, and options.

        - **Keep:** overlaps contiguous equivalence but specifically exercises chunk ownership and split code points.

      - `direct_callback_failures_and_reuse` — compares chunked/contiguous failures on malformed syntax/bytes and verifies reuse after callback panic and scratch release.

      - `packed_options_progress_and_equivalence` — checks parse/pack progress phases, offsets, direct equivalence, and omitted side data.

      - `cancellation_in_both_phases_and_reuse` — cancels at sampled parse/pack progress callbacks and verifies reuse, including the native trait adapter.

      - `packing_failure_and_reuse` — checks overflow is reported as `ParserError::Pack` and subsequent parsing succeeds.

        - **Merge candidate:** append this short failure case to `cancellation_in_both_phases_and_reuse`; retain error mapping rather than relying only on the internal packer overflow test.

      - `native_trait_discards_previously_interrupted_parse` — interrupts through native APIs, then checks the shared trait starts a different input cleanly.

        - **Keep:** cancellation through the trait itself does not cover externally interrupted parser state.

      - `options_reborrow_preserves_callback_and_pack_settings` — reuses one options value twice, retaining callback access and disabled side data.

        - **Merge:** make the no-side-data branch of `packed_options_progress_and_equivalence` parse two sources through `options.reborrow()`, preserving callback-count and option assertions, then delete this standalone setup.

    - `query_execution.rs`

      - **Share:** reuse `support` constructors and snapshots; generalize `json_query_tree` only enough to accept another language. Local byte/point range setup helpers can replace repeated cursor-configuration ladders.

      - `queries_match_with_and_without_plans` — compares optimized/general matches and capture sets on a C query matrix, including disabling and state-dedup pressure.

        - **Reduce:** both executions can share one tree and grammar; separate packing is unrelated to plan equivalence. Keep independent query clones only where mutation independence matters.

      - `error_queries_survive_native_mutations` — compares ERROR/wildcard queries with native expectations through capture/pattern disabling on JSON/C# errors.

      - `malformed_queries_match_with_and_without_plans` — generates truncations/insertions/deletions and structural queries for three languages; compares plans, ranges, and completed capture coverage.

        - **Keep:** generated malformed trees cover interactions missing from the hand-selected native matrix. Share collection machinery, not the oracle or cases.

      - `presence_scans_across_groups` — compares planned/general nested JSON queries around group boundaries, with error and absent-symbol cases.

      - `disabling_non_rooted_pattern_preserves_ranges` — verifies exact expected captures before/after repeated disabling of a C sibling-root pattern under byte bounds.

      - `cancellation_limits_ranges_and_reuse` — checks match-limit exhaustion, callback stop, range reuse, and explicit match removal.

        - **Reduce:** keep the match-limit/ambiguous-state scenario; removal is covered more thoroughly by `removal_keeps_current_captures_readable_and_nodes_independent`, range persistence by `cursor_and_iterator_ranges_narrow_validate_and_persist`, and resumable cancellation by `progress_cancellation_resumes_every_entry_point`. Retain a cancellation-plus-low-limit case only if that interaction is intentional.

      - `switching_between_matches_and_captures_preserves_finished_order` — interleaves result APIs across multiple finished patterns and compares optimized/general ordering.

        - **Reduce:** share the tree/query between cursors. The `optimized=false` pass compares identical paths and can go unless it is deliberately testing independent cursor state.

      - `query_edge_cases_match_tree_sitter` — compares compile outcomes, completed matches, and appropriate capture coverage over JSON/C syntax, errors, fields, supertypes, depth, and 15 range modes.

        - **Share:** extract range configuration and snapshot collection; preserve native comparison and range-specific capture semantics. This is already a useful data-driven consolidation.

      - `containing_ranges_combine_with_intersecting_ranges` — combines four bound types, checks rooted/rootless results and uncaptured root containment, then resets restrictions.

      - `containing_ranges_include_missing_nodes_at_the_end` — checks missing C semicolons at containing-range endpoints against native behavior in byte/point modes.

      - `containing_ranges_finish_deferred_matches_in_error_subtrees` — asserts the intentional Squatter result where native hidden-node traversal loses a deferred match.

        - **Keep:** cannot be absorbed into an equality-to-Tree-sitter oracle.

      - `quantified_roots_with_ranges_match_tree_sitter` — checks optional/repeated wildcard and number roots across byte/point bounds, excluding backend-dependent empty matches.

      - `disabled_rootless_and_branching_patterns_with_ranges` — checks repeated disable operations, disabled captures, and surviving results for wildcard/sibling/alternative roots in both stream modes.

        - **Share with** `disabling_non_rooted_pattern_preserves_ranges`, but retain the latter's exact expected results; this test currently only checks exclusions and nonemptiness.

      - `chunked_predicates_and_streaming_entry_points` — compares borrowed/owned/empty text chunks with contiguous input for predicate variants and seven query entry points.

        - **Reduce matrix:** test the full predicate/chunk matrix through core match/capture execution, then a small representative set through forwarding APIs. Keep owned chunks, split UTF-8, empty chunks, and both optimization modes.

      - `metadata_diagnostics_and_independent_clones` — checks metadata/properties/custom predicates, compiler errors, and clones independent of mutation/destruction.

      - `cloned_queries_preserve_compact_error_symbols` — checks cloned wildcard/ERROR/child queries on malformed JSON in both stream and optimization modes.

        - **Keep:** a clone of a valid query/tree need not expose erroneous compact ERROR remapping.

      - `predicate_diagnostics_match_tree_sitter` — compares full predicate errors across operators, bad argument shapes, prefixes, and Unicode offsets.

        - **Share:** use the same diagnostic assertion helper as `metadata_diagnostics_and_independent_clones`; keep the distinct input matrix.

      - `removal_keeps_current_captures_readable_and_nodes_independent` — checks repeated removal, borrowed captures, moved streams, retained nodes, and fresh cursor reuse.

      - `progress_cancellation_resumes_every_entry_point` — repeatedly pauses/resumes all stream types, with/without side data, including no-hit and predicate-rejected queries; checks output and progress positions.

      - `optimized_capture_progress_tracks_later_subtrees` — verifies optimized progress stays in later subtrees after earlier captures, with repeated cancellation.

        - **Keep:** the broader progress test only requires some positive offsets and would miss a later reset to zero.

      - `cursor_and_iterator_ranges_narrow_validate_and_persist` — checks reversed/overflowing ranges, depth/limit accessors, mid-stream narrowing, persisted restrictions, and reset.

      - `query_language_mismatch_is_an_execution_error` — rejects a query/tree language mismatch without yielding matches or captures.

      - `execution_owns_provider_and_releases_it_on_drop` — checks provider destruction, copied node lifetime, and cursor reuse.

        - **Optional removal:** ordinary Rust ownership already provides much of this guarantee; chunked owned-provider tests and compile-fail borrow tests cover adjacent contracts. Retain if explicit drop timing is an intended API promise; otherwise move the retained-node assertion into the removal test and delete the custom `Provider`/drop-counter fixture.

    - `scanning.rs`

      - **Share:** keep `check_pipeline`, `check_selection`, and `check_consumption`; they expose distinct count/fold/reversal/group paths without duplicating every assertion in each test.

      - **Reduce:** `check_selection` explicitly repeats reverse-node assertions already checked by `check_pipeline`. Keep reversal-before-filter coverage in one composition test, rather than every coordinate case.

      - `orders_subtrees_groups_and_directions` — compares preorder/postorder with native traversal and checks all subtree/group/direction/consumption paths.

      - `ranges_filters_waste_and_storage_variants` — checks byte overlap, kinds, fields, flags, and borrowed/repacked storage with coordinate-induced waste and optional side data.

      - `empty_missing_and_error_nodes` — checks range behavior and ERROR/invalid-kind filtering on empty and malformed JSON.

      - `range_seeks_across_subtrees` — checks byte/point selection on wide/deep trees with long gaps and sampled subtrees.

        - **Share:** factor its repeated relation checks with `check_position_selections`, passing explicit roots/ranges. Do not run the exhaustive small-fixture matrix over every large subtree.

      - `range_and_position_relations` — checks overlap/within/contain/start/end predicates at byte/point boundaries, including reversed/empty/extreme ranges and borrowed trees.

      - `zero_width_overlap_boundaries` — asserts readable, explicit missing-node boundary behavior, including empty containment versus point containment.

        - **Remove/merge candidate:** `[1` is already in `range_and_position_relations`, whose helper samples zero-width nodes and all these relations. Move the few explicit “empty interval versus point” assertions there if useful as specification, then remove this standalone fixture.

      - `id_set_intersection` — checks empty/disjoint/duplicate/high-ID set intersection and commutativity over the full ID domain.

      - `dense_id_filters` — checks dynamic kind sets and individual fields, including invalid IDs and ERROR combinations.

      - `sparse_kind_filters` — checks a selective kind filter followed by a broad set, and within-range filtering, with presence on/off.

      - `sparse_cursor_pipelines` — checks indexed sparse postings across many groups/subtrees, range restriction, flags, and both kind-filter orders.

        - **Merge candidate:** incorporate `sparse_kind_filters`' broad target set into this fixture, preserving its within-range case; then remove the smaller fixture. Confirm it still exercises sparse candidate masks as well as postings.

      - `prepared_kind_sets` — checks dynamic target-cardinality boundaries, invalid/error IDs, range/field/flag composition, and filter-order equivalence.

      - `indexed_kind_filters` — checks rare/common/absent kinds across multiword presence indexes, loaded storage, subtrees, and byte/point ranges.

        - **Keep:** a small sparse-candidate fixture does not guarantee multiword index skipping.

      - `fixed_kind_sets` — checks fixed arrays at several cardinalities, duplicate/invalid/error IDs, subtree/trait APIs, and equivalence with dynamic sets.

        - **Share:** reuse the identical malformed JSON fixture from `dense_id_filters`. Retain fixed and dynamic entry points; they specialize differently. Reuse the first native/tree parse at the end instead of parsing again.

      - `field_sets` — checks fixed/dynamic field unions, absent fields, invalid IDs, duplicates, subtrees, postorder, and range/kind composition.

      - `supertype_membership` — compares direct-mask JSON and dictionary-backed C# supertype filters against node membership in both traversal orders.

      - `composition_and_reverse_preserve_membership` — checks intersected kind filters and extras through postorder reversal on C#.

        - **Merge:** add this comment-bearing C# case and composition assertions to `supertype_membership`; remove the full `check_ranges` rerun once the range suite includes one extra-bearing fixture.

      - `scans_and_groups_are_send_sync` — compile-checks thread traits for scan/group types and moves a partially consumed scan to another thread.

        - **Reduce:** retain the type assertions but remove the runtime thread/count portion if thread transfer itself is not a regression target. Partial-consumption correctness is already checked, and auto-trait failures are compile errors. These assertions can share the basic traversal fixture.

      - `deep_and_wide_postorder` — compares 512-deep and 2,048-wide postorder/reverse traversal with native output and tests partial consumption.

        - **Share:** reuse a small native-order assertion helper with `orders_subtrees_groups_and_directions`; keep the larger fixtures separate from its all-subtree matrix.

    - `scan_patterns.rs` — executable usage examples and stable uninlined assembly-inspection entry points; not a timed benchmark.

      - `scan_patterns` — checks explicit text results for orders, kinds, fields, ranges, subtrees, partial consumption, and groups.

        - **Optional removal:** broad correctness is already covered by `scanning.rs`. Keep a short readable usage example; remove long expected traversal lists if executable documentation is not needed.

      - `assembly_patterns_match_examples` — invokes every assembly wrapper against scalar counts/sums/selections.

        - **Reduce:** keep enough calls/assertions to retain and identify the code-generation probes; extensive repeated semantic checks belong in `scanning.rs`. Do not delete wrappers merely because equivalent timed workloads exist: named uninlined functions serve inspection.

      - `patterns/` module — assembly probes, each retained by the test above:

        - `all_count` — aggregate unordered scan count.

        - `preorder_count` — aggregate preorder count.

        - `postorder_count` — aggregate postorder count.

        - `reverse_postorder_count` — aggregate reverse-postorder count.

        - `nodes_count` — count through the node iterator.

        - `kind_count` — dynamic kind-set count.

        - `four_kind_count` — fixed four-kind count.

        - `eight_kind_count` — fixed eight-kind count.

        - `scalar_kind_count` — node-by-node kind filtering/counting.

        - `field_count` — single-field count.

        - `two_field_count` — fixed two-field count.

        - `range_count` — byte-overlap count.

        - `range_slots` — sum slots selected by byte overlap.

        - `point_range_slots` — sum slots selected by point overlap.

        - `combined_count` — range/kind/field/extra/missing composition count.

        - `supertype_count` — supertype-filtered count.

        - `preorder_slots` — sum preorder slots.

        - `reverse_preorder_slots` — sum reverse-preorder slots.

        - `postorder_slots` — sum postorder slots.

        - `reverse_postorder_slots` — sum reverse-postorder slots.

        - `grouped_slots` — sum slots through group iteration.

        - `kind_start_bytes` — sum selected nodes' start bytes.

        - `first_kind_slot` — first selected preorder slot.

  - `examples/slab-compatibility.rs`

    - `main` compatibility probe — exchanges 16 packing-option variants, compares exact core/sidecar bytes, loads through copied/borrowed/safety paths, checks nodes/navigation, and compares compact copying with repack.

      - **Keep:** ordinary little-endian tests cannot replace execution on a big-endian or different-pointer-width target.

      - **Reduce:** `compare` repeats several API assertions already covered elsewhere, but keeping a representative semantic decode check is necessary; identical bytes alone do not prove correct foreign-endian reads.

  - `native/{grammar.c, internal.h, parser.c, query.c, query.h, reductions.h, supertypes.c}` — native implementation and headers consumed by tests; no standalone test entry points.

  - `QUERY_PROVENANCE.md` — query implementation provenance, not an executable check.

- `crates/squatter-bench/`

  - `Cargo.toml` — four standalone binaries and shared benchmark library; no Criterion harness.

  - `src/`

    - `lib.rs` — corpus selection, parse contexts, validation, measurement dispatch, and reporting shared by `squatter-bench` and `squatter-check`.

      - **Keep shared:** check and benchmark binaries are already three-line entry points over one runner. Merging the executables would save almost no source.

      - **Share:** `observe` maps both traversal workloads to the same correctness snapshot; validate it once per tree pair when both workloads are selected. Keep each timed traversal separate.

      - `batches_support_file_counts_and_source_working_sets` — checks file-count batches versus byte-target carousel batches, including overshooting a byte target.

      - `summaries_keep_all_cases_and_a_successful_direct_subset` — checks status counts, all-file statistics, successful-direct-only statistics, paired ratios, missing counters, and empty eligible subsets.

        - **Keep:** report eligibility mistakes can produce plausible but misleading benchmark results without breaking tree correctness.

      - `direct_parse_measurements_validate_and_classify_results` — checks cold/warm backend rotations and classification of successful, rejected, mismatched, malformed, and unsupported direct parses.

        - **Reduce:** run rejected/mismatched classification assertions once outside the rotation loop; they do not depend on rotation. Keep successful measurements in every rotation.

      - Named workloads, dispatched by both binaries; timings below apply to `squatter-bench`:

        - `query-matches` — consumes completed matches from registry queries for native and Squatter engines; snapshots/validation stay outside timing.

          - **Keep:** share setup with captures, but keep separate timing and exact completed-match validation.

        - `query-captures` — consumes capture events; validation requires completed-match capture coverage while allowing provisional differences.

          - **Keep:** not interchangeable with matching; event multiplicity, ordering, and engine work differ.

        - `cursor-forward` — walks native/packed cursors and consumes each node without loading full attributes.

        - `scan-forward` — walks the same cursors while consuming full constant-time attributes; despite its name, this is not the group-scan iterator benchmark.

          - **Keep both:** their difference measures attribute cost. Share traversal implementation through the existing const parameter.

        - `seek-byte` — performs 100 seeded zero-width byte descendant lookups per source.

        - `seek-point` — performs corresponding row/column descendant lookups.

          - **Share:** coordinate selection is already shared. Keep the measurements separate because their indexes/accessors differ.

        - `cold-parse` — compares fresh native parsing, native-plus-packing, and direct parsing, including applicable parser/grammar construction and parser destruction.

        - `warm-parse` — compares the same backends with warmed reusable parsers, language data, and packing scratch.

          - **Keep both:** constructor cost and scratch reuse are separate questions. Returned tree destruction is outside these timers, unlike most lifecycle workloads.

      - `setup-parse` — untimed prerequisite parsing when neither parse workload is selected; a report label, not a ninth benchmark.

    - `compare.rs` — shared native/packed traversal, identity, attributes, relationships, and seek oracles.

      - `field_policy_requires_independent_visible_child_agreement` — checks that only disagreement with native transitive lookup, plus agreement with visible children, qualifies as an expected field difference.

        - **Keep despite simple implementation:** this is the checker’s exemption rule. Loosening it could hide regressions without another test failing.

    - `queries.rs` — loads paired registry queries, executes streams, checks capture coverage, and records compilation diagnostics/timing.

      - `capture_coverage_allows_provisional_states_but_requires_completed_captures` — accepts reordered/duplicate/provisional events, rejects missing completed captures and invalid capture indexes, and excludes zero-end captures.

        - **Keep:** validates the oracle itself; parser/query integration tests assume it is correct.

      - Query `compile_ms` — load-time diagnostic measurement, not an independently repeated workload. Use lifecycle `query-new` for construction performance comparisons.

    - `measure.rs` — wall/thread CPU/hardware-counter measurement and quantiles.

      - `quantiles_interpolate_and_keep_missing_counters_missing` — checks interpolation, minimum/empty input, median aggregation, and absent hardware counters.

        - **Reduce:** `summaries_keep_all_cases_and_a_successful_direct_subset` already checks interpolation and absent counters at report level. Keep the empty/minimum cases here; optionally remove the duplicate `Metrics::median` fixture, or move all cases into the reporting test if exposing a small helper is natural.

    - `pressure.rs` — cache-pressure setup and application.

      - `randomized_ring_visits_every_line` — checks that randomized cache-line links form one complete cycle.

        - **Keep:** a short cycle could silently weaken pressure without changing test outputs or producing a benchmark error.

      - `none` — no induced pressure; baseline condition.

      - `carousel` — cycles source batches sized by working-set bytes.

      - `wash` — traverses a randomized buffer before measurement.

      - `tenant` — runs a concurrent duty-cycled pressure worker.

        - **Keep as conditions:** these are not duplicate workload implementations. Share profile setup and report validation; do not multiply handwritten timing loops by condition.

    - `bin/`

      - `squatter-bench.rs` — calls the shared runner with timing enabled; eight workloads listed under `lib.rs`.

      - `squatter-check.rs` — calls the same runner without timing; validates corpus originals/mutations and enabled parse/query/traversal/seek cases.

      - `core-lifecycle-bench.rs` — 16 operation-specific workloads; setup loads one source/native tree, language, packed tree, packer, and compact destination. Most timed operations include returned-owner destruction.

        - **Share:** query drop/disable operations already share bounded batches and exclude compilation. Keep this separate from ordinary operation timing. Query selection can be deferred when only non-query workloads are requested, simplifying their dependency on query fixtures.

        - **Share across binaries:** small registry-loading, digest-checking, and report metadata helpers may help; do not unify CLI input formats or residency policies merely to remove a few lines.

        - `pack-cold` — packs an existing native tree with a fresh pack context and drops the result.

          - **Keep:** `cold-parse` includes parsing/grammar setup, so it cannot isolate this cost.

        - `pack-reuse` — packs through retained scratch and drops the result.

        - `pack-drop-scratch` — drops scratch before each reused-context pack, then drops the result.

          - **Optional removal:** if scratch disposal is no longer being studied, keep `pack-cold`/`pack-reuse` and remove this diagnostic. It is not an exact duplicate: it retains the context and times scratch disposal.

        - `point-access` — traverses all nodes and reads both point endpoints, optionally with synthetic points when point data is disabled.

          - **Keep:** `seek-point` measures navigation and `scan-forward` loads more than endpoints.

        - `load-full` — copies and fully validates a core slab, then destroys the loaded tree.

        - `load-safety` — copies a slab with safety-only validation, then destroys it.

        - `load-borrowed` — validates borrowed bytes without copying, then drops the descriptor.

        - `load-retained` — loads through a retained stable owner, including Arc clone and owner allocation/destruction.

          - **Keep the four load modes:** copying, validation strength, and ownership have distinct costs. Share registration, not their measured bodies.

        - `compact-copy` — writes compact bytes into a reusable uninitialized destination.

        - `repack` — allocates a compact tree and destroys it.

          - **Keep both:** allocation-free copying and owning repack answer different questions even when resulting bytes agree.

        - `language-new` — prepares and destroys language data from native grammar metadata.

        - `language-cache` — restores and destroys language data from cached bytes.

          - **Keep both:** compare construction with restoration using the same fixture; do not infer one from parse timing.

        - `query-new` — compiles and destroys a query against prepared language data.

        - `query-drop` — destroys precompiled queries, excluding their construction.

          - **Keep:** construction and destruction can move independently. `query-new` alone cannot isolate the latter.

        - `query-disable-pattern` — disables pattern zero on fresh precompiled queries, excluding compile/drop costs.

        - `query-disable-capture` — disables the first capture by name on fresh precompiled queries, excluding compile/drop costs.

          - **Keep separate:** pattern disabling and capture disabling update different state; shared batch setup is sufficient consolidation.

      - `scanning-bench.rs` — 267 named workloads, expanded below. Parsing, packing, fixture statistics, and correctness validation are outside timing; scan construction is inside.

        - **Share:** replace manual name arrays and duplicated fixed/dynamic/node/count registration with small local macros taking explicit pipelines, cardinalities, and scalar predicates. `combined_kind_workloads` already demonstrates this. Keep generated functions statically specialized and dispatch outside the hot operation.

        - **Share:** associate scalar expected counts with registration rather than reconstructing the workload taxonomy in `main` through string-prefix matching. This removes a second list of workload semantics.

        - **Share:** extend the existing range registration macro to overlap, point overlap, and optional scalar/reverse consumers; their handwritten bodies repeat the same structure.

        - **Reduce validation duplication:** retain scalar semantic checks on corpus fixtures, but combine membership validation with workload registration. `validate`, registration macros, and `main` currently express parts of the same expected membership. Keep checks outside timing and count checks after timed samples.

        - **Keep distinct consumers:** `.nodes` uses `next` with per-node `black_box`; `.fold` uses iterator folding with per-node `black_box`; `.count` consumes only an aggregate. These may have specialized implementations despite equal counts. Removing all fold or reverse variants on semantic grounds would lose performance coverage.

        - Kind selections are frequent/rare/sparse/absent; sizes are 1/2/4/8/16 with repeated IDs when the grammar supplies fewer. Dynamic sets deduplicate. Presence indexing can be disabled. These conditions apply to the same workloads, not separate handwritten benchmarks.

        - Base traversal and filter workloads:

          - `preorder.fold` — folds over preorder nodes.

          - `preorder.rev.fold` — folds over reversed preorder nodes.

            - **Share:** call `consume_fold`, as the other fold workloads already do; this body repeats it inline.

          - `preorder.groups.fold` — folds nodes within each preorder group and sums group counts.

          - `postorder.fold` — folds over postorder nodes.

          - `postorder.rev.fold` — folds over reversed postorder nodes.

          - `kind.fold` — folds nodes matching the single selected kind.

            - **Remove:** identical selected IDs, dynamic set representation, pipeline, and consumer to `dynamic_1.fold`. Keep the sized name, or retain this name only as a CLI alias.

          - `multi_kind.fold` — folds nodes matching the configurable selected kind set.

            - **Conditional removal:** duplicates the corresponding `dynamic_4` workload at the default selection size. Retain if arbitrary `--kind-count` values matter; otherwise use the sized family and remove the configurable duplicate.

          - `supertype.nodes` — visits nodes matching the first grammar supertype (invalid ID if absent).

          - `supertype.count` — counts nodes matching that supertype.

          - `flags.count` — counts nodes excluding extras and missing nodes.

          - `combined.count` — counts the selected kind and field, excluding extras and missing nodes.

          - `multi_kind.nodes` — visits nodes matching the configurable selected kind set.

            - **Conditional removal:** duplicates the corresponding `dynamic_4` workload at the default selection size. Retain if arbitrary `--kind-count` values matter; otherwise use the sized family and remove the configurable duplicate.

          - `multi_kind.count` — counts nodes matching the configurable selected kind set.

            - **Conditional removal:** duplicates the corresponding `dynamic_4` workload at the default selection size. Retain if arbitrary `--kind-count` values matter; otherwise use the sized family and remove the configurable duplicate.

          - `field.nodes` — visits nodes matching the most frequent nonzero field, or no field if none exist.

          - `field.count` — counts nodes matching the selected field.

          - `field.scalar` — filters scalar preorder navigation by the selected field.

          - `postorder.field.nodes` — visits field-filtered nodes in postorder.

          - `postorder.field.count` — counts a field-filtered postorder scan.

          - `postorder.kind.nodes` — visits single-kind nodes in postorder.

          - `postorder.kind.count` — counts a single-kind postorder scan.

          - `postorder.rev.kind.nodes` — visits single-kind nodes in reverse postorder.

          - `preorder.nodes` — visits preorder nodes.

          - `preorder.rev.nodes` — visits reverse-preorder nodes.

          - `postorder.nodes` — visits postorder nodes.

          - `postorder.rev.nodes` — visits reverse-postorder nodes.

          - `all.nodes` — visits all nodes through the order-unspecified scan.

            - **Candidate removal:** currently shares the preorder scan representation with `preorder.nodes`. Keep one timing if those APIs are intentionally implementation-identical; retain both only to monitor future divergence in the order-unspecified API.

          - `all.rev.nodes` — visits the reversed all-node scan.

            - **Candidate removal:** same current traversal as `preorder.rev.nodes`; apply the same API-divergence decision as `all.nodes`.

          - `scalar_next_preorder` — visits nodes using repeated scalar next_preorder navigation.

          - `mainline_cursor` — visits native Tree-sitter preorder nodes through the shared trait.

          - `preorder.count` — counts directly from a preorder scan.

            - **Consolidate:** these unfiltered count paths delegate to subtree counting. Keep one representative timing plus correctness assertions for the wrappers if separate wrapper code generation is not being studied.

          - `preorder.nodes.count` — counts through the preorder node iterator.

            - **Consolidate:** these unfiltered count paths delegate to subtree counting. Keep one representative timing plus correctness assertions for the wrappers if separate wrapper code generation is not being studied.

          - `postorder.count` — counts directly from a postorder scan.

            - **Consolidate:** these unfiltered count paths delegate to subtree counting. Keep one representative timing plus correctness assertions for the wrappers if separate wrapper code generation is not being studied.

          - `all.count` — counts directly from the all-node scan.

            - **Consolidate:** these unfiltered count paths delegate to subtree counting. Keep one representative timing plus correctness assertions for the wrappers if separate wrapper code generation is not being studied.

          - `kind.nodes` — visits nodes matching the single selected kind.

            - **Remove:** identical selected IDs, dynamic set representation, pipeline, and consumer to `dynamic_1.nodes`. Keep the sized name, or retain this name only as a CLI alias.

          - `kind.count` — counts nodes matching the single selected kind.

            - **Remove:** identical selected IDs, dynamic set representation, pipeline, and consumer to `dynamic_1.count`. Keep the sized name, or retain this name only as a CLI alias.

          - `kind.scalar` — filters scalar preorder navigation by the selected kind.

          - `range.nodes` — visits overlapping nodes for the selected byte interval.

          - `range.count` — counts overlapping nodes for the selected byte interval.

          - `range.fold` — folds over overlapping nodes for the selected byte interval.

          - `range.reverse_nodes` — visits overlapping nodes in reverse preorder for the selected byte interval.

          - `range.scalar` — filters scalar preorder navigation by overlap for the selected byte interval.

          - `point_range.nodes` — visits overlapping nodes for the selected point interval.

          - `point_range.count` — counts overlapping nodes for the selected point interval.

          - `point_range.fold` — folds over overlapping nodes for the selected point interval.

          - `point_range.reverse_nodes` — visits overlapping nodes in reverse preorder for the selected point interval.

          - `point_range.scalar` — filters scalar preorder navigation by overlap for the selected point interval.

        - Coordinate-only families:

          - `within.nodes` — visits nodes contained in the byte interval.

          - `within.count` — counts nodes contained in the byte interval.

          - `within.fold` — folds over nodes contained in the byte interval.

          - `starting_in.nodes` — visits nodes starting in the byte interval.

          - `starting_in.count` — counts nodes starting in the byte interval.

          - `starting_in.fold` — folds over nodes starting in the byte interval.

          - `starting_at.nodes` — visits nodes starting at the byte interval start.

          - `starting_at.count` — counts nodes starting at the byte interval start.

          - `starting_at.fold` — folds over nodes starting at the byte interval start.

          - `point_within.nodes` — visits nodes contained in the point interval.

          - `point_within.count` — counts nodes contained in the point interval.

          - `point_within.fold` — folds over nodes contained in the point interval.

          - `point_starting_in.nodes` — visits nodes starting in the point interval.

          - `point_starting_in.count` — counts nodes starting in the point interval.

          - `point_starting_in.fold` — folds over nodes starting in the point interval.

          - `point_starting_at.nodes` — visits nodes starting at the point interval start.

          - `point_starting_at.count` — counts nodes starting at the point interval start.

          - `point_starting_at.fold` — folds over nodes starting at the point interval start.

        - Coordinate selection followed by kind filtering:

          - **Share:** generate fixed/dynamic consumers from one declaration per range method and cardinality; preserve byte/point and overlap/containment distinctions.

          - `range.fixed_1.nodes` — visits byte overlap matches filtered by 1 kinds using an array.

          - `range.fixed_1.count` — counts byte overlap matches filtered by 1 kinds using an array.

          - `range.dynamic_1.nodes` — visits byte overlap matches filtered by 1 kinds using a reusable set.

          - `range.dynamic_1.count` — counts byte overlap matches filtered by 1 kinds using a reusable set.

          - `range.fixed_4.nodes` — visits byte overlap matches filtered by 4 kinds using an array.

          - `range.fixed_4.count` — counts byte overlap matches filtered by 4 kinds using an array.

          - `range.dynamic_4.nodes` — visits byte overlap matches filtered by 4 kinds using a reusable set.

          - `range.dynamic_4.count` — counts byte overlap matches filtered by 4 kinds using a reusable set.

          - `range.fixed_8.nodes` — visits byte overlap matches filtered by 8 kinds using an array.

          - `range.fixed_8.count` — counts byte overlap matches filtered by 8 kinds using an array.

          - `range.dynamic_8.nodes` — visits byte overlap matches filtered by 8 kinds using a reusable set.

          - `range.dynamic_8.count` — counts byte overlap matches filtered by 8 kinds using a reusable set.

          - `point_range.fixed_1.nodes` — visits point overlap matches filtered by 1 kinds using an array.

          - `point_range.fixed_1.count` — counts point overlap matches filtered by 1 kinds using an array.

          - `point_range.dynamic_1.nodes` — visits point overlap matches filtered by 1 kinds using a reusable set.

          - `point_range.dynamic_1.count` — counts point overlap matches filtered by 1 kinds using a reusable set.

          - `point_range.fixed_4.nodes` — visits point overlap matches filtered by 4 kinds using an array.

          - `point_range.fixed_4.count` — counts point overlap matches filtered by 4 kinds using an array.

          - `point_range.dynamic_4.nodes` — visits point overlap matches filtered by 4 kinds using a reusable set.

          - `point_range.dynamic_4.count` — counts point overlap matches filtered by 4 kinds using a reusable set.

          - `within.fixed_1.nodes` — visits byte containment matches filtered by 1 kinds using an array.

          - `within.fixed_1.count` — counts byte containment matches filtered by 1 kinds using an array.

          - `within.dynamic_1.nodes` — visits byte containment matches filtered by 1 kinds using a reusable set.

          - `within.dynamic_1.count` — counts byte containment matches filtered by 1 kinds using a reusable set.

          - `within.fixed_4.nodes` — visits byte containment matches filtered by 4 kinds using an array.

          - `within.fixed_4.count` — counts byte containment matches filtered by 4 kinds using an array.

          - `within.dynamic_4.nodes` — visits byte containment matches filtered by 4 kinds using a reusable set.

          - `within.dynamic_4.count` — counts byte containment matches filtered by 4 kinds using a reusable set.

          - `point_within.fixed_1.nodes` — visits point containment matches filtered by 1 kinds using an array.

          - `point_within.fixed_1.count` — counts point containment matches filtered by 1 kinds using an array.

          - `point_within.dynamic_1.nodes` — visits point containment matches filtered by 1 kinds using a reusable set.

          - `point_within.dynamic_1.count` — counts point containment matches filtered by 1 kinds using a reusable set.

          - `point_within.fixed_4.nodes` — visits point containment matches filtered by 4 kinds using an array.

          - `point_within.fixed_4.count` — counts point containment matches filtered by 4 kinds using an array.

          - `point_within.dynamic_4.nodes` — visits point containment matches filtered by 4 kinds using a reusable set.

          - `point_within.dynamic_4.count` — counts point containment matches filtered by 4 kinds using a reusable set.

        - Composed kind-filter families:

          - **Keep order pairs:** equal membership does not imply equal preparation, candidate density, or index cost. `field`/`kind_field`, `range_field`/`range_kind_field`, and `intersection`/`intersection_reverse` merit separate timing. Reduce their registration code instead.

          - `field.fixed_2.nodes` — visits matches of field then kinds, with 2 array IDs.

          - `field.fixed_2.count` — counts matches of field then kinds, with 2 array IDs.

          - `field.dynamic_2.nodes` — visits matches of field then kinds, with 2 reusable-set IDs.

          - `field.dynamic_2.count` — counts matches of field then kinds, with 2 reusable-set IDs.

          - `field.fixed_4.nodes` — visits matches of field then kinds, with 4 array IDs.

          - `field.fixed_4.count` — counts matches of field then kinds, with 4 array IDs.

          - `field.dynamic_4.nodes` — visits matches of field then kinds, with 4 reusable-set IDs.

          - `field.dynamic_4.count` — counts matches of field then kinds, with 4 reusable-set IDs.

          - `field.fixed_8.nodes` — visits matches of field then kinds, with 8 array IDs.

          - `field.fixed_8.count` — counts matches of field then kinds, with 8 array IDs.

          - `field.dynamic_8.nodes` — visits matches of field then kinds, with 8 reusable-set IDs.

          - `field.dynamic_8.count` — counts matches of field then kinds, with 8 reusable-set IDs.

          - `field.fixed_16.nodes` — visits matches of field then kinds, with 16 array IDs.

          - `field.fixed_16.count` — counts matches of field then kinds, with 16 array IDs.

          - `field.dynamic_16.nodes` — visits matches of field then kinds, with 16 reusable-set IDs.

          - `field.dynamic_16.count` — counts matches of field then kinds, with 16 reusable-set IDs.

          - `kind_field.fixed_2.nodes` — visits matches of kinds then field, with 2 array IDs.

          - `kind_field.fixed_2.count` — counts matches of kinds then field, with 2 array IDs.

          - `kind_field.dynamic_2.nodes` — visits matches of kinds then field, with 2 reusable-set IDs.

          - `kind_field.dynamic_2.count` — counts matches of kinds then field, with 2 reusable-set IDs.

          - `kind_field.fixed_4.nodes` — visits matches of kinds then field, with 4 array IDs.

          - `kind_field.fixed_4.count` — counts matches of kinds then field, with 4 array IDs.

          - `kind_field.dynamic_4.nodes` — visits matches of kinds then field, with 4 reusable-set IDs.

          - `kind_field.dynamic_4.count` — counts matches of kinds then field, with 4 reusable-set IDs.

          - `kind_field.fixed_8.nodes` — visits matches of kinds then field, with 8 array IDs.

          - `kind_field.fixed_8.count` — counts matches of kinds then field, with 8 array IDs.

          - `kind_field.dynamic_8.nodes` — visits matches of kinds then field, with 8 reusable-set IDs.

          - `kind_field.dynamic_8.count` — counts matches of kinds then field, with 8 reusable-set IDs.

          - `kind_field.fixed_16.nodes` — visits matches of kinds then field, with 16 array IDs.

          - `kind_field.fixed_16.count` — counts matches of kinds then field, with 16 array IDs.

          - `kind_field.dynamic_16.nodes` — visits matches of kinds then field, with 16 reusable-set IDs.

          - `kind_field.dynamic_16.count` — counts matches of kinds then field, with 16 reusable-set IDs.

          - `flags.fixed_2.nodes` — visits matches of exclude extras/missing nodes then kinds, with 2 array IDs.

          - `flags.fixed_2.count` — counts matches of exclude extras/missing nodes then kinds, with 2 array IDs.

          - `flags.dynamic_2.nodes` — visits matches of exclude extras/missing nodes then kinds, with 2 reusable-set IDs.

          - `flags.dynamic_2.count` — counts matches of exclude extras/missing nodes then kinds, with 2 reusable-set IDs.

          - `flags.fixed_4.nodes` — visits matches of exclude extras/missing nodes then kinds, with 4 array IDs.

          - `flags.fixed_4.count` — counts matches of exclude extras/missing nodes then kinds, with 4 array IDs.

          - `flags.dynamic_4.nodes` — visits matches of exclude extras/missing nodes then kinds, with 4 reusable-set IDs.

          - `flags.dynamic_4.count` — counts matches of exclude extras/missing nodes then kinds, with 4 reusable-set IDs.

          - `flags.fixed_8.nodes` — visits matches of exclude extras/missing nodes then kinds, with 8 array IDs.

          - `flags.fixed_8.count` — counts matches of exclude extras/missing nodes then kinds, with 8 array IDs.

          - `flags.dynamic_8.nodes` — visits matches of exclude extras/missing nodes then kinds, with 8 reusable-set IDs.

          - `flags.dynamic_8.count` — counts matches of exclude extras/missing nodes then kinds, with 8 reusable-set IDs.

          - `flags.fixed_16.nodes` — visits matches of exclude extras/missing nodes then kinds, with 16 array IDs.

          - `flags.fixed_16.count` — counts matches of exclude extras/missing nodes then kinds, with 16 array IDs.

          - `flags.dynamic_16.nodes` — visits matches of exclude extras/missing nodes then kinds, with 16 reusable-set IDs.

          - `flags.dynamic_16.count` — counts matches of exclude extras/missing nodes then kinds, with 16 reusable-set IDs.

          - `range_field.fixed_2.nodes` — visits matches of byte overlap then field then kinds, with 2 array IDs.

          - `range_field.fixed_2.count` — counts matches of byte overlap then field then kinds, with 2 array IDs.

          - `range_field.dynamic_2.nodes` — visits matches of byte overlap then field then kinds, with 2 reusable-set IDs.

          - `range_field.dynamic_2.count` — counts matches of byte overlap then field then kinds, with 2 reusable-set IDs.

          - `range_field.fixed_4.nodes` — visits matches of byte overlap then field then kinds, with 4 array IDs.

          - `range_field.fixed_4.count` — counts matches of byte overlap then field then kinds, with 4 array IDs.

          - `range_field.dynamic_4.nodes` — visits matches of byte overlap then field then kinds, with 4 reusable-set IDs.

          - `range_field.dynamic_4.count` — counts matches of byte overlap then field then kinds, with 4 reusable-set IDs.

          - `range_field.fixed_8.nodes` — visits matches of byte overlap then field then kinds, with 8 array IDs.

          - `range_field.fixed_8.count` — counts matches of byte overlap then field then kinds, with 8 array IDs.

          - `range_field.dynamic_8.nodes` — visits matches of byte overlap then field then kinds, with 8 reusable-set IDs.

          - `range_field.dynamic_8.count` — counts matches of byte overlap then field then kinds, with 8 reusable-set IDs.

          - `range_field.fixed_16.nodes` — visits matches of byte overlap then field then kinds, with 16 array IDs.

          - `range_field.fixed_16.count` — counts matches of byte overlap then field then kinds, with 16 array IDs.

          - `range_field.dynamic_16.nodes` — visits matches of byte overlap then field then kinds, with 16 reusable-set IDs.

          - `range_field.dynamic_16.count` — counts matches of byte overlap then field then kinds, with 16 reusable-set IDs.

          - `range_kind_field.fixed_2.nodes` — visits matches of byte overlap then kinds then field, with 2 array IDs.

          - `range_kind_field.fixed_2.count` — counts matches of byte overlap then kinds then field, with 2 array IDs.

          - `range_kind_field.dynamic_2.nodes` — visits matches of byte overlap then kinds then field, with 2 reusable-set IDs.

          - `range_kind_field.dynamic_2.count` — counts matches of byte overlap then kinds then field, with 2 reusable-set IDs.

          - `range_kind_field.fixed_4.nodes` — visits matches of byte overlap then kinds then field, with 4 array IDs.

          - `range_kind_field.fixed_4.count` — counts matches of byte overlap then kinds then field, with 4 array IDs.

          - `range_kind_field.dynamic_4.nodes` — visits matches of byte overlap then kinds then field, with 4 reusable-set IDs.

          - `range_kind_field.dynamic_4.count` — counts matches of byte overlap then kinds then field, with 4 reusable-set IDs.

          - `range_kind_field.fixed_8.nodes` — visits matches of byte overlap then kinds then field, with 8 array IDs.

          - `range_kind_field.fixed_8.count` — counts matches of byte overlap then kinds then field, with 8 array IDs.

          - `range_kind_field.dynamic_8.nodes` — visits matches of byte overlap then kinds then field, with 8 reusable-set IDs.

          - `range_kind_field.dynamic_8.count` — counts matches of byte overlap then kinds then field, with 8 reusable-set IDs.

          - `range_kind_field.fixed_16.nodes` — visits matches of byte overlap then kinds then field, with 16 array IDs.

          - `range_kind_field.fixed_16.count` — counts matches of byte overlap then kinds then field, with 16 array IDs.

          - `range_kind_field.dynamic_16.nodes` — visits matches of byte overlap then kinds then field, with 16 reusable-set IDs.

          - `range_kind_field.dynamic_16.count` — counts matches of byte overlap then kinds then field, with 16 reusable-set IDs.

          - `intersection.fixed_2.nodes` — visits matches of selected kinds then the alternate-ID set, with 2 array IDs.

          - `intersection.fixed_2.count` — counts matches of selected kinds then the alternate-ID set, with 2 array IDs.

          - `intersection.dynamic_2.nodes` — visits matches of selected kinds then the alternate-ID set, with 2 reusable-set IDs.

          - `intersection.dynamic_2.count` — counts matches of selected kinds then the alternate-ID set, with 2 reusable-set IDs.

          - `intersection.fixed_4.nodes` — visits matches of selected kinds then the alternate-ID set, with 4 array IDs.

          - `intersection.fixed_4.count` — counts matches of selected kinds then the alternate-ID set, with 4 array IDs.

          - `intersection.dynamic_4.nodes` — visits matches of selected kinds then the alternate-ID set, with 4 reusable-set IDs.

          - `intersection.dynamic_4.count` — counts matches of selected kinds then the alternate-ID set, with 4 reusable-set IDs.

          - `intersection.fixed_8.nodes` — visits matches of selected kinds then the alternate-ID set, with 8 array IDs.

          - `intersection.fixed_8.count` — counts matches of selected kinds then the alternate-ID set, with 8 array IDs.

          - `intersection.dynamic_8.nodes` — visits matches of selected kinds then the alternate-ID set, with 8 reusable-set IDs.

          - `intersection.dynamic_8.count` — counts matches of selected kinds then the alternate-ID set, with 8 reusable-set IDs.

          - `intersection.fixed_16.nodes` — visits matches of selected kinds then the alternate-ID set, with 16 array IDs.

          - `intersection.fixed_16.count` — counts matches of selected kinds then the alternate-ID set, with 16 array IDs.

          - `intersection.dynamic_16.nodes` — visits matches of selected kinds then the alternate-ID set, with 16 reusable-set IDs.

          - `intersection.dynamic_16.count` — counts matches of selected kinds then the alternate-ID set, with 16 reusable-set IDs.

          - `intersection_reverse.fixed_2.nodes` — visits matches of alternate-ID set then selected kinds, with 2 array IDs.

          - `intersection_reverse.fixed_2.count` — counts matches of alternate-ID set then selected kinds, with 2 array IDs.

          - `intersection_reverse.dynamic_2.nodes` — visits matches of alternate-ID set then selected kinds, with 2 reusable-set IDs.

          - `intersection_reverse.dynamic_2.count` — counts matches of alternate-ID set then selected kinds, with 2 reusable-set IDs.

          - `intersection_reverse.fixed_4.nodes` — visits matches of alternate-ID set then selected kinds, with 4 array IDs.

          - `intersection_reverse.fixed_4.count` — counts matches of alternate-ID set then selected kinds, with 4 array IDs.

          - `intersection_reverse.dynamic_4.nodes` — visits matches of alternate-ID set then selected kinds, with 4 reusable-set IDs.

          - `intersection_reverse.dynamic_4.count` — counts matches of alternate-ID set then selected kinds, with 4 reusable-set IDs.

          - `intersection_reverse.fixed_8.nodes` — visits matches of alternate-ID set then selected kinds, with 8 array IDs.

          - `intersection_reverse.fixed_8.count` — counts matches of alternate-ID set then selected kinds, with 8 array IDs.

          - `intersection_reverse.dynamic_8.nodes` — visits matches of alternate-ID set then selected kinds, with 8 reusable-set IDs.

          - `intersection_reverse.dynamic_8.count` — counts matches of alternate-ID set then selected kinds, with 8 reusable-set IDs.

          - `intersection_reverse.fixed_16.nodes` — visits matches of alternate-ID set then selected kinds, with 16 array IDs.

          - `intersection_reverse.fixed_16.count` — counts matches of alternate-ID set then selected kinds, with 16 array IDs.

          - `intersection_reverse.dynamic_16.nodes` — visits matches of alternate-ID set then selected kinds, with 16 reusable-set IDs.

          - `intersection_reverse.dynamic_16.count` — counts matches of alternate-ID set then selected kinds, with 16 reusable-set IDs.

        - Sized kind-filter families:

          - **Keep cardinality boundaries:** fixed arrays, prepared dynamic sets, and sparse/indexed paths can differ at 1/2/4/8/16 IDs. Register with a size macro instead of repeating eight names per size. Remove the unsized single-kind duplicates listed above.

          - `fixed_1.nodes` — visits nodes matching 1 selected kinds using an array.

          - `fixed_1.count` — counts nodes matching 1 selected kinds using an array.

          - `fixed_1.fold` — folds over nodes matching 1 selected kinds using an array.

          - `fixed_1.reverse_nodes` — visits in reverse nodes matching 1 selected kinds using an array.

          - `dynamic_1.nodes` — visits nodes matching 1 selected kinds using a reusable set.

          - `dynamic_1.count` — counts nodes matching 1 selected kinds using a reusable set.

          - `dynamic_1.fold` — folds over nodes matching 1 selected kinds using a reusable set.

          - `dynamic_1.reverse_nodes` — visits in reverse nodes matching 1 selected kinds using a reusable set.

          - `fixed_2.nodes` — visits nodes matching 2 selected kinds using an array.

          - `fixed_2.count` — counts nodes matching 2 selected kinds using an array.

          - `fixed_2.fold` — folds over nodes matching 2 selected kinds using an array.

          - `fixed_2.reverse_nodes` — visits in reverse nodes matching 2 selected kinds using an array.

          - `dynamic_2.nodes` — visits nodes matching 2 selected kinds using a reusable set.

          - `dynamic_2.count` — counts nodes matching 2 selected kinds using a reusable set.

          - `dynamic_2.fold` — folds over nodes matching 2 selected kinds using a reusable set.

          - `dynamic_2.reverse_nodes` — visits in reverse nodes matching 2 selected kinds using a reusable set.

          - `fixed_4.nodes` — visits nodes matching 4 selected kinds using an array.

          - `fixed_4.count` — counts nodes matching 4 selected kinds using an array.

          - `fixed_4.fold` — folds over nodes matching 4 selected kinds using an array.

          - `fixed_4.reverse_nodes` — visits in reverse nodes matching 4 selected kinds using an array.

          - `dynamic_4.nodes` — visits nodes matching 4 selected kinds using a reusable set.

          - `dynamic_4.count` — counts nodes matching 4 selected kinds using a reusable set.

          - `dynamic_4.fold` — folds over nodes matching 4 selected kinds using a reusable set.

          - `dynamic_4.reverse_nodes` — visits in reverse nodes matching 4 selected kinds using a reusable set.

          - `fixed_8.nodes` — visits nodes matching 8 selected kinds using an array.

          - `fixed_8.count` — counts nodes matching 8 selected kinds using an array.

          - `fixed_8.fold` — folds over nodes matching 8 selected kinds using an array.

          - `fixed_8.reverse_nodes` — visits in reverse nodes matching 8 selected kinds using an array.

          - `dynamic_8.nodes` — visits nodes matching 8 selected kinds using a reusable set.

          - `dynamic_8.count` — counts nodes matching 8 selected kinds using a reusable set.

          - `dynamic_8.fold` — folds over nodes matching 8 selected kinds using a reusable set.

          - `dynamic_8.reverse_nodes` — visits in reverse nodes matching 8 selected kinds using a reusable set.

          - `fixed_16.nodes` — visits nodes matching 16 selected kinds using an array.

          - `fixed_16.count` — counts nodes matching 16 selected kinds using an array.

          - `fixed_16.fold` — folds over nodes matching 16 selected kinds using an array.

          - `fixed_16.reverse_nodes` — visits in reverse nodes matching 16 selected kinds using an array.

          - `dynamic_16.nodes` — visits nodes matching 16 selected kinds using a reusable set.

          - `dynamic_16.count` — counts nodes matching 16 selected kinds using a reusable set.

          - `dynamic_16.fold` — folds over nodes matching 16 selected kinds using a reusable set.

          - `dynamic_16.reverse_nodes` — visits in reverse nodes matching 16 selected kinds using a reusable set.

        - Sized field-filter families:

          - **Keep fixed versus dynamic:** even one-field unions use different predicate construction from the dedicated single-field API. `field.scalar` and `scalar_field_1.nodes` have equivalent selection but different source expressions; consolidate only if one scalar baseline is sufficient.

          - `fixed_field_1.nodes` — visits nodes matching 1 selected fields using an array.

          - `fixed_field_1.count` — counts nodes matching 1 selected fields using an array.

          - `dynamic_field_1.nodes` — visits nodes matching 1 selected fields using a reusable set.

          - `dynamic_field_1.count` — counts nodes matching 1 selected fields using a reusable set.

          - `scalar_field_1.nodes` — visits scalar-preorder nodes selected by a 1-field array.

          - `fixed_field_2.nodes` — visits nodes matching 2 selected fields using an array.

          - `fixed_field_2.count` — counts nodes matching 2 selected fields using an array.

          - `dynamic_field_2.nodes` — visits nodes matching 2 selected fields using a reusable set.

          - `dynamic_field_2.count` — counts nodes matching 2 selected fields using a reusable set.

          - `scalar_field_2.nodes` — visits scalar-preorder nodes selected by a 2-field array.

          - `fixed_field_4.nodes` — visits nodes matching 4 selected fields using an array.

          - `fixed_field_4.count` — counts nodes matching 4 selected fields using an array.

          - `dynamic_field_4.nodes` — visits nodes matching 4 selected fields using a reusable set.

          - `dynamic_field_4.count` — counts nodes matching 4 selected fields using a reusable set.

          - `scalar_field_4.nodes` — visits scalar-preorder nodes selected by a 4-field array.

- Dedicated runners and fixtures outside the two crates:

  - `crates/xtask/src/squat.rs` — CLI options and quick/corpus/benchmark dispatch. `quick` also runs persistence checks, so it exceeds this review's requested execution scope.

  - `crates/xtask/src/squat/run.rs` — deterministic corpus staging, grammar builds, fixture inclusion, isolated checks, and benchmark/pressure orchestration.

    - `selection_preserves_language_split_and_size_coverage` — checks split/language/size-bucket selection and independence from input order, including the intentionally omitted size band.

      - **Keep:** selection bias can silently invalidate benchmark coverage without breaking parsing.

  - `tools/squatter/`

    - `README.md` — commands and measurement contracts.

    - `matrix.toml` — repository/grammar selection and isolated, wash, carousel, and bursty/tenant profiles.

    - `summarize.py` — aggregates reports and rejects failed or incompatible comparisons; no standalone tests in this file.

    - `endian.py` — builds the compatibility example for host or 32-bit little-endian and PowerPC64 big-endian, then checks both same-endian and cross-endian reader/writer pairs.

      - **Keep cross-endian pairs:** host byte-layout checks do not execute big-endian accessors. Same-endian pairs provide useful baselines for diagnosing exchange failures; deleting them saves little source.

    - `fixtures/` — corpus regressions, run through the shared corpus checker when the corresponding grammar is selected:

      - `inherited-field.ts` — alias/inherited-field behavior for `typeof object.property`; also staged as TSX.

      - `hidden-seek.sh` — Bash variable expansion and heredoc hidden-node seek behavior.

      - `hidden-seek.css` — CSS selector/block hidden-node seek behavior.

      - `csound-header.orc` — Csound header assignments and direct-parser compatibility.

      - **Keep:** tiny fixtures with grammar-specific behavior. Reusing one fixture across TypeScript/TSX is already effective consolidation.

  - General `corpus-analysis` inventory/grammar/sampling utilities are dependencies of these runners, not additional Squatter-specific suites; their own tests are outside this inventory.

- Suggested order of work:

  - First share existing language/parse/tree-byte/query-snapshot helpers; this removes repeated code without changing coverage.

  - Then eliminate the three exact single-kind benchmark duplicates, consolidate current `all`/preorder aliases if desired, and generate registration/count metadata from one declaration.

  - Merge the small boundary, parser-option, and storage-layout cases into their natural suites while preserving explicitly identified unique assertions.

  - Finally reduce repeated loaded-tree/navigation checks and forwarding-API matrices. Keep synthetic encoding, native differential, borrow-safety, checker-policy, and unusual regression cases unless their replacement exercises the same path and oracle.
