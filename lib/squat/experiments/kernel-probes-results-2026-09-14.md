# Query SWAR, SIMD unpacking, and retained decode caches — 2026-09-14

This investigation compares changes to the packed scan and unpacking kernels,
and tests bulk decoding in two different parts of query execution. It uses
actual highlighting and tags queries, including text predicates, and complete
tree walks. Production code and the packed layout are unchanged.

## Measured results

Negative percentages mean less CPU time. These are grammar-balanced
paired ratios; all repeated comparisons below use three rounds. Points-enabled
and byte-only builds are separate baselines.

| Query change | Highlight, points | Tags, points | Highlight, byte-only | Tags, byte-only |
|---|---:|---:|---:|---:|
| Presence cache: 4 × 64 slots | -3.4% | -1.7% | -5.0% | -1.9% |
| Presence cache: 4 × 256 slots | -3.7% | -2.0% | -5.2% | -2.0% |
| Packed group equality with PEXT | -2.0% | +0.0% | -1.7% | -0.3% |
| Node-entry cache: 4 × 256 slots | +1.8% | +5.2% | — | — |
| Full decoded columns across executions | -3.3% | -3.0% | — | — |
| Later-sibling SWAR masks, local | +0.1% | +6.4% | +0.7% | +5.1% |
| Later-sibling SWAR masks, retained | +0.3% | +6.0% | +0.5% | +5.9% |

The later-sibling field-mask probes give no useful highlighting improvement
and slow tags by 5.1–6.4%. Keep that scalar path: scanning/retaining whole-group
field matches does not repay its overhead in these workloads. This result does
not argue against existing SWAR field checks in descendant-presence filtering.

The presence-only cache is the strongest broadly applicable query result. For
the 256-slot cache, points-enabled TSX-small and TypeScript-small highlighting
improve by 14.7% and 13.2%; TSX-large-1 improves by 11.3%. Other jobs vary from
small gains to approximately flat. The 64-slot cache performs almost as well
with one quarter of the decoded storage. Node-entry caching loses time despite
using the same general decoding machinery. Retaining full columns across
executions helps modestly, but requires much more memory and a sound tree lifetime.

| Root scan change (only jobs that use it) | JSON highlights, points | C/C++/Python tags, points | JSON highlights, byte-only | C/C++/Python tags, byte-only |
|---|---:|---:|---:|---:|
| Hoist divisions | -0.8% | -2.2% | -0.2% | -2.9% |
| Hoist + lane lookup | -3.0% | -4.1% | -2.2% | -2.0% |
| Constant-width specialization | -4.6% | -4.6% | -3.7% | -4.2% |

These are eight active-filter jobs, not the whole query suite. Width
specialization helps C tags by 9.4% and JSON highlighting by 2.4–5.8%; Python
tags are flat or slightly slower. The larger specialized function is therefore
a tradeoff, not proof of a universal optimum.

| Cached full-tree walk kernel | Points | Byte-only |
|---|---:|---:|
| Width-specialized BMI2 | -2.8% | -3.5% |
| Existing AVX2 unpack | +2.2% | +2.1% |
| Existing SWAR unpack | +3.3% | +3.5% |

The default BMI2 choice beats the existing AVX2 and SWAR unpack kernels on
this Intel host. Constant-width specialization still improves complete cached
walks by approximately 3%; existing dispatch is sensible but the implementation
is not exhausted. These unpack-kernel probes do not alter ordinary query reads.

The one-round screen found no broad win for manually vectorizing the root
filter with AVX2. Disabling the filter increased highlighting time by 5.5% and
tags time by 35.9%. Clipping hit masks earlier increased tags time by 11.6%.
Those screens motivated the repeated comparisons above; they are not equally
strong evidence for small differences.

Whole-column decoding for a single execution was also screened separately:
highlights −1.9% / −3.2%, tags +2.7% / −7.4% (points / byte-only). These noisier
single-round measurements include allocation and decode setup; do not combine
them with the reused-cursor results.

The practical candidates are a small retained presence cache and narrower
constant-width specialization where profiling justifies its code size. Their
effects have been measured separately, so they must not be added to predict a
combined speedup. No production optimization is selected by this experiment.


## What the implementation already does

The root-symbol filter precomputes repeated values and masks, merges compatible
symbol alternatives, and tests several packed values with exact SWAR equality.
The optimized GCC build already vectorizes its comparisons using SSE2. An
explicit AVX2 implementation therefore competes with existing SIMD, rather than
with a purely scalar loop. The arithmetic deliberately prevents inter-lane
carries: an approximate “any zero lane” idiom is insufficient when the scanner
must return the first exact matching node in reverse physical order.

