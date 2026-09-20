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

## One-direction scans and postorder (2026-09-19)

This candidate was rejected because it regressed preorder. The replacement is
described in the following section.

`Scan::rev()` now changes the source type before consumption. Forward and reverse
postorder keep only their own topology state; node iterators retain one mask.
Use `.rev().nodes()` or `.rev().groups()`; the resulting iterators no longer
support `next_back()`. An individual group's iterator still uses one mask and
supports both ends.

Rust retains borrowed slices for slab bytes and native grammar tables. Raw
pointers are confined to the C bridge; scans and groups inherit `Send + Sync`.
On x86_64, preorder node iterators shrink from 208 to 176 bytes, and both postorder
directions shrink from 288 to 200 bytes.

The internal group protocol returns a mask and borrows metadata retained by the
source. Only public group results copy the descriptor. Inlining postorder's
separate traversal methods removes per-node traversal calls from the slot-sum
probes. Reverse traversal defers child expansion until the following call and
returns the last child directly, queuing only earlier siblings. Unary paths need
no pending allocation. A Rust allocation probe yields the document and array
roots without allocating for arrays of 16, 10,000, and 100,000 elements.
Reverse traversal still needs O(nodes) pending space on wide trees.

Compared with branch head `75a9c43c9` on the Core Ultra 7 165U, pinned CPU 2,
Rust 1.95.0, portable release settings, 16-slot groups. The existing disjoint
32-file corpora contain 747,560 and 503,590 nodes across 11 languages. Each result
pools ten samples from two processes, targeting 40 ms/sample. Binary order and
workload order reverse for the second pair. No builds run during confirmation.
Rates are **million input nodes/s**, before → after:

| Operation | Tuning corpus | Second corpus |
| --- | ---: | ---: |
| Postorder nodes | 119.7 → 235.0 | 106.1 → 177.7 |
| Reverse postorder nodes | 100.7 → 192.7 | 97.1 → 152.5 |
| Postorder fold | 129.7 → 234.7 | 113.1 → 179.4 |
| Reverse postorder fold | 118.5 → 192.2 | 104.0 → 152.6 |
| Postorder + kind, nodes | 106.7 → 168.0 | 93.6 → 136.3 |
| Reverse postorder + kind, nodes | 99.3 → 161.0 | 86.3 → 130.3 |
| Postorder + field, nodes | 112.9 → 185.6 | 97.5 → 146.9 |
| Preorder nodes | 764.5 → 684.6 | 765.3 → 687.3 |
| Reverse preorder nodes | 1,743.9 → 1,459.9 | 1,679.4 → 1,385.3 |
| Preorder + kind, nodes | 1,210.6 → 1,568.0 | 1,137.5 → 1,360.2 |
| Byte range, nodes | 7,735.3 → 7,728.8 | 9,068.8 → 8,981.3 |

Plain preorder regresses about 10% forward and 16–18% reverse. In the reverse
consumer, assembly spills the tree pointer and counter in the per-node loop,
while the baseline keeps them in registers. Inlining and register allocation
remain consumer-dependent; these measurements do not isolate a single cause.
Unfiltered counts stay within 2.3% and the scalar traversal control within 0.2%.

Experiments with an empty mask as the exhaustion sentinel, alternative bit
clearing, and a pending `GroupNodes` did not justify retention. Empty-filter
short-circuiting remains deferred; no additional exhaustion check was added for
that case. Fragment batching was not revisited.

Validation passes for library/persistence tests, scanning tests at group sizes
16/32/64, strict library/test Clippy, and benchmark compilation. Tests cover
partial consumption in each direction, deep and wide trees, and moving scans
between threads. The benchmark affinity function is now platform-gated.

Artifacts are in `build/postorder-optimization/`: `confirmation.json` contains
pooled results, compiler/CPU metadata, and binary hashes; `confirm.py` records
commands and aggregation. Baseline/candidate binaries and source snapshots,
individual reports, disassembly, and the allocation probe are retained there.

## Preorder recovery (2026-09-19)

The shared node-iterator rewrite changed inlining and register allocation for
preorder. The first candidate's reverse loop spills its tree pointer and count,
where the original consumer keeps them in registers. Its forward fold also
regresses substantially. Slab reads already used checked byte slices before the
rewrite; these were not newly introduced bounds checks. Smaller iterator state
alone did not produce better machine code.

Unfiltered preorder now produces contiguous slot ranges directly, with no mask
construction or per-node bit scan. The source type selects forward or reversed
range iteration. Predicates still use the original mask-producing loop, and
postorder uses sparse fragments. `fold()` drains each fragment locally rather
than updating shared pending state for every node. The iterator stores a base
slot instead of another tree pointer. The single-ID SIMD kernel handles its first
16 slots directly, avoiding loop-carried mask and offset work for a 16-slot group.

The same CPU, compiler, corpora, and sampling procedure as above were used for
22 workloads. Each comparison pools ten samples from two processes, reversing
binary and workload order; no builds ran during measurement. Baseline is the
original branch head, not the rejected candidate. Rates are **million input
nodes/s**, baseline → retained:

| Operation | Tuning corpus | Second corpus |
| --- | ---: | ---: |
| Preorder nodes | 772.6 → 1,970.4 | 779.8 → 1,936.8 |
| Reverse preorder nodes | 1,805.0 → 2,007.3 | 1,747.9 → 1,953.8 |
| Preorder fold | 1,230.8 → 3,180.3 | 1,201.0 → 3,041.8 |
| Reverse preorder fold | 1,238.0 → 3,312.1 | 1,199.6 → 3,130.5 |
| Preorder + kind, nodes | 1,254.6 → 1,501.8 | 1,176.5 → 1,353.3 |
| Preorder + kind, count | 3,830.3 → 4,025.5 | 3,713.7 → 3,824.3 |
| Preorder + field, nodes | 2,764.5 → 2,900.5 | 2,088.5 → 2,156.2 |
| Byte range, nodes | 7,967.2 → 7,729.8 | 9,351.5 → 9,031.3 |
| Byte range, count | 7,996.4 → 8,535.6 | 9,318.1 → 9,915.7 |
| Postorder nodes | 123.7 → 244.0 | 108.6 → 187.4 |
| Reverse postorder nodes | 115.8 → 200.7 | 100.0 → 161.1 |

Plain preorder is 2.48–2.55× faster forward and 11–12% faster reverse. Group folds
and unfiltered counts remain within 1%. Byte-range enumeration retains a 3–3.4%
regression; range counts improve 6–7%. Outlining its matching pipeline made that
regression worse and was discarded. The unchanged scalar traversal control rose
about 5%; results describe these binaries and consumers, not portable guarantees.

The squatter and persistence suites pass. The final scan and example tests pass
at group sizes 16/32/64, including partial folds/counts and subtree boundaries;
strict Clippy and release benchmark compilation pass. Borrowed slices and
automatic `Send + Sync` are preserved.

