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
