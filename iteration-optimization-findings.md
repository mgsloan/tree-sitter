# Iteration optimization findings

## Confirmed results

All rates below are **million input nodes/s**, including skipped nodes for filters.
Before is the original group-scan prototype, rebuilt with the same benchmark source.

| Operation | Tuning corpus, before → after | Holdout, before → after |
| --- | ---: | ---: |
| Preorder nodes | 110.3 → 112.9 | 110.5 → 111.3 |
| Postorder nodes | 17.1 → 34.9 | 16.7 → 31.2 |
| Reverse postorder nodes | 18.9 → 34.3 | 18.3 → 30.6 |
| Postorder count | 23.8 → 2,152.7 | 22.2 → 2,056.6 |
| Postorder + one kind, nodes | 8.2 → 33.6 | 7.0 → 30.0 |
| One kind, nodes | 124.6 → 315.7 | 88.7 → 342.1 |
| One kind, count | 156.5 → 1,241.0 | 104.0 → 1,200.6 |
| One field, nodes | 204.9 → 348.0 | 175.5 → 275.1 |
| One field, count | 329.0 → 1,464.6 | 310.2 → 1,389.1 |
| Four kinds, nodes | 113.1 → 104.8 | 113.2 → 126.0 |
| Four kinds, count | 194.2 → 250.6 | 196.2 → 252.8 |
| Byte range, count | 2,124.7 → 2,728.5 | 2,563.2 → 3,207.8 |

Preorder enumeration is essentially unchanged. Four-kind enumeration regresses
7% on the tuning corpus and improves 11% on the holdout; counting improves on both.

## What worked

- **Remove metadata copies.** Pending node state needs only a base node, mask,
  and direction. Borrow predicate metadata; inline the set-bit loop and kind
  predicate. Profiles showed that typed adapters alone left substantial `memmove`.
- **Use layout invariants.** Export the group-size shift through the private C
  bridge, replacing division/remainder. Read waste only at group boundaries.
  The slab format stays unchanged.
- **Walk forward postorder once.** Scan descending slots and delay ancestors
  until their subtree ends: O(depth) stack space. Reverse postorder uses pending
  slots without visited flags, but still needs O(nodes) space on wide trees.
- **Count in physical order.** Unconsumed postorder counts can use preorder
  groups with composed pure predicates. After either end advances, preserve
  remaining topology and partial masks. Empty ranges return zero immediately.
- **Specialize IDs.** Single-kind/field equality uses SSE2 on x86_64, scalar
  elsewhere. Singleton masks use one scalar comparison; multi-kind kernels use
  one checked column slice per group. Preserve error-ID remapping and exclude
  waste/subtree slots.

## Experiments not retained

Postorder batching, mask normalization, separate low-bit clearing, branching
before population count, SIMD masking, and outlined multi-kind lookup did not
justify retention. CPU-specific compilation improved plain counts but regressed
some enumeration paths; portable release settings remain the default.

## Measurement and validation

