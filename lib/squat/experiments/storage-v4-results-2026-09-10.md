# Version 4: compact header, colocated runtime, reverse preorder

Compact packing takes 18–21% less time on the cloud VM. Version 4 persists a
16-byte header and fills node columns in reverse preorder.
Newly packed and compacted trees retain one allocation instead of three. The
runtime descriptor and supertype list immediately precede persisted bytes;
loading borrowed or copied external bytes uses a separate runtime allocation.

The physical slot is now the direct column index. Preorder descends through
these slots and skips trailing group waste. Growth and compaction preserve slot
indexes and copy used packed words without shifting ID lanes. Ordered query
plans convert their ascending positions to physical slots at the scan boundary.

## Timing comparison

The benchmark compares version 3 at `98967f593` with version 4, with points both
enabled and disabled. Both use 16-slot groups, 16-slot absolute-u32 unpack
windows, and the same Cargo release defaults (GCC 15.3.0, Rust 1.95.0). The old
binaries were rebuilt from an isolated archive of that revision. Binaries were
uploaded to the existing Google Cloud VM; nothing was compiled there.

The VM is `mgsloan-compute/us-central1-a/squatter-benchmark`, `e2-standard-2`,
8 GiB RAM, Intel Broadwell, Ubuntu glibc 2.39. The two vCPUs expose two hardware
threads of one core. Jobs run sequentially on CPU 0. Each file has eight timing
repeats; backend and workload order rotate within a run. Mutations and originals
are separate. Mainline is the vendored upstream Tree-sitter runtime, not the
packed implementation in `../main`.

The bounded sample has 88 files across eleven grammars. The mixed sample has
53 files; the large-file tables isolate the nine at least 1 MiB in original size.
Bounded runs measure queries, cursor/iterator walks, navigation, and cold parse;
large runs measure cursor walks and both iterator attribute walks. No delta-cache
baseline is timed. Cross-build ratios use separate per-file medians, followed by
an equal-file median. Mainline controls expose run drift; complete walk timings
include attribute collection and output recording, not just column access.

Bounded-corpus elapsed-time ratios, v4/v3 (lower is faster):

| Workload | Points original | Points mutated | Byte-only original | Byte-only mutated |
|---|---:|---:|---:|---:|
| cold-parse | 1.001 | 1.006 | 0.991 | 0.992 |
| cursor-forward | 0.972 | 0.987 | 0.988 | 0.984 |
| iterator-forward | 1.003 | 1.006 | 1.005 | 0.990 |
| walk-forward | 0.993 | 0.995 | 0.982 | 0.974 |
| walk-iterator | 0.990 | 1.001 | 0.985 | 0.981 |
| walk-iterator-cached | 0.994 | 0.994 | 0.985 | 0.982 |
| query-matches | 1.014 | 1.005 | 1.030 | 1.020 |
| query-captures | 1.003 | 1.000 | 1.022 | 1.015 |

Large-file elapsed-time ratios, v4/v3 (nine originals ≥1 MiB, also used for mutation membership):

| Workload | Points original | Points mutated | Byte-only original | Byte-only mutated |
|---|---:|---:|---:|---:|
| walk-forward | 0.970 | 0.985 | 0.981 | 0.990 |
| walk-iterator | 0.971 | 0.990 | 0.983 | 0.992 |
| walk-iterator-cached | 0.979 | 0.991 | 0.992 | 0.995 |

Mainline controls for the original point-enabled large walks are 0.977, so
most of their apparent 2–3% improvement may be run drift. Other walk controls
are generally within about 1%. Bounded byte-only query times increase about
1.5–3% with stable controls. This is a storage simplification with modest walk
changes, not an across-the-board query improvement.

Cached/uncached iterator attribute-walk ratios within v4:

| Sample | Points original | Points mutated | Byte-only original | Byte-only mutated |
|---|---:|---:|---:|---:|
| 88 bounded | 0.949 | 0.949 | 0.932 | 0.936 |
| Nine ≥1 MiB | 0.995 | 0.989 | 0.978 | 0.983 |

Both iterator types remain available. The [validated per-file timing artifact](storage-v4-results-2026-09-10.json)
preserves quantiles, summed timings, CPU timings, cache comparisons, controls,
input and grammar hashes, binary hashes, and run manifests. All 16 final cloud
operations passed: 1,128 file/build cases, with eight repeats each.

## Compact packing

Packing a pre-parsed tree with `repack=true` takes **18–21% less time** in the
separate layout probe. This exposes the benefit of copying packed word prefixes
during compaction; cold parse includes the unchanged parser and obscures this gain.

| Mode | 11 bounded grammar representatives | Nine ≥1 MiB files |
|---|---:|---:|
| Points, v4/v3 | 0.790 | 0.814 |
| Byte-only, v4/v3 | 0.796 | 0.820 |