Artifacts are in `build/preorder-recovery/`: `confirmation.json` records all 22
workloads and binary hashes, `confirm.py` reproduces the comparison, and
`extract-assembly.py` identifies consumers through the workload table. Original,
rejected, and retained binaries and source snapshots are saved alongside trials.

## Register allocation and byte-range recovery (2026-09-19)

The retained release binary confirms that both preorder `fold()` consumers keep
their accumulator, tree pointer, and slot iteration state in registers inside
the per-node loop. The two stack stores materialize the `Node` passed to
`black_box`; they are not spills. There are no calls or direction checks in these
loops. Group transitions still load spilled metadata and check slice bounds.
The separate forward/reverse slot-summing probes vectorize four slots at a time,
with register-only scalar tails and no per-node `Node` materialization.

Ordinary `next()` consumers still have spills: forward increments a stack-resident
benchmark counter; reverse also reloads the tree pointer for each node. This
does not apply to the fold loops. The new byte-range slot-summing probe calls the
predicate pipeline once per group, then drains matches with the mask, base, and
accumulator in registers. The predicate pipeline itself still has metadata
spills and bounds checks; the entire scan is not spill-free.

Register allocation does not explain all timing changes. A discarded byte-range
arithmetic experiment left the entire forward preorder consumer's instructions
and registers unchanged after normalizing addresses, yet reduced throughput
from roughly 1,900 to 1,225–1,420 million input nodes/s. Its inner loop moved from
`0x7ab00` to `0x7aa70`, crossing a 64-byte boundary. Compiler alignment-only
experiments also changed timings substantially. This supports code-placement
sensitivity, without isolating a particular hardware mechanism. No alignment
flags are retained. The final forward/reverse preorder node and fold consumers
also match the previous version after address normalization.

The retained production change is limited to predicate-mask iteration:
`remaining &= remaining - 1` replaces clearing the bit through its slot index.
The pending mask update compiles to `lea`/`and`, replacing `mov -2`/`rol`/`and`
and removing its dependency on the bit-scan result. Constructing the output mask
still needs the slot-dependent shift. Original range arithmetic, relative slot
indices, borrowed slices, and automatic `Send + Sync` are preserved. A trial
using absolute slot ranges hurt forward folds by about 28% and was discarded.

The confirmation compares the original branch head (`75a9c43c9`), the previous
retained version, and this change on CPU 2 of an Intel Core Ultra 7 165U with
rustc 1.95.0. As above, each result pools ten samples from two processes with
binary and workload order reversed; no builds ran during measurement. Rates are
**million input nodes/s**, previous → retained:

| Operation | Tuning corpus | Second corpus |
| --- | ---: | ---: |
| Preorder nodes | 1,937.0 → 1,935.6 | 1,863.8 → 1,868.3 |
| Reverse preorder nodes | 1,952.4 → 1,973.6 | 1,934.9 → 1,872.0 |
| Preorder fold | 3,142.9 → 3,122.2 | 3,005.7 → 2,967.8 |
| Reverse preorder fold | 3,225.6 → 3,197.7 | 3,042.9 → 3,035.8 |
| Byte range, nodes | 7,503.0 → 7,741.2 | 8,779.4 → 9,105.2 |
| Byte range, count | 8,260.2 → 8,829.0 | 9,530.6 → 10,295.9 |
| Postorder nodes | 237.3 → 236.6 | 182.4 → 182.1 |
| Reverse postorder nodes | 195.5 → 195.1 | 156.6 → 156.5 |

Byte-range enumeration improves 3.2–3.7%, bringing it within 0.7% of the original
branch; range counts are now 14% faster than that baseline. Plain preorder
remains 2.44–2.49× faster forward and 11–12% faster reverse, with folds
2.48–2.56× faster. Reverse enumeration drops 3.3% against the previous version on
the second corpus despite unchanged instructions; the unchanged scalar control
drops 4–5% on both corpora. These are measurements of particular binaries, not
portable guarantees. The supertype workload also benefits (51–60% against the
previous version); all 22 workloads are recorded in the report.

Scanning and example tests pass at group sizes 16/32/64, including the new range
reduction probe checked against scalar navigation. Strict library/test Clippy
and normal release compilation pass. Artifacts in `build/range-optimization/`
include `confirmation.json`, the reproduction script `confirm.py`, all three
binaries and their hashes, extracted consumers, source snapshots, and
`final-scan-patterns.s`.

## Selection API refresh (2026-09-19)

The byte/point relation implementation was measured against the saved
`5909281d9` binary above. The current code includes empty-query containment for
both `within_*` and `containing_*`. Node enumeration and folds are the priority;
count throughput is secondary.

The benchmark now includes point overlap enumeration, folds, counts, and scalar
navigation, plus byte overlap folds. Byte and point queries describe the same
1%-of-source interval near each file's midpoint. Point bounds are computed
before timing; points are stored in the slab. Validation compares point scan
nodes with scalar point getters and checks the byte/point match counts agree.

Measurements use CPU 2 of the same Intel Core Ultra 7 165U, rustc 1.95.0, default
release settings, and 16-slot groups. Each corpus has 32 files in 11 languages:
747,560 input nodes for tuning and 503,590 for holdout. Ten 40 ms samples are
pooled from two processes per binary/corpus, with binary and workload order
reversed. No builds ran during measurement. Rates are **million input nodes/s**,
previous → current with the expanded benchmark:

| Operation | Tuning corpus | Holdout corpus |
| --- | ---: | ---: |
| Preorder nodes | 1,919.3 → 1,965.6 | 1,871.5 → 1,832.1 |
| Reverse preorder nodes | 1,956.7 → 1,955.3 | 1,886.7 → 1,860.4 |
| Preorder fold | 3,070.5 → 3,095.5 | 2,896.4 → 2,950.9 |
| Reverse preorder fold | 3,163.6 → 3,177.9 | 2,995.6 → 2,999.3 |
| Byte overlap nodes | 7,718.5 → 8,413.2 | 9,021.7 → 9,648.7 |
| Byte overlap count | 8,712.4 → 7,074.5 | 10,178.6 → 8,196.7 |

Byte overlap enumeration improves 7–9%; preorder node/fold rates remain within
2.5%. These figures remain sensitive to code placement. A current-code build
using the original benchmark harness instead measures forward preorder nodes
at 1,407.5/1,308.7 million/s, 27–30% below the previous binary, and reverse nodes
about 9% lower. All four plain preorder node/fold consumers have identical
instructions and registers across all three binaries after address
normalization. The original-harness forward loop crosses a 64-byte boundary;
the previous and expanded-harness loops do not. This supports the code-placement
explanation from the earlier investigation, without identifying the hardware
mechanism. Folds stay within 2.5% in both harnesses. No alignment flags or
production traversal changes were added for this refresh.