Intel Core Ultra 7 165U, pinned logical CPU 2, Rust 1.95.0, Cargo release defaults,
16-slot groups. Two disjoint 32-file selections cover 11 languages: 747,560 tuning
nodes and 503,590 holdout nodes. Each result is the median of 14 samples from two
processes, targeting 60 ms/sample, with baseline/optimized run order reversed.
Parsing/packing are excluded; scan construction and `black_box` consumption are
included. Filters select common IDs or the middle 1% of source, as documented in
[the benchmark README](tools/squatter/README.md#group-scan-throughput).
These measurements describe repeated-corpus throughput, not universal speedups.

Library/persistence tests and scanning tests at group sizes 16, 32, and 64 pass.
Strict library Clippy passes; benchmark Clippy has only the existing
`collapsible_if` warning at `crates/squatter-bench/src/lib.rs:558`.

Raw data, source snapshots, binary hashes, and profiles are under
`build/iteration-optimization/`: `baseline-final/`, `retained-final/`,
`confirmed-*/results.json`, and `summary.json`. `confirm-retained.py` records the
build/run commands; `summarize.py` pools samples.

## Mask-width experiments

Retained **u64** for traversal throughput. Narrowing to u16 reduced a mask from
8 to 2 bytes but slowed field-filtered postorder from 37.2M to 27.6M input nodes/s.
Different clearing operations recovered preorder/range performance, but not that
regression. Wider register temporaries, constant group arithmetic, and forced
postorder inlining did not justify retention.

u32 was mixed. Fresh comparisons below use the same release settings, five samples
per binary targeting 40 ms, and opposite run order between corpora. Rates are
**million input nodes/s**, u64 → u32:

| Operation | Tuning corpus | Holdout |
| --- | ---: | ---: |
| Preorder nodes | 113.5 → 109.4 | 112.5 → 108.3 |
| Postorder + field, nodes | 38.5 → 38.8 | 32.7 → 34.4 |
| Preorder count | 2,178.3 → 2,444.5 | 2,100.0 → 2,389.1 |
| Four kinds, count | 250.4 → 244.8 | 260.3 → 236.2 |
| Range count | 2,764.4 → 2,828.3 | 3,213.4 → 3,284.3 |

The faster unfiltered counts did not justify slower preorder and multi-kind
counts for the default. Smaller state alone is not a reason to change mask width.
The original u64 implementation is restored; experimental sources/results remain
in `build/iteration-optimization/group-sized-mask-*/`, `speed-first-u32/`, and
`mask-width-{32,64}-{original,holdout}/`.

## Assembly investigation before the cloud optimizations

The baseline SIMD equality kernel was close to what we would write by hand for
SSE2. The complete scan retained iterator calls, repeated predicate preparation,
and group bookkeeping; the cloud experiments below address these costs.

[scan_patterns.rs](crates/squatter/tests/scan_patterns.rs) provides readable JSON
examples and named consumer functions under `patterns`. Tests cover traversal
orders, both reversal forms, subtrees, single/multiple/empty kind sets, fields,
overlap with ancestors, combined predicates, supertypes, early exit, group masks,
and consumption from both ends. Scalar reductions are checked against the legacy
iterator. Only consumer boundaries use `inline(never)`; `black_box` inputs prevent
the fixture from becoming constant arguments. Scan adapters inline normally.

Inspected Rust 1.95.0 / LLVM 22.1.2 on the same Core Ultra 7 165U, 16-slot groups:
default release, release with `target-cpu=native`, and the `optimize` profile
(portable CPU, full Rust LTO, one codegen unit). These are code-generation
observations, not new throughput measurements or a proof of optimality. Inlining
can differ with the surrounding consumer and shared generic instantiations.

| Pattern | Observed optimized code |
| --- | --- |
| `all/preorder/nodes().count()` | One group loop; no node handles or per-node calls. Still constructs a contiguous mask and population-counts it instead of summing its length. |
| Fresh `postorder().count()`, including `.rev()` | Takes the physical-group counting branch, without walking topology or allocating. Construction, the fresh/partial-state branch, and drop checks survive. |
| Field / one-kind count | Inlined SSE2 equality over 16 IDs: two loads, two comparisons, pack, movemask; kinds add two lane shifts. Field target broadcast is hoisted; kind-set cardinality checks, error-ID remapping, and target broadcast repeat per group. |
| Multiple-kind count | Scalar ID decoding and bitset lookup per used slot, including error-ID remapping. No dense SIMD membership kernel in the portable build. |
| Range + kind + field + flags | Predicates share a group loop and short-circuit empty masks. Range matching remains scalar; present flag columns still call `GroupRef::valid_mask` and reread waste, even with LTO. |
| Supertype count | Binary search for the same supertype repeats per group, then scalar membership checks per candidate. The dictionary path retains bounds checks. |
| Preorder slot sum | Calls `Nodes::next` per node, including with LTO. Early-exit lookup uses the same iterator machinery. |
| Reverse preorder slot sum | `next` inlines, but optional pending-group state and a direction test survive inside enumeration. |
| Grouped slot sum | Default release calls `GroupNodes::next` per node. LTO removes that call and the node structure, leaving the six-instruction loop below. |
| Postorder slot sums, either direction | Call `Postorder::take` per node; topology, vector-capacity checks, and growth paths remain. |
| Filtered `start_byte()` sum / scalar kind predicate | Per-node iterator and property calls remain. LTO removes the Rust property wrapper but still calls C `sq_node_start_byte` / `sq_node_symbol`. |

LTO grouped enumeration, actual inner loop in AT&T syntax:

```asm
bsr  %rcx, %rdi       # highest matching slot
btr  %rdi, %rcx       # clear it
add  %eax, %edi       # group base
add  %rdi, %rsi       # consumer's sum
test %rcx, %rcx
jne  loop
```

This is a tight loop for extracting an arbitrary mask. Group setup still does
runtime shifts, subtree clipping, waste loads, bounds checks, and 64-bit mask edge
handling. Even the dense equality path has substantially more surrounding work
than its six-instruction SSE2 kernel. C supplies group size through an opaque
metadata call, so Rust does not specialize the group loop to 16 slots.

Portable population count expands to about 20 scalar instructions. Native uses
`popcnt` and BMI instructions, but retains iterator calls and adds unrolling:
`combined_count` grows from 2,824 to 4,529 bytes, including cold paths. This supports
testing CPU flags per workload, not assuming a blanket speedup.

These observations motivated the cloud experiments below: inline the small `GroupNodes` methods;
provide a group-oriented `Nodes::fold` that avoids per-node iterator state;
prepare kind/supertype predicates once; reuse valid masks for flags; count
unfiltered contiguous spans directly. Preserve the existing subtree, waste, and
mixed-end semantics when experimenting.

Inspect the current code from the repository root:

```sh
cargo test -p tree-squatter --test scan_patterns
cargo rustc -p tree-squatter --test scan_patterns --release -- --emit=asm
RUSTFLAGS='-C target-cpu=native' cargo rustc -p tree-squatter --test scan_patterns --release -- --emit=asm
CARGO_PROFILE_OPTIMIZE_STRIP=false cargo rustc -p tree-squatter --test scan_patterns --profile optimize -- --emit=asm
```

Assembly is in `target/{release,optimize}/deps/scan_patterns-*.s`; look for
`patterns` consumer symbols and follow their callees. For linked disassembly,
run `objdump -Cd --no-show-raw-insn` on the corresponding test executable.
Saved binaries, hashes, full assembly, and extracted consumers are under
`build/scanning-assembly/{portable,native,lto}/`. Both tests pass in debug and all
three inspected configurations; strict Clippy passes for the new test target.

## Google Cloud follow-up (2026-09-19)

Benchmarked on `squatter-benchmark`, `mgsloan-compute/us-central1-a`:
`e2-standard-4`, Intel Broadwell Xeon 2.20 GHz, pinned CPU 1 with its SMT sibling
idle. Rust 1.95.0 / LLVM 22.1.2, portable release defaults, 16-slot groups,
u64 masks. These are cloud measurements, separate from the laptop results above.

Both binaries use the same expanded 41-workload harness; the baseline library is
`5f8f6f786`. The two disjoint corpora contain 747,560 and 503,590 nodes across
32 files each and 11 languages. Parsing/packing are excluded; scan construction
and per-node `black_box` consumption are included. Each invocation checks source
and grammar hashes and validates results against scalar traversal.

Final comparisons use two processes per binary/corpus, seven samples each,
targeting 80 ms/sample. Run order reverses for the second pair; reported rates
are medians of all 14 sample rates. The VM's idle watchdog interrupted an earlier
confirmation attempt; all final pairs ran after recovery on the same CPU
platform, using `squatter-idle run --` to hold its activity lock. Frequency and
host contention are uncontrolled, so these are repeated-corpus measurements.

Rates below are **million input nodes/s**, before → after. Filtered rates
include skipped input nodes; output rates and match counts are saved separately.

| Operation | Tuning corpus, before → after | Second corpus, before → after |
| --- | ---: | ---: |
| Preorder nodes | 141.2 → 338.0 | 141.1 → 336.3 |
| Reverse preorder nodes | 160.3 → 630.6 | 160.1 → 619.8 |
| Postorder nodes | 42.1 → 50.9 | 34.9 → 45.6 |
| Reverse postorder nodes | 38.3 → 40.8 | 32.4 → 37.4 |
| Preorder fold | 156.9 → 484.3 | 158.1 → 480.0 |
| Grouped fold | 273.0 → 488.6 | 272.7 → 478.0 |
| Unfiltered count | 2,149.8 → 6,541.0 | 2,077.8 → 6,396.0 |
| One kind, nodes | 365.5 → 487.3 | 404.6 → 473.7 |
| One kind, count | 908.9 → 1,092.3 | 889.6 → 1,043.9 |
| Four kinds, nodes | 127.9 → 234.6 | 147.4 → 244.5 |
| Four kinds, count | 195.4 → 517.9 | 202.0 → 524.9 |
| One field, nodes | 434.5 → 951.3 | 355.6 → 806.2 |
| One field, count | 1,220.5 → 1,165.8 | 1,177.8 → 1,122.3 |
| Supertype count | 536.2 → 562.2 | 558.8 → 601.4 |
| Flag count | 953.0 → 1,557.8 | 786.9 → 1,375.9 |
| Kind + field + flags, count | 522.0 → 804.1 | 503.3 → 741.3 |
| Byte range, nodes | 1,745.1 → 2,629.7 | 2,094.1 → 3,146.2 |
| Byte range, count | 2,673.1 → 2,598.8 | 3,174.9 → 3,131.7 |

Field-only counts regressed 4.5–4.7%; byte-range counts regressed 1.4–2.8%.
These remain tradeoffs of the combined candidate. The `native_iterator`,
`scalar_next_preorder`, and `mainline_cursor` controls stayed within about 1.1%.
The large enumeration/count gains were consistent across both corpora.

Retained changes:

- **Inline node extraction and specialize folds.** Small group methods inline;
  `Nodes::fold`/`rfold` drain a group before acquiring the next. Pending groups
  preserve their original extraction direction after mixed-end consumption.
- **Prepare predicates once.** Cache empty/single/multiple-kind selection,
  single-kind error-ID encoding, and the grammar's supertype index when attaching
  a filter. Removing preparation slowed combined counts by about 20% in screening.
- **Count contiguous spans directly.** Unfiltered counts sum clipped live slots
  instead of constructing and population-counting masks. Fresh postorder counts
  share this path; partially consumed scans preserve remaining traversal state.
- **Reuse valid candidates.** Extra/missing bitmaps intersect the existing mask,
  avoiding another waste load. Range-only counts avoid an unnecessary identity
  predicate layer.
- **Use SIMD for 2–4 kinds.** OR the existing SSE2 equality masks for dense
  candidates. Singleton candidates and larger sets retain scalar lookup.

Experiments not retained:

- A custom SIMD kernel loading the kind column once improved four-kind counts
  another 14–15%, but slowed single-kind enumeration about 6% and combined counts
  9–11% in screening. The simpler composition gave a better overall result.
- Extending SIMD through eight kinds improved eight-kind counts 12–18%, but
  slowed two/four-kind counts 5–7%. Crossover runs tested 2, 4, 8, and 16 kinds on
  both corpora. Four is the retained tradeoff, not a proven universal cutoff.
- Removing the custom folds slowed preorder folds about 21%. Inlining choices
  also moved unrelated code enough to change throughput; isolated gains did not
  simply add together. The final combined binary was measured independently.

Supertype workloads include unsupported grammars' empty-result path. Actual
supertype-enabled inputs account for 319,370 tuning and 195,274 second-corpus
nodes; do not interpret the aggregate rate as dense supertype lookup throughput.

Updated portable assembly removes per-node calls from preorder and grouped slot
sums without LTO, and unfiltered counts no longer contain software population
counts. Filtered property consumers still have per-group calls and per-node C
property calls; postorder still pays for topology and pending-state management.
The complete scan is not universally optimal.

Validation passed for the library/persistence suite, scan tests with 16-, 32-,
and 64-slot groups, release assembly probes, and strict library/test Clippy.
Tests exercise folds after mixed-end consumption and small kind sets containing
error and invalid IDs. Changed Rust files pass formatting checks; workspace-wide
formatting still reports the pre-existing layout in `squatter-bench/src/compare.rs`.

Artifacts are under `build/cloud-iteration/`: final sources/binary hashes and
`final-{a,b}-{tuning,holdout}.json` in `baseline-final/` and `count-inline/`,
all pooled samples in `final-summary.json`, and current disassembly in
`count-inline/asm/`. `experiment.py`, `final-confirm.py`, and `final-summary.py`
record build, upload, execution, and aggregation. `crossover.py` records the
kind-count experiment. Locally built binaries needed only their ELF interpreter
path changed for Ubuntu; uploaded binary hashes were checked before execution.
The VM was restored to its initial `TERMINATED` state after measurement.

## Compile-time ID sets and field unions (2026-09-19)

Retained const-generic arrays for all existing set-valued scan entry points:
`filter_kind_ids`, `Node::descendants_matching_kinds`, and `NodeLike` on both
backends. Added `filter_field_ids` with the same input forms: `[u16; N]`,
`&[u16; N]`, or a reusable `&IdSet`. `KindSet` remains an alias for `IdSet`.
Sets mean OR, successive filters mean AND, duplicates do not duplicate results,
and an empty set matches nothing. Field zero includes nodes without a field.

The restarted GCP `e2-standard-4` instance received **AMD EPYC 7B12**, not the
earlier Broadwell host. All comparisons here were rerun on this boot, pinned to
CPU 1 with sibling 3 idle, using portable release builds and 16-slot groups.
The same two 32-file corpora contain 747,560 and 503,590 nodes. Each final rate
pools 14 samples from two processes, targeting 80 ms/sample, with workload order
reversed in the second process. Array and dynamic cases share the same binary,
trees, selected IDs, and expected matches. The previous binary was also rerun
on this CPU as a check on the original workloads.

Rates are **million input nodes/s, dynamic set → fixed array**. Reusable dynamic
sets are built before timing; both paths include scan and predicate preparation.
Arrays select frequent IDs and repeat the most frequent if fewer than N exist;
dynamic sets deduplicate that same selection. N counts supplied entries, not
necessarily distinct matching IDs. Per-file selections are saved in the reports.

Kind sets:

| Supplied IDs | Tuning nodes/s | Tuning count nodes/s | Second nodes/s | Second count nodes/s |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 676.9 → 745.1 | 2,436.9 → 2,540.1 | 674.9 → 734.4 | 2,344.1 → 2,440.9 |
| 2 | 475.7 → 839.6 | 1,358.2 → 2,130.2 | 481.4 → 814.0 | 1,330.1 → 2,051.5 |
| 4 | 349.6 → 656.2 | 931.8 → 2,257.2 | 366.1 → 679.5 | 922.9 → 2,206.2 |
| 8 | 245.3 → 421.9 | 520.9 → 1,843.6 | 250.2 → 436.5 | 521.0 → 1,789.5 |
| 16 | 240.2 → 308.8 | 527.5 → 770.0 | 239.1 → 306.2 | 522.5 → 759.3 |

Field sets:

| Supplied IDs | Tuning nodes/s | Tuning count nodes/s | Second nodes/s | Second count nodes/s |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 781.6 → 1,304.2 | 1,400.6 → 2,584.6 | 639.6 → 1,023.0 | 1,350.0 → 2,506.0 |
| 2 | 645.2 → 1,012.0 | 1,231.9 → 2,132.2 | 538.2 → 799.1 | 1,193.3 → 2,067.1 |
| 4 | 603.5 → 978.8 | 1,101.5 → 2,298.4 | 481.2 → 729.4 | 1,050.0 → 2,228.1 |

The older-binary check showed plain enumeration and dynamic four-kind counts
about 4–5% slower in the new build. Most other original workloads moved a few
percent. That comparison also changes the harness and its allocations; unchanged
traversal controls varied by up to 5.8%, so it does not isolate the cause. The
same-binary array/dynamic comparisons above are the stronger evidence.

What worked:

- **Preserve N through the entire pipeline.** Fixed predicates own arrays, with
  no allocation or dynamic cardinality dispatch. Kind IDs are encoded once;
  invalid entries repeat a valid target, or mark the selection empty if none
  exist. This preserves fixed comparison counts without reserving a u16 sentinel.
- **Choose kernels at compile time.** One target uses existing equality; two
  combine equality masks; larger arrays load each column chunk once and compare
  all targets. Sparse single-candidate groups use direct membership checks.
- **Inline the shared kernel deliberately.** Screening ordinary vs forced
  inlining raised eight-kind counts from about 1,030M to 1,830M nodes/s, while
  reducing enumeration from about 557M to 415M. Both beat dynamic sets. For two
  targets, composing equality kernels recovered enumeration from about 600M to
  840M, trading some count throughput. Const generics help but do not remove
  consumer-dependent code-generation tradeoffs.

Portable assembly confirms four/eight targets become straight-line comparisons
inside the column loop: two vector loads, eight/sixteen word comparisons, ORs,
and one final movemask per 16 slots. Target mapping and broadcasts are outside
that loop; there is no runtime target-count loop. Grammar/error preparation,
group clipping, bounds checks, and software population count still cost work.

Library/persistence tests, strict library/scan-test Clippy, and scan tests at
16/32/64 slots pass. Tests cover borrowed/owned arrays, empty/duplicate/invalid
IDs, error IDs, field zero, subtrees, compositions, and mixed-end folds. Benchmark
Clippy still reports only its existing `collapsible_if` warning.

The next useful set filter is **OR over supertypes**: resolve grammar indices
once and combine membership masks while preserving actual node membership.
Boolean flag filters gain nothing from sets; selecting both values is identity.

Artifacts: `build/cloud-iteration/const-{or,shared,forced,hybrid,final}/` stores
source snapshots, hashes, and measurements; `const-hybrid/asm/` holds inspected
assembly and executed probes. `const-confirm.py` and `const-summary.py` reproduce
the final comparison and aggregation. Final reports are
`const-final/confirm-{a,b}-{tuning,holdout}.json`; older-API controls are in
`count-inline/const-final-{a,b}-{tuning,holdout}.json`.
The instance was restored to its initial `TERMINATED` state after measurement.