Only JSON highlighting uses this root filter in the sampled highlighting suite.
C/C++ and Python tags also use it. The full JavaScript-plus-TypeScript tags
queries have 25 root targets and fall back to scalar membership; the narrower
TypeScript-only supplements would give a misleading picture of this workload.

There is another important scan path. Descendant-presence checks call the public
group equality scanners repeatedly for symbol/field requirements. Their existing
cache reuses established empty intervals for each requirement; it does not share
decoded column values across requirements. This is a distinct opportunity for
bulk unpacking even when the root-symbol filter is disabled.

## Probes

| Probe | Change |
|---|---|
| `noscan` | Disable the root SWAR filter; retain ordinary query matching. |
| `scalarfilter` | Disable compiler vectorization of the root filter only. |
| `simd` | Hand-written AVX2 masked comparisons, four alternatives at a time. |
| `autoavx` | Let GCC vectorize the original root filter for AVX2. |
| `clipped` | Clip matching bits to the requested interval before selecting a hit. |
| `hoist` | Reuse the stored lane count and hoist the final packed-word calculation. |
| `lookup` | Add a precomputed bit-position-to-lane table to `hoist`. |
| `scanwidth` | Specialize the root scan for widths 5, 6, 8, 9, 10, and 16; retain a generic fallback. |
| `swar`, `avx2` | Select the existing unpacking kernels for cached full walks. |
| `special` | Specialize BMI2 unpacking for each width, eliminating variable divisions/shifts. |
| `cache16`, `cache64`, `cache256` | Decode IDs for query node entry: one 16-slot block, four 64-slot blocks, or four 256-slot blocks. |
| `cachefull` | Lazily decode entire symbol/field columns and retain them until the execution ends. |
| `warmfull` | Retain full decoded columns across repeated executions on the same immutable fixture tree; compare against an equally reused baseline cursor. |
| `presence64`, `presence256` | Cache decoded blocks specifically for descendant-presence checks; compare 16 decoded IDs using SSE2. Leave node-entry reads unchanged. |
| `fieldlocal`, `fieldcache` | SWAR field equality during later-sibling checks; reuse locally or retain four `(group, field)` masks across calls. |
| `grouppext` | Precompute SWAR constants once per group scan, then use BMI2 PEXT to extract matching lane bits instead of iterating and dividing for every hit. |

The block caches add approximately 80 bytes, 1 KiB, or 4 KiB per cursor. Full
decoding uses two bytes per physical slot for each decoded column, up to four
bytes per slot for both IDs. The presence probes share decoded blocks across
requirements, navigation, and successive capture/match calls. They reset on
`exec()`. None of these probes bulk-decodes sparse grammar-symbol overrides.

The warm experiment deliberately pins every immutable tree for its cursor's
lifetime. Its pointer-equality guard is an experimental assumption, not a
production lifetime mechanism: a reusable production cache must be owned by the
tree or otherwise invalidated when that tree is destroyed/replaced.

## Method and scope

- GCP `squatter-benchmark`, `us-central1-a`, `e2-standard-2`; Intel Xeon 2.20 GHz,
  family 6/model 79 (Broadwell), BMI2 and AVX2. Timed processes run serially on
  CPU 0. Results do not establish optimality on other processors.
- Frozen runtime `ab9143b2fa3aff3fbde2062eb85988be7c6c7042`, the same baseline
  used for the preceding byte-width experiments. Query, query-plan, unpack, and
  iterator kernels match the starting checkout; subsequent format changes from
  other work are excluded. There is no rounding or slab-size change between
  the candidates.
- GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`, 16-slot groups/windows, no LTO;
  Rust 1.95.0 release harness. Explicit ISA probes are experiments for this
  supported host, not unconditional production dispatch changes.
- The existing hashed corpus has 42 files, 11 grammars, 20 highlighting jobs,
  and 12 tags jobs. Small jobs contain three files; larger jobs use individual
  files of approximately 1–3.5 MiB. Full upstream JavaScript tags are included
  with the TypeScript additions.
- Query timing includes cursor setup, matching, predicates, and consumption of
  all ordered captures/matches. Parsing and query compilation precede timing.
  It does not measure editor rendering, injections, or tag-document formatting.
  Warm timings reuse cursors on both sides and amortize the initial decode.
- Every first-round query result is compared with Tree-sitter's complete ordered
  output. Every timed repetition and every paired variant must have identical
  capture/match counts and checksums. Complete walks consume identical node
  attributes through cached, uncached, and cursor APIs.
- Five calibrated CPU-time samples per binary/job/run. Compare paired medians,
  then take the median ratio across rounds for each job. Aggregate geometrically
  within a grammar, then across grammars. A byte-only/points-enabled comparison
  is always paired within the same configuration.
- Reject a complete paired block if its identical-code control differs from
  baseline by more than 15% in any timed operation. Retain rejected raw results
  and show unfiltered sensitivity. Exploratory one-round screens are identified
  separately from repeated comparisons.

## Profiling and code generation

Software `cpu-clock:u` profiles attribute approximately 40% of C-small-tags,
24% of C++-small-tags, and 15% of JSON-small-highlighting samples to the root
scanner. Group equality accounts for approximately 15% of TSX-small-highlighting
samples. These are whole-process self samples, including setup, and serve to
locate work rather than replace query timings. Hardware cycle/instruction events
were unavailable on this VM.

The original root scanner contains SSE2 `paddq`/mask operations and variable
integer divisions. Hoisting reduces the static division sites in its generated
function from five to three; the lane lookup leaves two. Specialization trades
code size for constant-width arithmetic: the root scan grows from 1,255 to
8,409 bytes, and BMI2 unpacking grows from 708 to 5,326 bytes in this build.

The query engine already uses SWAR field equality in descendant-presence
checks. At ordinary node entry, it reads the field ID once and compares that
value with active steps, which is not a batch of independent node loads.
Later-sibling field checks are a separate scalar loop. The two field-mask probes
replace only that loop's ID comparisons with group SWAR masks, while retaining
sibling navigation so a field belonging to a descendant cannot qualify.

The retained presence cache amortizes decoding over comparisons for several
requirements. Its SSE2 kernel compares two groups of eight u16 values, packs
the comparison results into bytes, and extracts a 16-bit mask. The PEXT
alternative keeps values packed and extracts their matching high bits directly.

A supporting unpack microbenchmark improves non-native-width BMI2 throughput
by approximately 31–57% after specialization, substantially more than the 3%
full-walk improvement. The complete-walk results determine the recommendation;
the isolated kernel numbers are retained only as supporting evidence.

## Control audit

Three rounds were attempted for each repeated comparison; rejected blocks
remain in the raw data. The walk repair contributes one additional attempt for
each of two noisy cases. Accepted per-job sample counts appear in the summaries.

| Phase | Paired blocks | Rejected |
|---|---:|---:|
| Node-entry cache | 96 | 2 |
| Full cache across executions | 96 | 2 |
| Presence cache / PEXT | 192 | 1 |
| Later-sibling field masks | 192 | 5 |
| Root-scan specialization | 48 | 0 |
| Complete walks, including repairs | 122 | 2 |

Keeping the rejected blocks changes the presence-cache aggregates by less
than 0.02 percentage points. Unfiltered summaries for all repeated phases are
included in the raw artifact; no favorable-only job selection is applied.

## Correctness and reproduction

The experiment tooling is committed separately as `ab6162834`:

```sh
python3 tools/squatter/prepare-kernel-probes.py --points 1,0
python3 tools/squatter/benchmark-kernel-probes.py BUNDLE \
  --kind query --variants exact,control,presence64,presence256,grouppext \
  --points 1,0 --rounds 3 --repeat 5 --tag presence-queries
python3 tools/squatter/summarize-kernel-probes.py RUN.json --output summary.json
```

The prepared host bundle contains the existing hashed query/source/grammar jobs,
`walk-jobs.json`, generated binaries, per-variant patches, and the build manifest.
The GCP loader is `/lib64/ld-linux-x86-64.so.2`. Build manifests, commands, hashes,
raw timings, diagnostic/profiling output, validation logs, and summaries accompany
the final results. Historical manifests are retained for each measurement phase.

Unpack tests cover widths 1–16, every starting lane and short count, output
sentinels, dirty tails, and the last allocated word. The additional group-scanner
fixture compares public scans with scalar reads for widths 1–32, every waste
count, and dense/sparse matches. ASan/UBSan runs also exercise query ranges,
capture snapshots, removal, limits, and optimization modes. The existing query
tests retain their documented expected negated-field exceptions; the real
highlighting/tags comparisons use no such exception.

The first walk-only harness had a cleanup bug after omitting query setup. It was
fixed before collecting usable walk results; the failed invocation is retained
in the audit. Unrelated changes to `lib/squat/Makefile` and `todo.md` are excluded.

[Raw measurements and audit](kernel-probes-results-2026-09-14.json).
