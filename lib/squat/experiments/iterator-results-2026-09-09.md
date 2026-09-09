# Preorder iterator and unpack windows — 2026-09-09

A 128-slot unpack window reduces cached attribute-walk time by about 1–2% on
bounded inputs and 1.8–2.4% on large files on the Broadwell VM. The gains are
modest; keep the 16-slot default and keep caching optional.

The iterator supports stackless preorder traversal with an optional unpack cache.
Independent unpack windows span 16, 32, 64, or 128 physical slots while preserving
16-slot slab groups. The default remains 16 slots. Window slots include group
padding; a partial final window stops at the last live group.

## Independent unpack windows: slab groups stay at 16

These ratios compare **cached attribute-walk time with the 16-slot cached
variant**, using five repeats. The source, compiler flags apart from window size,
input bytes, slab sizes, and group counts match. Values below 1 favor the wider
window. Allocation sizes include the iterator itself; a plain iterator is 48 B.

| Unpack slots | Cached allocation | Bounded original | Bounded mutated | >1 MiB original | >1 MiB mutated |
|---:|---:|---:|---:|---:|---:|
| 16 | 160 B | 1.000 | 1.000 | 1.000 | 1.000 |
| 32 | 256 B | 0.994 | 0.997 | 0.996 | 0.989 |
| 64 | 448 B | 0.993 | 1.000 | 0.987 | 0.993 |
| 128 | 832 B | 0.984 | 0.992 | 0.982 | 0.976 |

The 128-slot window saves roughly 1–2% on bounded inputs and 1.8–2.4% on large
inputs relative to 16-slot unpacking. The large-file mainline controls for that
comparison were 0.991/0.994, so part of the small raw improvement may be run drift.
32 and 64 slots give smaller gains. **Keep 16 as the default**: the larger windows
are useful experimental options, but do not establish a universal improvement
large enough to require more memory for every cached iterator.

For large files, cached/plain attribute ratios are 0.969/0.972 for 16 slots
and 0.956/0.952 for 128 slots (original/mutated). These compare complete checked
walks, not just field extraction.

## Full-cache endpoint test

A wider full cache helps relative to the full cache's own 16-slot baseline:

| Full cache: 128 slots / 16 slots | Original | Mutated |
|---|---:|---:|
| Bounded | 0.977 | 0.961 |
| Files over 1 MiB | 0.971 | 0.964 |

However, full-cache 128-slot walks are essentially tied with ID-only 128-slot
walks: full/ID ratios are 1.004/0.987 on bounded originals/mutations and
1.006/1.006 on the large files. The current full cache allocates 584 B at 16 slots
and 3,944 B at 128, versus 160 B and 832 B for ID-only caching. Retain the full
cache as an experiment rather than replacing the smaller ID cache.

## Optional cache and traversal comparisons

The twelve-repeat, rotated-order bounded run gives:

| Comparison | Original ratio | Mutated ratio | Mainline control, original / mutated |
|---|---:|---:|---:|
| Plain iterator navigation / cursor | 0.734 | 0.716 | 0.889 / 0.889 |
| Plain iterator attributes / cursor | 0.996 | 0.995 | 1.010 / 1.012 |
| 16-slot cached attributes / plain iterator | 0.945 | 0.946 | 1.003 / 1.005 |

The navigation control discrepancy remains even after balancing workload order;
it limits attribution of the raw 26–28% reduction. In the initial large-file
matrix, plain iterator navigation took 6–8% less time with controls near 1.0.
Those large navigation measurements precede independent-window support; the
window matrix above measures the newer attribute paths. The ordinary and cached
iterators both remain available.

## Local Intel Ultra 7 check

The same window binaries also ran on an Intel Core Ultra 7 165U, pinned to
performance-core CPU 2, using the same bounded inputs and five repeats. Cached
attribute time divided by plain iterator attribute time was:

| Unpack slots | Original | Mutated |
|---:|---:|---:|
| 16 | 1.011 | 1.007 |
| 32 | 0.996 | 1.004 |
| 64 | 0.997 | 0.989 |
| 128 | 0.995 | 0.988 |

