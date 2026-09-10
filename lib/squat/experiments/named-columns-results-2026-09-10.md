# Named columns after version 4

Named fields reduce isolated compact-packing time by **21–31%** relative to the
array-based version-4 implementation. Cold parse takes roughly **7–14% less
time** across the reported cohorts. The runtime layout, packer, iterator cache,
and bulk-equality API now use named fields. Fixed-width flags, u8/u16 deltas, and
u32 bases have direct typed access;
packed symbol/field IDs retain their explicit bit widths. SWAR equality and SIMD
widening to u32 with broadcast-base arithmetic remain in place.

This follows storage commit `d766e8015`, which was benchmarked and committed
before the cleanup. The persisted version-4 bytes and runtime descriptor sizes
are unchanged. An independent public-API probe produced 88 byte-identical
snapshots across eleven grammars and both point modes, including forced growth
and compact packing; copied and borrowed loaders accept every reference slab.

## Cloud timings

The baseline is `d766e8015`; the candidate is the named-field implementation in
the commit containing this report. Both release binaries were built locally and
uploaded. Timings ran sequentially on CPU 0 of the two-vCPU `e2-standard-2` VM
`mgsloan-compute/us-central1-a/squatter-benchmark` (8 GiB, Intel Broadwell,
Ubuntu glibc 2.39). These vCPUs expose two hardware threads of one core.
GCC 15.3.0 and Rust 1.95.0 use the same Cargo release defaults.

Both variants use 16-slot groups and 16-slot absolute-u32 unpack windows.
The bounded sample contains 88 files across eleven grammars; the mixed sample
contains 53 files, with nine originals at least 1 MiB. Mutations use seed 42.
Each file has eight timing repeats, with rotating backend/workload order.
Mainline controls use the vendored Tree-sitter runtime, not `../main`.
No delta-cache baseline is timed.

Ratios below are **named/v4**, lower is faster: divide separate per-file timing
medians, then take an equal-file median. Large-file mutation membership uses the
original input size. Cold parse is also recorded during large-run setup, although
large runs select only the three attribute-walk workloads. Complete walks include
attribute collection and output recording.

Bounded corpus:

| Workload | Points original | Points mutated | Byte-only original | Byte-only mutated |
|---|---:|---:|---:|---:|
| cold-parse | 0.870 | 0.899 | 0.926 | 0.933 |
| cursor-forward | 0.991 | 0.992 | 0.980 | 0.992 |
| iterator-forward | 0.977 | 0.981 | 0.994 | 0.993 |
| walk-forward | 0.994 | 0.994 | 1.007 | 0.998 |
| walk-iterator | 0.986 | 0.991 | 0.994 | 1.003 |
| walk-iterator-cached | 0.965 | 0.967 | 0.968 | 0.979 |
| query-matches | 1.001 | 1.000 | 0.990 | 0.994 |
| query-captures | 1.000 | 1.003 | 0.993 | 0.994 |

Nine originals at least 1 MiB, and their mutations:

| Workload | Points original | Points mutated | Byte-only original | Byte-only mutated |
|---|---:|---:|---:|---:|
| cold-parse | 0.869 | 0.864 | 0.911 | 0.909 |
| walk-forward | 0.987 | 0.973 | 1.001 | 0.994 |
| walk-iterator | 0.990 | 0.981 | 0.997 | 0.993 |
| walk-iterator-cached | 0.971 | 0.978 | 0.990 | 0.985 |

Cold-parse gains are larger than the accompanying control changes, and appear
in every bounded-corpus grammar median for original inputs. Ordinary walks and
queries remain close to the baseline. Bounded cached walks take 2–3.5% less time;
some of the point-enabled difference overlaps control drift.

Do not interpret every raw large-file walk gain as an implementation gain.
Point-enabled mutated walk controls improve 3.7–4.8%, exceeding the candidate's
1.9–2.7% raw gains. Original point-enabled controls improve about 1%; original
byte-only controls are nearly stable. The small traversal differences are mixed
once these controls are considered. Full per-file controls and quantiles are
retained in the timing artifact.

Mainline controls on those nine large inputs (same ratio convention):

| Workload | Points original | Points mutated | Byte-only original | Byte-only mutated |
|---|---:|---:|---:|---:|
| cold-parse | 0.994 | 0.984 | 1.012 | 1.004 |
| walk-forward | 0.991 | 0.952 | 1.001 | 0.990 |
| walk-iterator | 0.990 | 0.963 | 1.003 | 0.995 |
| walk-iterator-cached | 0.990 | 0.957 | 1.006 | 0.987 |

Cached/uncached iterator attribute walks within the named-field build:

| Sample | Points original | Points mutated | Byte-only original | Byte-only mutated |
|---|---:|---:|---:|---:|
| 88 bounded | 0.936 | 0.929 | 0.915 | 0.914 |
| Nine ≥1 MiB | 0.977 | 0.981 | 0.957 | 0.969 |

