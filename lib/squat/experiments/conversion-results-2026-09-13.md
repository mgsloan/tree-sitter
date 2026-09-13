# Conversion follow-up, 2026-09-13

## Reusable context

`SQPackContext` retains one language, its ordered supertype list and index table,
the public-symbol mapping, and traversal/presence scratch. Per-pack counters and
dictionary IDs reset each call, including after failures. Output trees own their
slabs and retain their own language references. `trim` releases high-water
scratch, retaining grammar tables. Separate contexts support concurrent callers;
one context requires exclusive access. Rust exposes the same ownership through
`PackContext`, whose pack and trim methods require `&mut self`.

[Measurements](conversion-context-2026-09-13.json) were taken on the small GCP
`squatter-benchmark` VM, CPU 0, GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`,
points enabled, 16-slot groups. Five alternating process pairs, with nine timed
samples per small-file process, each doing 16 complete batches; reported times
are divided by 16. Context creation is outside timing, while per-tree output
allocation and deletion are inside. All eight grammars have 300 files below
2 KiB. Lists use newline separation because some YAML paths contain spaces.
An initial sub-millisecond-batch pilot was discarded in favor of these longer
samples.

| Grammar | Ordinary ms/batch | Context ms/batch | Change |
|---|---:|---:|---:|
| C++ | 1.842 | 1.618 | −12.2% |
| CSS | 1.071 | 0.995 | −7.1% |
| Go | 6.817 | 6.597 | −3.2% |
| JSON | 0.945 | 0.890 | −5.8% |
| Python | 0.894 | 0.782 | −12.6% |
| TSX | 1.208 | 1.069 | −11.5% |
| TypeScript | 0.456 | 0.330 | −27.6% |
| YAML | 0.559 | 0.476 | −14.8% |
| Total | 13.792 | 12.757 | −7.5% |

The nine large files are a control, not the target. Against the previous binary,
the new ordinary path measured +0.7% default / +1.2% compact; context measured
−0.8% / +0.8%. These small movements do not establish a large-file benefit.
Default totals were 377.59 / 380.16 / 374.62 ms (old / ordinary / context), compact
385.44 / 390.23 / 388.37 ms. Each uses five alternating pairs of seven-conversion
medians, with equal serialized hashes, bytes, groups, capacities, and node counts.
These are conversion measurements, not parse-plus-convert improvements.

Validation: exact ordinary/context byte equality across eight combinations of
capacity, compaction, and presence on all nine large inputs; context trimming,
wrong-language rejection, invalid arguments, overflow, output lifetime, and
allocation-failure recovery. A large TypeScript input also passed every pack
allocation failure under ASan/UBSan with leak detection. Rust `cargo check`
passed. Byte-only, 32-slot, and 64-slot context checks passed on all nine large
files. ASan/UBSan differential traversal and persistence checks passed across all
eleven grammars (edge cases, small files where available, and deterministic
mutations); expected field mismatches were zero. Existing ignored seek
differences remained. Context failure recovery also passed with synthetic 9- and
65-supertype metadata, exercising dictionary and multiword scratch paths.

## Scalar fit checks

Reverse preorder makes start bytes and start rows nonincreasing: visit later
children before earlier ones, then the parent. For each of these ranges the
candidate is the new minimum and the first accepted value remains the maximum.
Two general min/max extensions can therefore become simple range checks. The
full-group rejection in `group_fits` is also redundant: `emit` closes it before
staging a candidate.

[Paired cloud results](conversion-fit-2026-09-13.json): default 377.08 to
367.37 ms (−2.57%), compact 384.43 to 374.34 ms (−2.62%). Same nine inputs and
five alternating pairs of seven-repeat medians, exact serialized hashes and
topology counts throughout. Python improves about 5.7%; two individual rows
move slightly backward (+0.4% YAML default, +0.6% one TypeScript compact).
ASan/UBSan differential/persistence checks pass on all eleven grammars after
this change, including malformed input and deterministic edits.

The [diagnostic counts](conversion-diagnostics-2026-09-13.json) were collected
separately from timed binaries. Apply the accompanying
[instrumentation patch](conversion-diagnostics-2026-09-13.patch) to `0a439c1e2`
to reproduce them. Fit counts are the first rejecting test in existing order,
not independent attribution of overlapping constraints.

## Capacity estimate: retain the 75% default

[Capacity sweep](conversion-capacity-2026-09-13.json), after the scalar fit
change: choosing initial capacity as `nodes * 100 / (group_size * percent) + 1`
with percent 80 instead of 75 saves 5.5% retained bytes on the nine large files
and measures 1.0% faster default packing. No large file grows under either
estimate. Their actual occupancy ranges from 83.5% to nearly 100%.

On the 2,400 small files it saves 3.0% retained bytes but measures 0.4% slower,
and growth rises from 41 to 59 files: Go 34→46, C++ 4→8, Python 0→1, TSX 1→2,
JSON unchanged at 2. Thus a universal increase trades additional growth for
modest memory savings; the production default remains 75%.

The allocation probe also measured construction peaks. Summed per-file peaks
including mainline (not simultaneous batch peaks) fall from 520.46 to 515.06 MB
for default construction and 589.36 to 583.95 MB compact. Compact retained bytes
remain identical, 80.27 MB; default retained bytes fall 97.84→92.43 MB. These
are requested allocation sizes, not RSS, and exclude the probe's accounting
table and grammar mappings.

Reproduce with `capacity-bench LIBRARY SYMBOL REPEATS SOURCE...`, setting
`SQ_CAPACITY_PERCENT=75` or `80` and `SQ_BATCH_LOOPS=16` for small batches.
The probe hashes canonical compact output outside timing to check semantic and
serialized equivalence despite intentional capacity/layout differences.
`memory-bench` accepts the same percentage setting. Timed samples include output
deletion, exclude parsing and canonicalization, and alternate five pairs.

## Conditional opportunities

The large-input diagnostics find one nonempty hidden subtree with no visible
children across the entire nine-file sample, and zero dictionary comparisons
(the tested grammars use at most eight supertypes). Adding a pruning branch to
every child, or a hash table for mask interning, has no demonstrated payoff on
these workloads. Both remain conditional on other grammars or error-heavy
inputs showing significant counts. No representation or default-presence change
is justified by these results.
