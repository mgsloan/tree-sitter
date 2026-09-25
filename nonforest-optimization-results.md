# Portable optimizations on main

Baseline: clean `main` at `3f957fb68c2dfab71f32a38ef9aaf9e599132b97`.
This comparison uses the non-forest implementation on both sides.

Three changes are retained:

- Inline `Node::next_preorder`. The pre-forest node already passes in registers;
  assembly confirms that inlining removes the external caller's per-node call.
- Copy sidecars into reserved storage before setting the vector length. The copy
  initializes every word, eliminating a redundant zero-fill with unchanged
  allocation size and contents.
- Build point data from known-live slots. Resolve the root once, then construct
  nodes directly inside the existing group/slot bounds. Source-coordinate and
  cancellation checks remain.

A cursor rewrite that updates only the slot was also tested. Its timing changed
sign across linker layouts, so it was discarded. The original compiler output
already used the same sibling-update instructions; the child path's saved store
was insufficient to establish an overall improvement. Repacking already wrote
directly into its final allocation and needed no corresponding change.

## Measurements

GCP `corpus-builder`, `e2-standard-16`, Intel Xeon 2.20 GHz, CPU 2, one boot,
no competing benchmark jobs. The host CPU differs from the earlier EPYC forest
measurements. Fresh baseline/candidate builds use the same compiler,
`-C target-cpu=x86-64-v3`, clang/LLD, and matched section-shuffle seeds 101–105.
Baseline/candidate order alternates by seed.

264 files, 11 grammars. Core comparisons use five repeats; scan benchmarks use
five samples targeting 15 ms; lifecycle benchmarks use three samples targeting
5 ms per file. Per-file comparisons sum medians over the same corpus; scan
comparisons use aggregate corpus iterations. Report the median paired change
and its full range across five seeds. Negative means less elapsed time.

Each change was first tested independently. A combined exploratory candidate
also ran all 267 scan workloads and broader core/lifecycle controls. The final
combination excludes the cursor rewrite and is measured separately against the
unchanged baseline. Differences changing sign or smaller than their seed range
are treated as inconclusive.

Independent changes against the same baseline:

| Trial / workload | Median | Five-seed range | Decision |
|---|---:|---:|---|
| Preorder inlining: scalar traversal | -62.09% | -62.93% to -47.51% | keep |
| Sidecar copy: repack | -30.81% | -31.26% to -29.94% | keep |
| Known-live slots: point construction | -2.85% | -3.16% to -2.52% | keep |
| Cursor slot rewrite: traversal | -4.58% | -5.69% to +0.33% | discard |
| Cursor slot rewrite: bulk attributes | -2.28% | -3.19% to +0.99% | discard |

Final three-change combination against unmodified `main`:

| Operation | Median | Five-seed range |
|---|---:|---:|
| Scalar preorder | -62.09% | -62.80% to -48.53% |
| Scalar field filter | -29.83% | -35.44% to -22.85% |
| Scalar kind filter | -10.56% | -15.93% to -9.42% |
| Scalar byte-range filter | -12.23% | -24.90% to -1.10% |
| Scalar point-range filter | -16.46% | -18.09% to -12.88% |
| Preorder nodes | -2.41% | -4.08% to +3.56% |
| Preorder count | -0.49% | -1.99% to +0.99% |
| Postorder nodes | -2.44% | -20.61% to +2.43% |
| Point-range count | +2.07% | -6.65% to +5.99% |
| Cursor traversal | +0.04% | -1.66% to +1.20% |
| Bulk cursor attributes | +1.14% | -1.96% to +1.90% |
| Byte seek | -1.05% | -4.26% to +0.64% |
| Point seek | 0.00% | -1.25% to +1.02% |
| Pack with reused context | -4.61% | -5.93% to +6.75% |
| Build point sidecar | -3.00% | -3.32% to -2.85% |
| Read point endpoints | +1.34% | -19.07% to +8.41% |
| Load copied tree | +3.19% | -7.04% to +5.55% |
| Repack | -28.56% | -30.66% to -27.67% |

The three intended gains survive the combined run. Scalar field, kind, and
point-range filters also improve. The byte-range filter changes less than its
seed spread, and the navigation, iterator, packing, loading, and point-read
controls remain inconclusive.

## Checks and artifacts

The complete debug suite passes (121 tests). Targeted release navigation,
storage, persistence validation, and compact-copy tests pass (17 tests). After
the point-construction change, navigation/storage tests were rerun in debug and
release (9 tests each), including short sources, cancellation, backing storage,
and derived point positions. Serialization and loader validation are unchanged.

All 80 isolated/exploratory timing jobs passed their corpus checks. The
exploratory combined build covered all 267 scan workloads across five seeds.
The 30 final-combination jobs also pass: all core comparisons succeed, scan
output counts agree, and lifecycle workloads complete without errors. Final
timing jobs took 342 seconds; the earlier isolated/exploratory matrix took
1000 seconds. The benchmark VM was stopped after result collection.

Frozen sources, build commands and hashes, per-file results, logs, and assembly
are under `build/nonforest-opt-20260924/`. The `retained/` snapshot and
`final-bin/` binaries contain the final three changes. Final paired results are
under `build/nonforest-opt-final-20260924/`.