For the same queries in the expanded benchmark:

| Operation | Tuning corpus | Holdout corpus |
| --- | ---: | ---: |
| Byte overlap nodes | 8,413.2 | 9,648.7 |
| Point overlap nodes | 7,588.3 | 8,557.5 |
| Byte overlap fold | 8,463.9 | 9,704.6 |
| Point overlap fold | 7,225.5 | 8,191.5 |

Point enumeration has 10–11% lower throughput than bytes; point folds are
15–16% lower. Input-node rates include skipped nodes: the selected outputs are
9,589/5,196 nodes per corpus pass. The previous implementation returns one fewer
holdout node because it excluded zero-width nodes from overlap queries.

Byte overlap counts regress 19–20% in both harnesses. The new consumer makes two
152-byte state copies before calling an outlined `count_matches`; its group loop
also checks delta-column bounds before rejecting groups by their position bases.
These are investigation candidates, not established explanations of the full
regression. Counting optimization is lower priority than node-value scans.

Return to lower-bound seeking for `starting_in_*`, `starting_at_*`, and
`within_*`: their start bounds can skip earlier groups as well as later ones.
Current restriction uses only the upper start bound. Overlap and containing
queries must preserve earlier-starting ancestors.

All 12 scanning tests and strict library/test Clippy pass. Artifacts in
`build/selection-refresh/` include `confirm.py`, `confirmation.json`, per-run
reports, both current binaries and source snapshots, extracted consumers, and
`codegen-comparison.txt`. The previous binary remains in
`build/range-optimization/final`.

## Encoded range comparisons on Google Cloud (2026-09-19)

Range filters now translate byte and point bounds into intervals of stored
deltas once per endpoint column. Dense masks use SSE2 over `u8` byte starts or
`u16` byte ends and point deltas; masks with one or two candidates remain scalar.
Exact endpoints use equality. Column conversion is forced inline so each
relation's constant bound variants specialize before the kernel. Oversized point
queries retain decoded `Point` comparisons. See [range-scans.md](range-scans.md)
for the implementation and remaining opportunities.

Compared baseline `d0108cc86` with `972f74162` on `squatter-benchmark`,
`mgsloan-compute/us-central1-a`, `e2-standard-4`. This VM start selected an
**AMD EPYC 7B12**, unlike the Broadwell instance used for earlier cloud results.
Both binaries use rustc 1.95.0, portable release settings, the same benchmark
source, and 16-slot groups. Only the ELF interpreter path was changed for the
cloud host. Runs held the `squatter-idle` activity lock and benchmark lock, pinned
to CPU 1, with no competing benchmark or compilation on the instance.

Each corpus contains 32 files across 11 languages: 747,560 tuning input nodes
and 503,590 holdout nodes. Byte and stored-point overlap queries cover the middle
1% of each source, producing 9,589 and 5,196 matches. Scan construction is timed;
parsing, packing, and validation are excluded. Each result is the median of
14 samples from two processes, targeting 80 ms/sample. Binary and workload order
reverse for the second pass. The harness checks source/grammar hashes and scalar
agreement; output counts also agree across binaries.

Rates are **million input nodes/s**, including nodes skipped by range pruning:

| Operation | Tuning, before → after | Holdout, before → after |
| --- | ---: | ---: |
| Byte overlap nodes | 4,749.7 → 5,096.2 (+7.3%) | 5,602.9 → 5,981.1 (+6.8%) |
| Byte overlap fold | 4,773.9 → 5,115.0 (+7.1%) | 5,630.7 → 6,013.8 (+6.8%) |
| Byte overlap count | 4,019.4 → 4,116.8 (+2.4%) | 4,748.3 → 4,897.6 (+3.1%) |
| Point overlap nodes | 4,517.2 → 4,584.2 (+1.5%) | 5,109.6 → 5,312.5 (+4.0%) |
| Point overlap fold | 4,515.9 → 4,606.5 (+2.0%) | 5,166.7 → 5,339.1 (+3.3%) |
| Point overlap count | 3,508.9 → 3,773.9 (+7.6%) | 4,042.9 → 4,417.9 (+9.3%) |
| Preorder nodes | 1,345.2 → 1,350.1 (+0.4%) | 1,316.4 → 1,305.3 (-0.8%) |
| Preorder fold | 1,747.2 → 1,797.8 (+2.9%) | 1,721.9 → 1,734.9 (+0.8%) |
| Scalar preorder control | 64.2 → 63.2 (-1.5%) | 66.9 → 63.3 (-5.5%) |

All six overlap workloads improve on both corpora. The unchanged scalar control
slows down, particularly on holdout; code placement and VM variability remain
possible influences. These measurements establish results for these binaries
and overlap queries, not uniform gains across all relations or selectivities.

Exhaustive delta tests, scanning tests, and example tests pass at group sizes
16/32/64. The full `tree-squatter` suite, including bindings and doctests, passes
at the default size; strict library/test Clippy passes. Tests compare encoded
filtering with decoded positions across every `u16` delta, signed-lane boundaries,
empty/sparse/dense masks, and partial groups. Existing relation tests cover
zero-width nodes, empty queries, traversal direction, and absent/oversized points.

Downloaded artifacts are in `build/delta-scans/delta-scans-20260919/`: `run.py`,
`summary.json`, individual reports/logs, source snapshots, and both binaries with
verified hashes. The instance was returned to its previous stopped state.

## Range group rejection profiling (2026-09-19)

Two changes make rejected groups cheaper:

- `fd24c8692` defers delta-slice construction and its bounds checks until a
  comparison needs deltas. Whole-column acceptance and rejection need only bases.
- `c0c7ba2ec` defers candidate-mask construction until a range predicate accepts
  the group's conservative bounds. Rejected groups skip waste lookup, subtree
  clipping, and mask construction. Preorder enumeration and counting share this
  path, including composed predicates.

Neither change alters the packed format or introduces unchecked reads. Other
traversals and predicates retain the ordinary mask path.

### Profiles and assembly

On the cloud VM, hardware performance counters were unavailable. Profiles use
`perf record -e cpu-clock:u -F 997`, with twelve 500 ms samples of byte or point
overlap enumeration on the tuning corpus. Sampling includes setup; benchmark
timings exclude it. No samples were lost. Instruction percentages are approximate
software-sampling evidence, not cycle counts.

Before these changes, `Restricted::next_mask` accounts for 78% of byte-profile
samples and 77% of point-profile samples. Within the byte function, 57.5% falls
in group iteration and candidate-mask construction, 36.7% in column setup and
rejection, and only 2.9% in surviving-group comparisons. Disassembly confirms
that rejected groups construct masks and check delta-slice bounds first.