Within-build cached/uncached mainline controls are within 1% on bounded inputs
and 0.3% on the large subset. Both iterator types remain available. Runtime descriptors still occupy 136/104 B
with/without points on x86-64. Uncached iterators occupy 48 B; cached iterators at
16/32/64/128 slots remain 552/1,032/1,992/3,912 B with points and
296/520/968/1,864 B without points. This cleanup adds no retained tree or cache
storage; see the [storage report](storage-v4-results-2026-09-10.md) for allocation
measurements against mainline and version 3.

## Isolated compact packing

Compact packing takes 28–31% less time with points and 21–23% less time without
points. Every tested file improves in this separate probe. The cleanup replaces
runtime column dispatch and general packed-word writes with named extrema and
explicit bit/u8/u16/u32 stores. These changes were measured together; this run
does not isolate the contribution of each individual source change.

| Mode | 11 bounded grammar representatives | Nine ≥1 MiB files |
|---|---:|---:|
| points | 0.694 | 0.722 |
| bytes | 0.768 | 0.791 |

The existing C layout probe measures `sq_tree_pack(..., repack=true)` on a
pre-parsed tree, excluding parsing and deletion. It uses seven repetitions,
GCC `-O2 -g`, and alternating per-file variant order on the same cloud CPU.
All 80 processes pass; all non-timing metadata, including slab bytes, agrees
exactly. The reused v4 packing probes have the same timed code as `d766e8015`;
they precede its loader-only validation change, which this probe does not call.
These are compact construction timings, not the public `sq_tree_repack` API.

## Compatibility and validation

The cleanup removes `SQColumn` and `sq_tree_group_equal` from the C API. Each
previously exposed encoded column has a named equality function instead, such
as `sq_tree_group_field_equal`. Returned physical-lane masks and encoded-value
semantics are unchanged. Rust did not expose the removed selector. Point equality
functions, like point getters, are absent when points are disabled.

Six C configurations pass unit, traversal/persistence, and query suites. They
cover points on/off, ASan/UBSan with 128-slot absolute caches in both modes,
32/64-slot groups, and 64-byte column alignment. Historical cache modes remain
in correctness checks. Native-word reference tests verify fixed-width reads and
writes, and distinct patterns exercise every named column through repeated
resize/compaction. Big-endian lane handling is explicit in the code, but these
runs execute on x86-64 and do not independently validate a big-endian host.

Rust tests/doctests, strict Clippy, and formatting pass in both feature modes.
The default point-enabled release binary is restored. Cloud runs pass all 16
operations, covering 1,128 file/build cases with eight repeats each. Node counts,
group counts/capacities, expected field differences, and slab sizes match the
baseline. The existing seek/field mismatch policy is unchanged.

The retired column-addressing experiment depended on deleted column selectors
and obsolete index-bias variants. Its source remains in `98967f593`; its
[historical results](column-addressing-results-2026-09-10.md) are preserved.

## Artifacts and reproduction

- [Traversal/query data](named-columns-results-2026-09-10.json): per-file timings,
  quantiles, CPU timings, mainline controls, cache comparisons, and binary,
  grammar, and source identities.
- [Packing data](named-columns-packing-2026-09-10.json): every probe command,
  input/binary hash, raw measurement, and summary.
- [Validation](named-columns-validation-2026-09-10.json): configurations, source
  and log hashes, 88 snapshot identities, and compiled allocation sizes.

Raw cloud output is saved under `build/squat-named/cloud-results/`. Its `run.sh`
records the full sequential suite. Upload the two builds' binaries and reuse the
existing corpus/grammar bundle, then invoke `benchmark-upload.py` with variants
`v4-points named-points v4-bytes named-bytes` (reversed order for large inputs),
`--repeat 8`, and the workload selectors recorded in each run manifest.
The idle wrapper covers the complete suite.

Validate and summarize the downloaded traversal/query runs with:

```sh
python3 tools/squatter/summarize-storage.py build/squat-named/cloud-results \
  --build-manifest build/squat-named/cloud-build-manifest.json \
  --input-manifest build/squat-iterator/absolute-build-manifest.json \
  --previous v4 --current named --slab-delta 0 \
  --output /tmp/named-columns-results.json
```

Build `lib/squat/tests/slab-compatibility.c` against each revision's static library
and Tree-sitter runtime (the Makefile has a `slab-compatibility` target). Invoke
both with the same `GRAMMAR_LIBRARY GRAMMAR_SYMBOL SOURCE OUTPUT_PREFIX`; give
the candidate a final `REFERENCE_PREFIX` argument to require matching bytes and
exercise both loaders. The validation artifact identifies all eleven inputs.

The enabled cloud idle timer stops the VM after 30 minutes without an active
wrapped job, within its one-minute check interval. This policy was observed
requesting poweroff after 1,812 idle seconds during the preceding storage run
and after 1,811 idle seconds following this complete suite. The VM was restarted
to retrieve its saved results. The timer remains enabled after reboot; no source
builds run on the VM. See the
[operational instructions](../../../tools/squatter/README.md#cloud-idle-shutdown).