These are roughly ties, unlike Broadwell's clearer cache benefit. Local
cross-build mainline controls moved by as much as 7%, so the larger raw
cross-build speedups in that artifact should not be attributed to window size.
Keeping caching optional is supported by this second CPU check.

## Measurement method

The primary machine is Google Cloud `squatter-benchmark`, project
`mgsloan-compute`, zone `us-central1-a`: `e2-standard-2`, 8 GiB RAM, Intel Broadwell
Xeon reporting 2.20 GHz. Its two vCPUs are the two hardware threads of one
exposed core.
Each benchmark is pinned to CPU 0 and variants run sequentially. CPU steal time
was about 0.02% in the initial large-file runs. Hardware counters were unavailable
(permission denied); elapsed and process CPU times are retained.

Existing binaries were uploaded, as requested. Host builds use GCC 15.3.0 and
Rust 1.95.0, Cargo release optimization, and the variant CFLAGS recorded in the
artifacts. The guest loader runs them against Ubuntu 24.04 libraries. No remote
build or corpus download is needed. The machine has no external address or
service account; SSH uses the existing IAP access. A 12-hour maximum runtime
stops the VM automatically while preserving its disk and uploaded bundle.

The bounded sample contains 88 training/holdout files across eleven grammars
(171,306 original nodes; 167,642 mutated). The mixed sample contains 53 files;
nine files over 1 MiB account for 4,665,634 original nodes and 4,352,863 mutated
nodes. Large-only tables use original byte size for membership in both states.
These are saved corpus bytes, grammar libraries, and deterministic seed-42
mutations, with hashes checked before comparing measurements.

Ratios are ratios of separate per-file timing medians, followed by an equal-file
median. Lower is faster: 0.95 means 5% less elapsed time. The raw artifacts also
preserve CPU timings, quantiles, summed timings, and unchanged mainline controls.
Mainline here means the vendored upstream Tree-sitter runtime, not the packed
engine in `../main`.

Walks record every node's supported attributes, field, identity, and depth.
Iterator depth is recovered from descendant counts to preserve the existing
walk contract. Navigation records every identity without unpacking attributes.
Identity-map construction and correctness comparisons are outside the timers;
iterator allocation/destruction and result recording are timed. Ordinary yielded
nodes remain independent tree-borrowing handles: use iterator attribute access
to benefit from its unpack cache.

## Initial experiments

The initial matrix used nine repeats and fixed workload order. Its small-file
navigation result had a control discrepancy: unchanged mainline traversal was
11–12% faster under the iterator benchmark labels. The harness now rotates
workloads after each pair of backend orders and by batch; a separate twelve-repeat
run balances all six traversal workloads. That control discrepancy persists,
so workload order alone does not explain it. Treat the raw small-file navigation
ratios as harness/context measurements, not an isolated API speedup. Large-file
controls were near 1.0. Cached navigation never unpacks columns; its small timing
differences likewise should not be attributed to an unpack-cache optimization.

The initial full cache gave little large-tree benefit: cached/plain attribute
ratios were 0.996 original and 1.000 mutated, versus 0.978/0.977 for ID-only caching.
Batching the ID cache-hit checks improved those ratios to 0.968/0.961. This is why
ID-only caching remains the ordinary implementation. A separate endpoint test
checks whether a wider full cache changes that conclusion.

Changing slab groups to 32 or 64 slots did not clearly improve bounded cached
walk times. Allocated slab sizes were 21.65, 21.38, and 31.59 bytes/node for
16/32/64 groups respectively. These are unrepacked bounded inputs, not a claim
about every corpus. Independent unpack windows avoid this layout tradeoff.

## Explicit unpack kernels

The kernels preserve the format's unused high bits at the end of each u64.
Portable SWAR spreads four fields into u16 lanes; BMI2 uses PDEP; AVX2 broadcasts
a word, shifts four 64-bit lanes, masks them, and compacts the results with byte
shuffles. Whole-byte columns use explicit SSE2 widening (8 bits) or native u16
copying (16 bits). Automatic non-byte unpacking selects BMI2 on supported Intel
CPUs and SWAR elsewhere; hardware-specific variants remain guarded experiments.