The final assembly branches back to group iteration before waste lookup, mask
construction, or delta-slice checks. Follow-up profiles still place 74% of both
workloads in `next_mask`; within the byte function, 87% now falls in its shorter
iteration/rejection loop. This points toward skipping groups as the next target.
The byte function's stack allocation remains 184 bytes, and total function size
does not shrink: the gain comes from avoiding work on the frequent path.

Point query packing is already hoisted out of the per-group rejection loop in
release assembly, though it repeats on calls to `next_mask`. Moving the source
conversion alone should not be assumed to save work for every rejected group.
The SIMD comparison regions receive few samples for these narrow queries.

### Cloud comparison

This start of `squatter-benchmark` selected an **Intel Xeon Broadwell**, model 79,
at 2.20 GHz, on the same `e2-standard-4` instance in
`mgsloan-compute/us-central1-a`. The preceding encoded-delta experiment ran on
AMD EPYC; its absolute rates are not directly comparable.

The baseline scan source is `1999904af`; the final source is `c0c7ba2ec`. Both
use the same benchmark harness from `c564d636f`, built in the same worktree path
with rustc 1.95.0, portable release settings, and 16-slot groups. Only the ELF
interpreter path was patched for the cloud host. Runs held the activity and
benchmark locks, used CPU 1, and had no competing benchmark or compilation.

The tuning/holdout corpora contain 747,560/503,590 input nodes across 32 files
and 11 languages each. Midpoint 1% overlap queries produce 9,589/5,196 matches.
Each result is the median of 14 samples from two processes, targeting 80 ms per
sample; binary and workload order reverse on the second pass. Scan construction
is timed; parsing, packing, and validation are excluded. The harness verifies
source/grammar hashes and scalar agreement; match counts agree across variants.

Rates are **million input nodes/s**, including skipped nodes:

| Operation | Tuning, before → after | Holdout, before → after |
| --- | ---: | ---: |
| Byte overlap nodes | 2,934.2 → 4,964.3 (+69.2%) | 3,455.4 → 5,750.4 (+66.4%) |
| Byte overlap fold | 2,883.0 → 4,896.9 (+69.9%) | 3,397.3 → 5,635.9 (+65.9%) |
| Byte overlap count | 1,768.7 → 6,298.9 (+256.1%) | 2,119.1 → 7,205.1 (+240.0%) |
| Point overlap nodes | 2,790.5 → 4,428.1 (+58.7%) | 3,247.5 → 5,071.4 (+56.2%) |
| Point overlap fold | 2,731.8 → 4,355.3 (+59.4%) | 3,211.8 → 4,922.4 (+53.3%) |
| Point overlap count | 1,986.4 → 7,639.2 (+284.6%) | 2,333.8 → 8,381.0 (+259.1%) |
| Preorder nodes | 580.6 → 579.2 (-0.3%) | 576.9 → 575.0 (-0.3%) |
| Preorder fold | 917.6 → 915.7 (-0.2%) | 872.5 → 870.0 (-0.3%) |
| Scalar preorder control | 49.9 → 51.4 (+3.1%) | 50.8 → 51.4 (+1.2%) |

The benchmark now accepts `--range-start-percent` and `--range-percent`, keeping
the midpoint 1% default. Short checks at other positions use one process and
three 50 ms samples per variant/corpus, with baseline first:

| Query | Byte nodes, tuning / holdout | Point nodes, tuning / holdout |
| --- | ---: | ---: |
| First 1% | +1.3% / +1.5% | +12.8% / +13.2% |
| Last 1% | +80.1% / +77.5% | +65.2% / +60.7% |
| Whole source | +1.8% / +2.6% | +13.7% / +38.9% |

All six overlap workloads improve in these checks. They provide less evidence
than the midpoint confirmation; code placement and VM variation can influence
individual gains. The stronger late-query gains fit the larger rejected prefix.
These results do not establish gains for every range relation or selectivity.

### Remaining opportunities

- Skip earlier groups for `starting_in_*`, `starting_at_*`, and `within_*`, using
  their lower start bound as well as the existing upper bound.
- For overlap, investigate subtree skipping or hierarchical maximum-end
  summaries. A start-only lower bound would incorrectly omit ancestors. The
  current loop still visits preceding groups even when each rejection is cheap.
- Consider separating the oversized-point fallback from the compact-point path
  to reduce setup and code size. Measure before retaining it; query packing is
  already partly hoisted by the compiler.
- Profile broad queries before tuning SIMD further. Fusing start/end masks or
  changing dense-kernel thresholds targets little of the measured narrow-query
  cost.

Exhaustive delta tests, scanning tests, and pattern tests pass at group sizes
16/32/64. The full default-size suite, including bindings and doctests, strict
library/test Clippy, and formatting checks pass.

Artifacts in `build/range-profile/` include source snapshots, `matrix-build.json`,
build/comparison/profile scripts, and extracted assembly. Its downloaded
`range-profile-20260919/` directory contains raw perf data, annotations, reports,
per-run benchmark data, summaries, and binaries. Local and downloaded binary
hashes agree with the build manifest, as do the committed scan and harness
sources. The instance was returned to its previous stopped state.

## Direct supertype SIMD (2026-09-19)

`filter_supertype_id` now tests direct `u16` membership masks with SSE2 for at
least three candidates. Smaller masks and dictionary membership remain scalar.
The implementation and operations table are in `c62c35cb9`.

The cloud comparison used `squatter-benchmark` in
`mgsloan-compute/us-central1-a`, an `e2-standard-4` running on **AMD EPYC 7B12**.
Baseline `542b37ce3` and candidate `c62c35cb9` were built in the same isolated
checkout path with rustc 1.95.0, portable release settings, 16-slot groups, and
the same harness. Only the ELF interpreter path was patched for the cloud host.

The harness now records each grammar's supertype count. Selecting grammars with
1–8 supertypes from the existing corpora leaves 17 files per corpus across Bash,
C, Go, Python, TSX, and TypeScript. Tuning has 319,370 nodes and 11,488 matches;
holdout has 195,274 nodes and 30,345 matches. Each query uses the grammar's first
supertype. Grammars without supertypes are excluded; neither corpus contains a
dictionary-backed grammar.

Runs held the activity and benchmark locks and pinned measurement to CPU 1.
Each rate is the median of 14 samples from two processes, targeting 80 ms per
sample; binary and workload order reverse on the second pass. Scan construction
is timed; parsing, packing, and validation are excluded. Source/grammar hashes,
input descriptions, and match counts agree across variants. The harness also
checks each scan against scalar traversal before timing.

Rates are **million input nodes/s**:

| Operation | Tuning, before → after | Holdout, before → after |
| --- | ---: | ---: |
| Supertype nodes | 588.2 → 1,956.3 (+232.6%) | 486.4 → 1,285.9 (+164.4%) |
| Supertype count | 610.3 → 2,121.0 (+247.5%) | 598.1 → 1,906.7 (+218.8%) |
| Preorder nodes control | 1,358.5 → 1,368.4 (+0.7%) | 1,311.4 → 1,326.0 (+1.1%) |
| Preorder fold control | 1,756.8 → 1,762.4 (+0.3%) | 1,676.3 → 1,675.7 (−0.03%) |