The existing C layout probe uses seven repetitions, GCC 15.3.0 `-O2 -g`, and
alternating per-file variant order on the same VM/CPU. Parsing and destruction
are outside this timer. All 80 probe processes passed; node/group/column
statistics agree and slabs shrink by exactly 16 bytes. These are compact
construction timings, not the public `sq_tree_repack` API.
[Packing measurements and commands](storage-v4-packing-2026-09-10.json) retain every file.

The initial bounded pilot showed a 7–10% attribute-walk regression with mostly
stable controls. It exposed repeated slot validation in the new first-child
path: the slot had already been normalized, but went through public preorder and
node-at-slot checks again. Removing that duplication restored point-enabled
walks to approximately v3 speed and improved byte-only walks about 2%. The final
comparison uses the corrected binaries. The [superseded pilot summary](storage-v4-pilot-2026-09-10.json)
is retained; its interrupted large-file run is excluded from results.

## Allocation measurements

With identical groups and capacities, every default-alignment slab is exactly
16 bytes smaller than version 3. Runtime-prefix alignment uses up to six of those
saved bytes: retained requested allocation sizes decrease by 10–16 bytes per
tree. The substantial structural improvement is reducing three retained
allocations to one. The runtime descriptor still uses 136 bytes with points and
104 bytes without on x86-64. The copying loader uses two allocations; the
borrowed loader owns one runtime allocation and leaves the payload with its caller.

Borrowed loading also avoids temporary column copies. The presence validator
checks occurrences and group membership in place, counts each symbol's distinct
groups, and uses bitmap popcounts to reject extra bits. Its scratch space scales
with grammar symbols and tree depth, rather than allocated slab capacity.
This loader-only improvement was made after freezing the benchmark binaries;
none of the timed packing, traversal, or query paths calls the byte loader.

Summed retained requested MiB for original inputs:

| Representation | 88 bounded files | Nine files ≥1 MiB |
|---|---:|---:|
| Mainline | 12.392 | 402.694 |
| Squatter, default | 3.593 | 93.308 |
| Squatter, compact | 3.001 | 76.554 |
| Squatter, byte-only | 2.429 | 63.456 |
| Squatter, byte-only compact | 2.061 | 52.218 |

Allocator usable sizes can move either way at size-class/page boundaries; they
are preserved separately from requested bytes. These are allocation counters,
not RSS. Construction still temporarily holds mainline and Squatter together:
the largest original tracked peak is 132.66 MiB with points, 141.98 MiB with
compaction, 127.32 MiB byte-only, and 132.49 MiB byte-only compact. The equivalent
mainline peak is 114.57 MiB. See the [memory methodology](memory-results-2026-09-10.md)
for scanner-private and allocator exclusions.

The repeated allocation probe ran locally in the pinned corpus container before
the request to move timing off the battery-powered laptop. It covers 282 input
cases and 1,128 executions, with exact repetitions, matching mainline baselines,
and zero allocations left after cleanup. The [full v4 memory measurements](storage-v4-memory-2026-09-10.json)
include originals, mutations, default packing, and compact packing in both point modes.

## Correctness and compatibility

- Six C configurations pass traversal, persistence, and query suites for JSON,
  TypeScript, CSS, and YAML: points, byte-only, both under ASan/UBSan with
  128-slot unpacking, 32-slot groups, and byte-only 64-slot groups aligned to 64
  bytes. The experimental cache modes remain covered by correctness checks.
- Tests cover root/subtree iteration, non-straddling ID lanes, slot stability
  across growth/compaction, runtime pointer rebasing, read-only borrowed mappings,
  unaligned borrowed-input rejection, corrupted headers, missing presence indexes,
  supertype dictionaries, and releasing an external mapping after making a copy.
  Targeted index mutations check missing/extra bitmap bits, occurrence slots,
  sentinels, mode selection, mode tails, and padding in both loading modes.
- Rust tests/doctests and strict Clippy pass both feature modes. A compile-fail
  doctest prevents a borrowed tree from outliving its byte buffer. Both examples
  exercise borrowed nodes and queries; formatting passes.
- All 352 bounded corpus/build cases pass, checking every supported workload on
  originals and mutations with points enabled/disabled. Matching version-3
  comparisons preserve node/group/capacity counts and the existing expected
  field/seek mismatch counts. Those two-repeat local runs establish correctness;
  the timing tables use the separate cloud runs.
- The format version changes intentionally. Version 1–3 blobs and incompatible
  point/group/alignment layouts are rejected. Recreate old blobs from source;
  no migration reader is included. The serialized format remains native-endian.

The enabled cloud idle timer stops the VM after 30 minutes without wrapped
benchmark work, checked once a minute. Its lock survives an SSH disconnect and
is inherited by benchmark children. The existing 12-hour runtime limit remains
a hard backstop. See [operational instructions](../../../tools/squatter/README.md#cloud-idle-shutdown).

The [validation record](storage-v4-validation-2026-09-10.json) lists configurations
and local log hashes. Raw cloud output is under `build/squat-v4/cloud-results/`.
The idle timer was observed requesting poweroff after 1,812 idle seconds. The VM
was then restarted to retrieve results and prepare the named-field follow-up.
