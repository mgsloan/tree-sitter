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