Direct-supertype enumeration improves 2.64–3.33× and counting 3.19–3.48×, with
little change in traversal controls. These results cover full preorder scans,
not sparse filter combinations or dictionary performance. Unit tests cover all
eight membership bits and 16/32/64-lane inputs; integration tests cover direct
JSON and dictionary-backed C# membership, including postorder and reversal.

Artifacts are in `build/supertype-bench/`: the build script and logs, source and
binary hashes in `cloud/build.json`, and downloaded reports, manifests, scripts,
source snapshots, binaries, and `summary.json` in `supertype-simd-20260919/`.
Downloaded binary hashes and current scan/harness hashes match the build record.

## Two-sided range seeking and byte subtree pruning (2026-09-19)

Retained two changes to reduce visited groups:

- `f5a6e951f` seeks both start bounds for within, starting-in, and exact-start
  selections, in bytes or points and either preorder direction. The lower seek
  retains the crossing group for slot comparisons.
- `98d8ee8e6` adds a subtree rejection proof before ordinary group evaluation.
  The retained policy (`1f951f1df`) enables it for bytes. If all group ends are
  too early, its span base bounds a safe jump across whole descendant groups.
  No new index, allocation, or slab format is needed.

`573c2a205` specializes bound variants and point probes before binary search and
omits lower searches at coordinate zero. Queries without a useful subtree bound
use a group loop without pruning checks. Reverse counts can use forward pruning
over their remaining groups; reverse enumeration retains its group walk.
See [range-scans.md](range-scans.md) for boundary rules and the current pipeline.

### Assembly findings and rejected variants

An initial implementation read exact subtree spans after ordinary rejection. It
regressed midpoint overlap enumeration by 11–19% for bytes and 31–39% for points.
Testing the end bound first and using only nonzero span bases avoids delta reads
and short jumps on the common path. Zero bases cover groups whose spans all fit
in a byte.

Adding the second search changed inlining decisions. Point binary-search probes
began copying the whole group metadata before calling `start_minimum`. Explicit
inlining removes these copies and substantially improves short point queries.
The larger seek routine also needs explicit specialization of its constant bound
variants.

Point subtree pruning improved narrow overlap enumeration, but its additional
decoder path regressed broad enumeration and some counts. Splitting queries into
pruning/non-pruning loops and outlining the pruning kernel did not remove that
tradeoff. The retained code leaves point traversal flat after seeking. Two-sided
point seeking remains enabled.

Code placement still matters. Across intermediate binaries, plain preorder fold
rates moved by up to 28% even though its instructions, registers, and relative
branches were identical after address normalization. Its final control rates are
within 0.5%. Reverse overlap has an unresolved regression described below; its
consumer assembly also changes register allocation and inlining, so that result
cannot be explained solely by address placement.

### Cloud confirmation

Compared baseline `8810c4828` with retained scan source `1f951f1df` on
`squatter-benchmark`, `mgsloan-compute/us-central1-a`, `e2-standard-4`. This start
selected **Intel Xeon Broadwell**, model 79, at 2.20 GHz. Both binaries use the
same expanded harness (`a9f66edb8`), built in the same isolated worktree path with
rustc 1.95.0, portable release settings, and 16-slot groups. Only the ELF
interpreter path was patched for the cloud host.

Runs held the activity and benchmark locks and pinned timing to CPU 1. Each
corpus has 32 files across 11 languages: 747,560 tuning input nodes and 503,590
holdout nodes. Queries cover the midpoint 1%, with 9,589/5,196 overlap matches.
Exact-start queries use that interval's start. The harness validates membership
against scalar accessors; output counts agree across binaries for every workload.

Each rate is the median of 14 samples from two processes, targeting 80 ms/sample.
Binary and workload order reverse on the second pass. Scan construction is timed;
parsing, packing, and validation are excluded. Rates below are **million input
nodes/s**, including skipped nodes. Large exact-start rates mostly measure seeks,
not reading that many node records.

| Operation | Tuning, before → after | Holdout, before → after |
| --- | ---: | ---: |
| Byte overlap nodes | 5,339.1 → 9,770.7 (+83.0%) | 6,073.4 → 12,055.0 (+98.5%) |
| Byte overlap fold | 5,202.3 → 9,407.5 (+80.8%) | 5,947.8 → 11,693.5 (+96.6%) |
| Byte overlap count | 6,278.0 → 10,634.0 (+69.4%) | 7,104.2 → 14,016.2 (+97.3%) |
| Point overlap nodes | 4,376.6 → 4,401.6 (+0.6%) | 4,955.9 → 5,005.1 (+1.0%) |
| Point overlap count | 7,501.6 → 7,589.9 (+1.2%) | 8,338.1 → 8,249.7 (-1.1%) |
| Byte within nodes | 5,165.9 → 28,023.7 (5.43×) | 6,127.8 → 30,964.0 (5.05×) |
| Point within nodes | 2,742.0 → 23,362.9 (8.52×) | 3,648.4 → 23,847.7 (6.54×) |
| Byte starting-in nodes | 5,682.4 → 28,660.6 (5.04×) | 6,682.5 → 31,683.4 (4.74×) |
| Point starting-in nodes | 2,942.0 → 20,911.7 (7.11×) | 3,488.8 → 21,665.0 (6.21×) |
| Byte exact-start nodes | 7,692.9 → 193,337.4 (25.13×) | 8,905.2 → 131,652.8 (14.78×) |
| Point exact-start nodes | 3,911.1 → 105,860.0 (27.07×) | 4,551.9 → 68,987.2 (15.16×) |
| Reverse byte overlap nodes | 6,331.7 → 5,202.8 (-17.8%) | 7,333.7 → 6,023.3 (-17.9%) |
| Reverse point overlap nodes | 6,330.7 → 5,262.5 (-16.9%) | 7,196.0 → 6,013.3 (-16.4%) |
| Preorder nodes control | 581.7 → 580.2 (-0.3%) | 576.6 → 570.8 (-1.0%) |
| Preorder fold control | 1,126.7 → 1,131.9 (+0.5%) | 1,081.5 → 1,085.9 (+0.4%) |

Within/starting-in counts improve 5.8–16.3×, and exact-start counts 11.3–21.0×.
The unchanged scalar traversal control stays within 0.3%. The gains chiefly
benefit start-bounded scans and forward byte overlap. Reverse overlap is slower
in this build and remains a follow-up, rather than an assumed improvement.

Short checks use one process and three 50 ms samples per variant/corpus, with
baseline first. Enumeration changes are:

| Query | Byte overlap, tuning / holdout | Point overlap, tuning / holdout |
| --- | ---: | ---: |
| First 1% | +15.9% / +18.3% | +6.1% / +3.5% |
| Last 1% | +223.6% / +183.0% | +2.5% / +3.3% |
| Middle 50% | +5.6% / +7.3% | +0.1% / +2.3% |
| Whole source | +18.1% / +16.7% | +0.4% / +0.2% |

The middle 50% starts at 25% of source length. These checks are less conclusive
than the midpoint confirmation. Counts have additional tradeoffs: broad point
overlap counts regress 6–8%, and whole-source byte starting-in counts regress
20%. Reverse byte overlap regresses about 24–25% on broad queries. Avoid treating
the narrow-query gains as uniform improvements across consumers and selectivities.

Follow-up profiles use `perf record -e cpu-clock:u -F 997` with twelve 500 ms
samples per overlap workload, on the tuning corpus and CPU 1. Hardware PMU events
remain unavailable. No samples were lost. `Restricted::next_mask` accounts for
65% of sampled byte time and 71% of point time; consumer time is 14%/7%. Sampling
includes setup and validation, so these shares are not timed-kernel speedups.
Raw data and assembly annotations are retained.

The new integration test exercises larger nested subtrees, repeated starts,
coordinate-induced group waste, subtree clipping, both directions, and absent
or stored points. Scanning, pattern, and unit tests pass at group sizes 16/32/64;
the full default-size suite, including bindings and doctests, strict library/test
Clippy, and formatting checks pass.

Artifacts are in `build/range-seeking/`: source snapshots, build manifests,
comparison/profile scripts, extracted consumer assembly, and normalized control
comparisons. Its downloaded `range-seeking-20260919/` directory contains all
per-run reports, summaries, experimental binaries, and perf data. The retained
binary is `byte-pruning`; the main result is `confirm-byte-pruning-summary.json`.
Local/downloaded binary hashes match the build manifests and confirmation report;
the baseline parent, committed scan source, and shared harness hashes also agree.
The cloud instance was returned to its previous stopped state.

## Range scans with 16-, 32-, and 64-slot groups (2026-09-19)

Larger groups substantially improve narrow overlap scans and broad counts.
64 slots gives the strongest results for those workloads, but increases default
slab storage by 30–50%. 32 slots improves most measured scans while reducing slab
storage by 2–3%; its broad reverse byte overlap and byte-within enumeration are
slower than 16. The default remains 16.

### Method

Built `619a65964` three times in the same isolated worktree path with
`CFLAGS=-DSQ_GROUP_SIZE=16`, `32`, or `64`, using rustc 1.95.0 and portable release
settings. Scan implementation is unchanged from `1f951f1df`; the harness adds
representation identity and slot counts to its reports. Only the ELF interpreter
path was patched for the cloud host.

All runs used `squatter-benchmark`, `mgsloan-compute/us-central1-a`,
`e2-standard-4`, on the same Intel Xeon Broadwell CPU allocation, model 79 at
2.20 GHz. Timing was pinned to CPU 1 under the activity and benchmark locks.
Tuning and holdout each contain 32 files across 11 languages, totaling
747,560 and 503,590 nodes. Trees use default packing: points and symbol presence
enabled, without repacking away spare group capacity.

Five query windows cover the first, middle, and last 1% of source bytes,
the middle 50% starting at 25%, and the whole source. Point queries use the
corresponding source positions; exact-start queries use each window's start.
Each scenario has three processes per size/corpus, with size order rotating
16/32/64, 32/64/16, then 64/16/32. The second round reverses workload order;
the harness rotates workloads between samples.

Midpoint rates are medians of 15 samples targeting 60 ms each. Other windows use
nine samples targeting 40 ms. The harness caps repetitions at 10,000, so some
exact-start samples are shorter. Parsing, packing, and scalar validation are
outside timing; scan construction and consumption are included. Every process
validates membership and timed counts. All 90 reports agree on source/grammar
identities and cross-size output counts, and confirm the requested group size.

### Midpoint 1%

Rates are **million input nodes/s**, including skipped nodes. Each cell is
**tuning / holdout**. There are 9,589 / 5,196 overlap matches per iteration.
Compare sizes within this build; earlier sections use different harness binaries.

| Operation | 16 slots | 32 slots | 64 slots |
| --- | ---: | ---: | ---: |
| Byte overlap nodes | 9,799.5 / 12,244.4 | 14,423.0 / 16,407.0 | 18,407.7 / 19,166.7 |
| Byte overlap count | 14,177.7 / 17,227.2 | 23,744.9 / 25,316.1 | 33,869.8 / 31,614.9 |
| Point overlap nodes | 4,439.7 / 5,093.7 | 7,127.5 / 7,648.6 | 9,913.5 / 9,565.0 |
| Point overlap count | 7,713.2 / 8,483.5 | 13,068.8 / 12,985.9 | 18,759.1 / 16,424.6 |
| Reverse byte overlap nodes | 5,898.1 / 6,837.0 | 9,040.0 / 9,996.2 | 13,020.6 / 13,073.5 |
| Reverse point overlap nodes | 5,734.1 / 6,529.0 | 8,921.6 / 9,550.2 | 11,580.2 / 11,549.2 |
| Byte within nodes | 28,035.3 / 31,301.2 | 27,572.9 / 31,115.8 | 33,106.8 / 35,029.0 |
| Point within nodes | 23,563.2 / 24,594.4 | 26,384.9 / 27,042.7 | 27,944.4 / 27,809.6 |
| Byte starting-in nodes | 29,172.9 / 32,058.8 | 32,208.4 / 35,066.9 | 33,623.2 / 35,562.5 |
| Point starting-in nodes | 21,202.4 / 22,153.2 | 22,718.1 / 23,301.0 | 26,042.8 / 26,404.4 |
| Byte exact-start nodes | 200,841.0 / 138,398.1 | 206,166.2 / 140,883.3 | 209,109.2 / 138,723.2 |
| Point exact-start nodes | 105,149.0 / 72,074.2 | 108,822.2 / 75,206.8 | 111,978.8 / 75,317.2 |

Relative to 16 slots, 32 improves forward byte overlap enumeration by 34–47%
and point overlap by 50–61%. At 64 slots, those gains are 57–88% and 88–123%.
Within/starting-in counts improve 18–40% at 32 and 29–67% at 64. Exact-start
changes are much smaller because both start bounds already restrict the scan to
very few groups.

### Other query windows

Overlap enumeration throughput relative to 16 slots; each cell is tuning /
holdout. A value below 1 means slower.