Median nanoseconds per 9-bit value on Broadwell (nine rotated microbenchmark
repeats, all outputs checked against scalar reads):

| Unpack slots | Scalar | SWAR | BMI2 | AVX2 |
|---:|---:|---:|---:|---:|
| 16 | 3.912 | 2.433 | 1.917 | 2.195 |
| 32 | 3.647 | 1.748 | 1.238 | 1.597 |
| 64 | 3.523 | 1.444 | 0.943 | 1.336 |
| 128 | 3.530 | 1.300 | 0.802 | 1.225 |

The first three microbenchmarks used matching compile-time group/window sizes,
but the unpack kernel itself takes a slot count and does not traverse groups.
Wider real-iterator windows are measured separately with every slab group fixed
at 16. Faster decoding alone does not imply an equally large whole-walk gain.

## Validation

Unit tests cover widths 1–16, all starting positions within a packed word, every
count through the configured window size, dirty tail bits, output sentinels, and
final allocated-word boundaries. Traversal checks cover complete trees, interior
subtrees, empty nodes, repeated/lazy attribute access, multiple windows, partial
tails, permanent exhaustion, and nodes retained after iterator destruction.

C checks passed at 16/32/64 slab groups and 16/32/64/128 unpack windows. JSON,
TypeScript, CSS, and YAML comparisons passed, including error recovery trees.
ASan/UBSan passed 32- and 128-slot windows and a 128-slot full cache. Rust tests,
doctests, and strict Clippy passed.

The broad bounded matrix includes the existing cold-parse relationship checks
and expected-field policy. Large timing runs select traversal workloads, which
still compare every visited node and attribute; prerequisite parse timings are
recorded without repeating the unrelated relationship checks. One exploratory
large run with those repeated checks was intentionally interrupted and archived;
it is excluded from completed measurement summaries. No seek or field policy
was relaxed by this work.

## Artifacts and reproduction

All 48 cloud benchmark runs and eight local runs completed with zero comparison
failures. The artifacts retain every per-file result, input/binary/source hashes,
compiler flags, grammar identities, commands, controls, and machine details:

- [Independent ID windows](iterator-window-results-2026-09-09.json)
- [Full-cache endpoints](iterator-full-window-results-2026-09-09.json)
- [Balanced traversal run](iterator-balanced-results-2026-09-09.json)
- [Initial kernel, group-size, and cache matrix](iterator-matrix-results-2026-09-09.json)
- [Local pinned-core check](iterator-local-window-results-2026-09-09.json)
- [Explicit-mask code-generation comparison](iterator-codegen-2026-09-09.json)

The final source spells out power-of-two lane masks instead of modulo. GCC 15.3
at `-O3` produces identical instruction bytes and relocation targets for all six
measured window/full-cache configurations. The machine-code check links the
final spelling to the measured source; no timing change is claimed for it.

Immutable source snapshots, uploaded binaries, and raw logs remain under
`build/squat-iterator/`. The final release executable is also built locally.
The VM retains `~/squatter-benchmark` and its staged grammars/corpus; reuse its
existing binaries without rebuilding:

```sh
gcloud compute ssh mgsloan@squatter-benchmark --project=mgsloan-compute \
  --zone=us-central1-a --tunnel-through-iap
python3 benchmark-upload.py ~/squatter-benchmark --output-name windows-repeat \
  --repeat 5 --variants window16 window32 window64 window128 \
  --benchmarks walk-iterator walk-iterator-cached --unpack-sizes 128
```

Use a new output name. See [the runner documentation](../../../tools/squatter/README.md)
for building variants, downloading results, and validation with
`summarize-iterators.py`. For a local build of the wider ID cache, use
`CFLAGS="-DSQ_GROUP_SIZE=16 -DSQ_ITERATOR_UNPACK_SLOTS=128" cargo build --release -p squatter-bench`.
The window setting affects only iterator cache storage and decoding, not the slab
format or query engine.