| Window and coordinates | 32 / 16 | 64 / 16 |
| --- | ---: | ---: |
| First 1%, bytes | 1.09× / 1.05× | 1.13× / 1.05× |
| First 1%, points | 1.13× / 1.09× | 1.20× / 1.11× |
| Last 1%, bytes | 1.48× / 1.38× | 1.91× / 1.66× |
| Last 1%, points | 1.74× / 1.63× | 2.58× / 2.22× |
| Middle 50%, bytes | 1.14× / 1.13× | 1.22× / 1.16× |
| Middle 50%, points | 1.20× / 1.17× | 1.31× / 1.24× |
| Whole source, bytes | 1.09× / 1.07× | 1.05× / 1.06× |
| Whole source, points | 1.18× / 1.16× | 1.29× / 1.20× |

Counts benefit more than enumeration. Across both broad windows and corpora,
byte overlap counts improve 1.77–1.82× at 32 slots and 2.60–2.95× at 64;
point overlap counts improve 1.69–1.74× and 2.32–2.51× respectively.

There are repeatable regressions. With 32 slots, reverse byte overlap enumeration
is 7–16% slower at the first 1%, 21–22% slower at the middle 50%, and 25% slower
over the whole source. At 64 slots, whole-source reverse byte overlap is 4–8%
slower. Byte-within enumeration at 32 slots loses 5–8% on broad windows; 64 slots
improves it 14–20%. Larger groups are not a uniform win across consumers.

Midpoint forward overlap process medians vary by less than 3% within each
size/corpus. Some broad enumeration and exact-start workloads have substantially
more variation, reaching 28% between process medians. Treat small differences,
including the whole-source byte enumeration ranking, as inconclusive.

### Storage and interpretation

Each cell is tuning / holdout. Unused slots are padding inside populated groups.
Slab bytes include spare group capacity retained by default packing; they exclude
grammar metadata and are not compact-export sizes.

| Group size | Populated groups | Unused slots | Slab MiB |
| --- | ---: | ---: | ---: |
| 16 | 49,536 / 34,147 | 5.7% / 7.8% | 16.97 / 11.64 |
| 32 | 26,721 / 18,909 | 12.6% / 16.8% | 16.52 / 11.45 |
| 64 | 16,606 / 12,643 | 29.7% / 37.8% | 22.03 / 17.43 |

Larger groups amortize coordinate bounds, mask construction, and count operations
over more nodes. They keep the same SSE2 comparison width, using more chunks per
group. Fixed delta limits still close groups early, so increasing group size also
increases padding. The group count falls by less than the nominal size ratio.

The extracted forward overlap nodes/count consumers, reverse overlap consumers,
and plain preorder nodes/fold controls have identical normalized instructions and
identical entry addresses across all three binaries. Plain preorder nodes change
only 1–2%; fold improves 10% at 32 and 13–16% at 64. Scalar preorder stays within
0.6%. The observed range differences are not accompanied by the consumer code
placement changes seen in earlier experiments.

For these corpora, 32 offers a useful space/throughput compromise; 64 is more
attractive when narrow overlap or counts dominate and the storage increase is
acceptable. These measurements cover resident range scans with stored points,
not packing time, point-free slabs, random access, or general query performance.
No production configuration changed.

Artifacts are in `build/range-group-sizes/`: build/comparison/analysis/verification
scripts, source and binary hashes in `build.json`, assembly and
`codegen-comparison.txt`, and `analysis.txt`. The downloaded
`range-group-sizes-20260919/` directory contains all 90 reports and logs, the
three binaries, source snapshots, and `summary.json`. Downloaded hashes and
recomputed medians match the build and comparison records.

## Symbol filters and range/filter combinations by group size (2026-09-19)

For `filter_kind_ids`, 64 slots usually gives the highest throughput; 32 captures
much of the gain with slightly smaller slabs than 16. The range-only result above
understates the gains for broad enumeration when a symbol filter also removes
most output nodes. The default remains 16.

### Method and coverage

The scan implementation is unchanged. Harness `a4b8d18b9` adds range-plus-symbol
workloads and validates their exact node sequences against scalar accessors.
All three binaries use the same source/worktree path, rustc 1.95.0, portable
release settings, and `CFLAGS=-DSQ_GROUP_SIZE=16`, `32`, or `64`.

The cloud instance was restarted for this extension and again selected Broadwell
model 79. Runs use CPU 1, activity/benchmark locks, and the same tuning/holdout
corpora and default packing as the preceding comparison. Each corpus has 32 files
across 11 languages, with 747,560 / 503,590 nodes. Binary order rotates across
three processes per size/corpus, with workload order reversed on the second pass.

The 46 standalone workloads cover arrays and reusable sets of 1/2/4/8/16 symbol
IDs, using nodes, fold, and count consumers; postorder symbol filtering; fields,
supertypes, flags, a kind/field/flags combination, and traversal controls.
Each rate uses 15 samples targeting 60 ms. Combined-query runs cover 32
combinations plus eight range-only controls: byte/point overlap and within,
followed by one/four symbol IDs in an array or set, with nodes/count consumers.
They use nine samples targeting 40 ms, for midpoint 1% and middle 50% windows.

IDs are each file's most frequent named public kind IDs. Arrays repeat the most
frequent ID if a grammar has fewer than the requested number; sets deduplicate.
A one-ID selection matches 19.5% / 14.4% of nodes, four IDs 48.3% / 33.2%, and
sixteen IDs 62.4% / 54.5%. These are frequent-symbol measurements. Reusable set
construction, parsing, packing, and validation are outside timing; scan/filter
preparation and consumption are included. All rates use input nodes, including
skipped nodes, as the denominator.

### Symbol filters

Each cell is **tuning / holdout**. The 16-slot column is million input nodes/s;
other columns are throughput ratios against 16. Arrays call
`filter_kind_ids([ids...])`; sets call `filter_kind_ids(&kinds)`.

| Filter and consumer | 16 slots, M input nodes/s | 32 / 16 | 64 / 16 |
| --- | ---: | ---: | ---: |
| Array, 1 ID, nodes | 989.5 / 866.7 | 1.20× / 1.28× | 1.25× / 1.38× |
| Array, 1 ID, count | 1,483.7 / 1,352.8 | 1.53× / 1.55× | 1.77× / 1.69× |
| Set, 1 ID, nodes | 672.2 / 624.8 | 1.38× / 1.41× | 1.62× / 1.66× |
| Set, 1 ID, count | 1,376.4 / 1,277.0 | 1.46× / 1.46× | 1.77× / 1.66× |
| Array, 2 IDs, nodes | 554.3 / 520.5 | 1.29× / 1.35× | 1.41× / 1.51× |
| Array, 2 IDs, count | 1,129.0 / 1,062.0 | 1.41× / 1.39× | 1.58× / 1.47× |
| Set, 2 IDs, nodes | 446.6 / 425.3 | 1.38× / 1.43× | 1.65× / 1.70× |
| Set, 2 IDs, count | 775.8 / 740.4 | 1.60× / 1.57× | 1.96× / 1.82× |
| Array, 4 IDs, nodes | 548.5 / 546.8 | 1.11× / 1.19× | 1.23× / 1.34× |
| Array, 4 IDs, count | 1,142.5 / 1,092.4 | 1.40× / 1.40× | 1.56× / 1.43× |
| Set, 4 IDs, nodes | 324.1 / 321.2 | 1.32× / 1.38× | 1.52× / 1.59× |
| Set, 4 IDs, count | 616.9 / 594.9 | 1.43× / 1.43× | 1.64× / 1.54× |
| Array, 8 IDs, nodes | 394.1 / 366.7 | 1.15× / 1.24× | 1.27× / 1.38× |
| Array, 8 IDs, count | 998.2 / 965.4 | 1.25× / 1.25× | 1.30× / 1.20× |
| Set, 8 IDs, nodes | 197.1 / 193.7 | 1.11× / 1.14× | 1.17× / 1.22× |
| Set, 8 IDs, count | 283.3 / 280.7 | 1.09× / 1.09× | 1.13× / 1.12× |
| Array, 16 IDs, nodes | 253.6 / 240.2 | 1.28× / 1.32× | 1.41× / 1.44× |
| Array, 16 IDs, count | 449.6 / 441.2 | 1.35× / 1.34× | 1.53× / 1.41× |
| Set, 16 IDs, nodes | 194.9 / 188.8 | 1.10× / 1.12× | 1.17× / 1.20× |
| Set, 16 IDs, count | 282.2 / 277.7 | 1.10× / 1.09× | 1.15× / 1.13× |

For one-ID enumeration, 32 gains 20–28% with arrays and 38–41% with sets;
64 gains 25–38% and 62–66%. Counts benefit more. Fixed arrays are faster than
sets at every measured cardinality in these binaries. Large dynamic sets retain
per-candidate membership work, so their gains from fewer groups are smaller.
Fold results follow the same general ordering and are retained in the artifacts.

64 is not always faster than 32: eight-ID array count is 3.9% faster on tuning
but 3.6% slower on holdout. All standalone process medians vary by less than 5%
within a size/corpus; small differences do not establish a universal ranking.

### Range followed by symbol filtering

The narrow window is the midpoint 1%; the broad window starts at 25% and spans
50%. Each cell is again tuning / holdout, relative to the same combined query at
16 slots. Both the range and symbol predicates execute in the timed operation.

| Selection and filter, nodes | Narrow, 32 / 16 | Narrow, 64 / 16 | Broad, 32 / 16 | Broad, 64 / 16 |
| --- | ---: | ---: | ---: | ---: |
| Byte overlap, one-ID array | 1.70× / 1.53× | 2.41× / 1.90× | 1.54× / 1.51× | 1.86× / 1.85× |
| Byte overlap, four-ID set | 1.53× / 1.41× | 1.98× / 1.65× | 1.32× / 1.39× | 1.53× / 1.59× |
| Point overlap, one-ID array | 1.71× / 1.57× | 2.43× / 2.01× | 1.46× / 1.47× | 1.83× / 1.79× |
| Point overlap, four-ID set | 1.62× / 1.51× | 2.18× / 1.83× | 1.35× / 1.37× | 1.61× / 1.61× |
| Byte within, one-ID array | 1.38× / 1.27× | 1.56× / 1.35× | 1.43× / 1.45× | 1.74× / 1.74× |
| Byte within, four-ID set | 1.24× / 1.20× | 1.42× / 1.27× | 1.30× / 1.35× | 1.56× / 1.60× |
| Point within, one-ID array | 1.39× / 1.29× | 1.57× / 1.38× | 1.50× / 1.51× | 1.79× / 1.81× |
| Point within, four-ID set | 1.29× / 1.25× | 1.43× / 1.33× | 1.38× / 1.43× | 1.62× / 1.66× |

All measured combined enumeration cases improve over 16 slots at both larger
sizes. For broad byte overlap plus a one-ID array, 32 improves enumeration by
51–54% and 64 by 85–86%; their count gains are 68–70% and 116–140%.
For the corresponding narrow query, enumeration improves 53–70% and 90–141%,
and counts improve 54–72% and 97–160%.

The range-only broad byte-within control still regresses 6–7% at 32 slots,
while adding either measured symbol filter improves enumeration. The result
therefore depends on the complete pipeline and how many nodes it emits.
Some narrow within counts are effectively tied at 32 and 64. Combined-query
process medians vary by less than 7%, apart from one range-only control at 7.4%.

### Other filters and storage

| Filter and consumer | 32 / 16 | 64 / 16 |
| --- | ---: | ---: |
| One field, nodes | 1.25× / 1.24× | 1.36× / 1.31× |
| One field, count | 1.53× / 1.58× | 1.88× / 1.83× |
| Four-field array, nodes | 1.13× / 1.17× | 1.28× / 1.27× |
| Four-field set, nodes | 1.25× / 1.23× | 1.42× / 1.35× |
| Supertype, nodes | 1.69× / 1.54× | 2.39× / 2.00× |
| Supertype, count | 1.76× / 1.68× | 2.54× / 2.30× |
| Exclude extra/missing, count | 1.67× / 1.56× | 2.25× / 1.87× |
| One kind + field + flags, count | 1.41× / 1.50× | 1.78× / 1.79× |
| Postorder + one kind, nodes | 1.03× / 1.04× | 1.05× / 1.06× |
| Postorder + one kind, count | 1.41× / 1.42× | 1.79× / 1.69× |

Postorder enumeration gains much less than preorder filtering. Its count can
use the forward group walk, so it benefits similarly to preorder counts.
Supertype selects the grammar's first supertype, or an invalid ID when none
exists; the input-node denominator includes those early-rejected trees.

Storage exactly matches the preceding range comparison. Relative to 16 slots,
32 reduces default slab bytes by 2.7% / 1.6%; 64 increases them by 29.8% / 49.8%.
These totals include spare capacity and exclude grammar metadata. Plain preorder
nodes stay within 1.1%, scalar preorder within 0.3%, and preorder fold improves
10% at 32 and 13–17% at 64.

64 is the stronger throughput choice for these symbol/filter workloads when
that storage cost is acceptable. 32 offers a useful compromise. This does not
resolve the reverse byte-range regressions recorded above, and neither setting
has been selected as a new production default.

All 54 processes pass membership and timed-count validation. Downloaded reports
confirm group sizes, source/grammar identities, cross-size match counts, binary
hashes, and recomputed medians. Formatting and compilation pass; Clippy passes
with the existing `collapsible_if` warning in `crates/squatter-bench/src/lib.rs`
suppressed. No production scan code changed.

Artifacts are in `build/filter-group-sizes/`: build, comparison, analysis, and
verification scripts; source/binary hashes in `build.json`; and `analysis.txt`.
The downloaded `filter-group-sizes-20260919/` contains all 54 reports/logs,
source snapshots, binaries, and `summary.json`.
The cloud instance was returned to its previous stopped state.
