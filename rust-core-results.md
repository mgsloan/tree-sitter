# Rust core implementation and comparisons

The candidate implements storage, packing, navigation, typed scans, and query
execution in Rust. C retains grammar preparation, tree-feller, and query
compilation. Rust packing reads the private subtree layout through generated
bindings and traverses directly into the encoder. Compiled query records stay in their native
allocation; Rust borrows them and releases the owner on drop. Release execution
trusts compiler output; debug builds validate it.

Public Rust signatures are unchanged. `tree-squatter-rust` is an unpublished
comparison crate. The default remains C-backed `tree-squatter`, and the C facade
is deferred. Persistence and benchmarks can select the candidate with their
`rust-core` feature. There is no native query execution fallback.

## Rust assembly follow-up — 2026-09-22 UTC

Three changes are retained on `c-optimizations`, with conversion entirely in Rust:

| Change | Commit | Assembly effect |
| --- | --- | --- |
| Inline common child-mask propagation | `dd20ec429` | Zero/one-word masks avoid an allocation-capable helper and its six saved registers. |
| Capture the writer for node IDs | `c622d51eb` | Symbol, field, and optional grammar stores share one slab-address load. |
| Encode point deltas separately | `e07cb3011` | The slot loops lose stack accesses; point encoding can use four-lane vectors. |

The baseline is `8162ad00e`, whose Rust executable matches the previously selected
`d96b0793d` build. C includes emission inlining from `1e9c80d16`. Default packing
uses the same 264 files and 11 languages:

| Operation | Rust before ms | Rust selected ms | C ms | Rust time reduction | Rust / C |
| --- | ---: | ---: | ---: | ---: | ---: |
| pack-cold | 65.457 | 62.071 | 62.095 | 5.17% | 1.000× |
| pack-reuse | 64.958 | 61.986 | 61.689 | 4.57% | 1.005× |
| pack-trim | 65.394 | 62.314 | 61.945 | 4.71% | 1.006× |

Times sum per-file milliseconds, averaging two process medians. Rust is now
within 0.6% of C for default packing: cold packing is effectively tied, while
reuse and trim take 0.5–0.6% longer. All three Rust improvements agree in both
process orders. Every language aggregate improves, though CSS varies by order.

Child-mask inlining alone reduces default reuse time by 2.40% in the initial
screen. Separate point encoding adds 0.97–1.32% across default modes after that
change. Writer caching adds little after child-mask inlining alone, but adds
0.56–1.22% once point encoding is split. Removing either addition from the final
combination slows every measured profile in both orders; these effects are not
additive.

The 22-file alternate-layout subsets improve 3.0–3.5% without points and
4.9–5.7% without presence indexes. Rust still takes 6.4–6.6% longer than C without
points and 2.7–3.0% longer without presence indexes.

Two dictionary experiments replace generic one-word comparisons with scalar
`cmp`, including an inline variant. They remain uncommitted: the exact corpus
grammars have zero to seven supertypes and never exercise dictionary lookup.
Timing differences for those builds cannot establish a lookup speedup. A corpus
with 9–64 supertypes is needed to evaluate this candidate.

Ordinary cold parsing on the 22-file subset initially regresses 1.20%, then
improves 0.64% in a confirmation run with the same binaries and reversed orders.
Warm parsing changes by -0.03% and +0.10%. The four-file direct subset also varies:
its initial warm gain of 2.77% becomes a 0.24% regression on confirmation. These
results do not establish an end-to-end parse speedup or a repeatable regression.
Both batches remain in the report.

All 2,110 eligible corpus comparisons and 26 focused release tests pass,
including byte-for-byte C compatibility. The two pre-existing shared Go capture
exclusions are unchanged. No new unsafe code is introduced. The selected source
patch and all three executable identities match the measured build.

All timings ran on the Google Cloud e2-standard-4 VM, pinned to CPU 1 under the
shared lock. The first screen uses five 15 ms samples; combination and final
runs use five 10 ms samples, in two reversed process orders. Final packing
compares baseline Rust, child-mask inlining, each addition, their combination,
and optimized C contemporaneously. No laptop benchmarks ran. The VM was stopped
after verified collection.

The [formatted report](build/rust-asm-followup/report.html) and
[Markdown report](build/rust-asm-followup/report.md) include all layouts,
per-language results, incremental comparisons, both parse runs, and source
identities. The [assembly follow-up](rust-core-assembly.md#rust-follow-up-after-c-emission-inlining)
records function sizes, loop counts, and remaining opportunities. Patches,
binaries, raw samples, assembly, grammar facts, and test logs are retained under
`build/rust-asm-followup/`.

## C emission experiments — 2026-09-22 UTC

`1e9c80d16` retains inlining of C's `emit` and `emit_values` helpers into the
traversal. The second experiment, capturing group metadata by value before
closing its slots, is dropped. The baseline is `487670d5a`, whose C code is
unchanged from `fa2e389641c6`; the Rust comparison retains all four optimizations
from `d96b0793d`.

The initial 264-file pack-reuse comparison improves 10.04% with inlining,
1.87% with metadata capture alone, and 10.01% with both. Both process orders
favor inlining. In the final comparison, metadata capture reduces cold-packing
time by 0.67% but increases reuse time by 0.66%; trimming changes by only 0.11%. The
alternate-layout results are mixed. The extra copies offer no consistent
overall improvement after inlining, so only inlining is retained.

| Operation | C before ms | C selected ms | Latest Rust ms | C time reduction |
| --- | ---: | ---: | ---: | ---: |
| pack-cold | 72.122 | 65.713 | 68.734 | 8.89% |
| pack-reuse | 71.692 | 64.485 | 68.055 | 10.05% |
| pack-trim | 72.223 | 64.877 | 68.237 | 10.17% |

Times sum representative per-file milliseconds across the same 264 inputs and
11 languages. C now takes 4.4–5.2% less time than Rust for default packing.
Forward/reverse C reductions agree: cold 8.14% / 9.62%, reuse 9.40% / 10.69%,
and trim 9.76% / 10.58%. Every language's reuse aggregate improves against C's
baseline, from 7.96% for TypeScript to 13.09% for HTML.

The 22-file profiles improve 8.2–11.8% without points and 10.6–11.6% without
presence indexes. No-points cold/trim results vary substantially between rounds;
their direction agrees, but their exact percentages are less stable.

Ordinary cold/warm parsing improves 1.03% / 0.80% on the 22-file subset, with
both orders agreeing. Direct cold parsing improves 1.50%, while direct warm
parsing regresses 0.41%; that comparison covers only four eligible files. Rust
remains faster for ordinary cold parsing in this subset, while selected C is
faster for ordinary warm parsing.

The [assembly follow-up](rust-core-assembly.md#c-emission-and-metadata-follow-ups)
records the code-size cost: traversal plus its emission helpers grows from
5,292 to 7,448 bytes. Both per-node helper calls disappear; frame initialization
and group closing retain their previous code. Metadata capture removed builder
reloads but introduced stack spills and a larger slot loop.

All benchmarks ran on the Google Cloud e2-standard-4 VM, pinned to CPU 1 under
the shared lock. The screen uses five 15 ms samples per file/workload; the final
packing run uses five 10 ms samples. Tables average two process medians per file.
Stage order rotates by language and reverses in the second round, along with
input and language order. All four builds were included in the final packing
run. No laptop benchmarks ran.

All 2,110 eligible corpus comparisons pass, with the two pre-existing shared Go
capture exclusions unchanged. The native unit, supertype, and parser suites
pass at `-O3 -Wall -Wextra -Werror`, including allocation failures. All eight
C-backed binding tests and both Rust/C byte-compatibility storage tests pass.
The committed source matches the measured patch; result hashes and binary
identities verify. The cloud VM was stopped after collection.

The [formatted C report](build/c-packing-candidates/report.html) and
[Markdown report](build/c-packing-candidates/report.md) contain all profiles,
language tables, incremental metadata comparisons, parsing results, and source
identities. Patches, binaries, raw samples, assembly, and test logs are preserved
under `build/c-packing-candidates/`.

## Four packing optimizations — 2026-09-21

All four assembly candidates are retained, with one commit each. The baseline
is `cc783db89`; the selected implementation is `d96b0793d`. Conversion stays in
Rust, and the frame rewrite adds no unsafe code.

The first cloud experiment builds each change independently. A second compares
all four against builds with each change removed. Both cover pack-reuse over
264 files and reverse execution order on the second pass:

| Change | Independent time reduction | Contribution in combined build | Commit |
| --- | ---: | ---: | --- |
| Inline small-mask handling | 2.82% | 5.02% | `44b8fdd63` |
| Hoist pending bounds checks | 0.28% | 1.32% | `0e906b68a` |
| Inline group fitting | 1.99% | 4.00% | `d12c23835` |
| Initialize retained frames earlier | 1.18% | 3.25% | `d96b0793d` |

Every contribution is positive in both execution orders. These effects interact
and cannot be added: the combined build improves pack-reuse by 9.02% in the
removal experiment. A separate final comparison measures the selected source
against the baseline and unchanged C (`fa2e389641c6`):

| Operation | C ms | Rust before ms | Rust selected ms | Rust time reduction |
| --- | ---: | ---: | ---: | ---: |
| pack-cold | 69.098 | 71.985 | 66.298 | 7.90% |
| pack-reuse | 68.655 | 71.697 | 65.529 | 8.60% |
| pack-trim | 69.005 | 72.167 | 65.972 | 8.58% |

Times sum representative per-file milliseconds across all 264 inputs. Rust now
takes 4.1–4.6% less time than C for default packing. All 11 language reuse
aggregates improve against the Rust baseline, from 6.52% for HTML to 12.09% for
JSON. Forward/reverse reductions agree: cold 7.59% / 8.21%, reuse 8.62% / 8.59%,
and trim 8.83% / 8.34%.

The 22-file alternate profiles improve too: 11.4–11.6% without points and
7.6–9.3% without presence indexes. No-points results vary more between rounds
(7.5–15.3% improvements), and C remains slightly faster for that profile.

Ordinary cold/warm parsing improves 1.43% / 0.93% on the 22-file subset, with
both process orders improving. Direct cold/warm parsing improves 4.66% / 2.12%,
but covers only four eligible inputs. These end-to-end effects are smaller than
the isolated packing gains and have narrower corpus coverage.

The [assembly analysis](rust-core-assembly.md#four-packing-follow-ups) records
the tradeoffs. Small masks avoid a helper call; pending-prefix iteration moves
the bounds check outside the slot loop; fitting stays in the emission function;
and frame initialization reduces its fixed stack allocation from 216 to 120
bytes. Inlining fitting increases traversal code size. The cloud results,
including removal experiments, justify keeping that tradeoff.

All benchmarks ran on the Google Cloud e2-standard-4 VM, pinned to CPU 1 under
the shared lock. The independent screen and final packing comparison use five
10 ms samples per process; removal experiments use five 15 ms samples. Tables
average two process medians per file. Stage order rotates by language and
reverses in the second round, along with input and language order. No laptop
benchmarks ran.

All 26 focused release tests and 2,110 eligible corpus comparisons pass. The
two pre-existing shared Go capture exclusions are unchanged. Formatting passes;
Clippy reports warnings only in unchanged code. The committed implementation
matches the measured source patch exactly, and all result hashes and binary
identities verify. The cloud instance was stopped after collection.

The [formatted report](build/packing-candidates/report.html) and
[Markdown report](build/packing-candidates/report.md) contain all profiles,
language tables, repeated-run results, and parsing comparisons. Patches,
executables, disassembly, raw samples, per-file summaries, and source identities
are preserved under `build/packing-candidates/`.

## Cached slab writer — 2026-09-21

The group-closing loop captures the slab pointer once through a borrowed
`SlabWriter`. Its lifetime prevents resizing or replacing the descriptor during
writes. Existing byte/short setters share the same implementation, preserving
unaligned little-endian storage. Traversal, group fitting, mask lookup, and
pending-array iteration are unchanged.

The [assembly follow-up](rust-core-assembly.md#slab-pointer-follow-up) confirms
that per-slot descriptor pointer loads fall from six to zero with points, and
four to zero without points. Both loops have five fewer instructions. Stack
spills remain; fixed local stack space stays at 104 bytes, and extra setup grows
the whole function from 1,034 to 1,068 bytes. The writer adds no allocation or
out-of-line call.

The Google Cloud comparison uses the full 264-file, 11-language corpus for
default packing. The baseline is `bddbf8a38` (fused Rust from `288f1139e`), and C
is unchanged from `fa2e389641c6`. Times sum representative per-file milliseconds:

| Operation | C ms | Rust before ms | Rust after ms | Rust time reduction |
| --- | ---: | ---: | ---: | ---: |
| pack-cold | 70.633 | 74.452 | 73.268 | 1.59% |
| pack-reuse | 70.166 | 73.908 | 72.730 | 1.59% |
| pack-trim | 70.299 | 74.619 | 73.252 | 1.83% |

The two input orders agree: cold packing improves 1.59% in each; reuse improves
1.80% / 1.39%, and trim improves 1.84% / 1.82%. Median same-build repetition
ratios lie between 0.999 and 1.002. Rust still takes 3.7–4.2% longer than C for
these operations. Nine language reuse aggregates improve 1.2–2.9%; Bash and C++
are approximately flat (-0.27% / +0.07%).

The 22-file profile without presence indexes improves 1.2–2.1%. The no-points
profile shows no clear gain, with opposite-sign results in the two process
orders. Ordinary parse changes are below 0.5%; direct-parse results cover only
four files and are mixed. These do not establish end-to-end parsing gains.

All timings ran on the e2-standard-4 cloud VM, pinned to CPU 1 under the shared
benchmark lock. Packing uses five samples targeting 10 ms, with C / before /
after / after / before / C order and reversed files/languages on odd rounds.
The initial 22-file pilot used 50 ms samples and found a 0.85% reuse improvement.
No benchmarks ran on the laptop.

All 26 focused release tests, 10 ASan storage/binding tests, and 2,110 eligible
corpus comparisons pass. The two pre-existing shared Go capture exclusions are
unchanged. Formatting passes; Clippy reports only existing warnings.

The [formatted report](build/slab-writer/report.html) and
[Markdown report](build/slab-writer/report.md) contain all profiles, language
tables, repetition controls, and identities. All 790 medium-run and 136 pilot
artifact hashes verified; [per-file results](build/slab-writer/cloud/summary.json),
assembly, binaries, patches, and logs are preserved under `build/slab-writer/`.

The full run completed at 06:18 UTC. The VM subsequently shut down and could not
start SSH because its boot disk was full. Expanding the disk from 10 to 12 GB
restored access; the completed results were collected and verified without
repeating timings. The VM was stopped after recovery.

## Fused Rust packing and assembly comparison — 2026-09-21

Commit `288f1139e` moves private subtree traversal and direct-parser reduction
traversal into Rust, calling the encoder directly. Traversal frames now retain
their physical subtree boundaries; there is no event batch or second boundary
stack. The native bridge retains grammar preparation, tree-feller parsing, and
query compilation. Builds now require libclang for target-specific subtree
bindings.

The [assembly comparison](rust-core-assembly.md) identifies an extra emission
call layer and 128-byte frame copies in the initial port. Inlining emission and
borrowing completed frames removes those costs. Remaining candidates include
inlining the small-mask fast path, caching column addresses during group closure,
and moving pending-array bounds checks outside the slot loop. The public
Tree-sitter Rust node/cursor API is absent from the per-node packing path.

The initial port was measured on the same 264-file, 11-language cloud corpus as
the previous refresh. Isolated packing regressed 4.3–4.7% against batched Rust,
while ordinary parsing was nearly unchanged and direct warm parsing improved
2.9%. Those measurements precede the final assembly changes.

The current executable was then compared on 22 files: the median-size and
largest file per language. All timings ran on the Google Cloud e2-standard-4 VM,
pinned to CPU 1. Cells sum per-file representative milliseconds, averaging two
independent processes per implementation:

| Operation | C ms | Batched Rust ms | Initial fused ms | Current Rust ms | Reduction vs batched |
| --- | ---: | ---: | ---: | ---: | ---: |
| pack-cold | 16.199 | 17.466 | 18.228 | 17.116 | 2.00% |
| pack-reuse | 16.043 | 17.477 | 17.988 | 16.957 | 2.98% |
| pack-trim | 16.055 | 17.411 | 18.183 | 17.141 | 1.55% |
| cold-parse | 226.938 | 223.246 | 223.327 | 221.468 | 0.80% |
| warm-parse | 152.230 | 153.288 | 153.491 | 152.941 | 0.23% |

The assembly changes reduce packing time 5.7–6.1% from the first fused version.
Current Rust still takes 5.7–6.8% longer than C for isolated packing in this
selection. Language results vary: C++, TypeScript, Go, and Python reuse improve
5–8% against batched Rust, while JSON, TSX, and YAML regress 1–3%. Sub-percent
parse changes are not established gains. The focused follow-up does not replace
the medium comparison or establish final alternate-profile timings.

Packing uses five samples targeting 20 ms per process. The order is C / batched
Rust / initial fused / current / current / initial fused / batched / C, reversing
file and language order on odd rounds. Median same-build packing repetition
ratios range from 0.997 to 1.004. C remains unchanged from `fa2e389641c6`; batched
Rust is `d1712fee2918`. The initial and current candidates have separate saved
patches and executable hashes.

The current executable passes all 2,110 eligible file/operation comparisons over
the full corpus, retaining the two shared Go capture exclusions documented
below. All 26 focused release tests pass. Before the final inlining/frame-borrow
changes, the port also passed 15 ASan tests and 11 i686 Linux-musl runtime tests.
PowerPC64 big-endian compilation passed; execution remains unverified. Formatting
passes; Clippy reports only existing warnings.

The [formatted report](build/fused-packing/report.html) and
[Markdown report](build/fused-packing/report.md) contain both experiments,
alternate profiles, direct parsing, language tables, and exact exclusions.
[Current results](build/fused-packing/assembly-check-cloud/assembly-check/summary.json)
retain per-file samples and identities. All 805 medium-run and 685 follow-up
artifact hashes verified. Assembly, binaries, patches, source archives, profiles,
and logs are under `build/fused-packing/`. No benchmarks ran on the laptop; the
VM was stopped after collection.

## Google Cloud refresh and Rust packing optimization — 2026-09-21

The merged `c-optimizations` baseline is `fa2e389641c6`, including the C repack/navigation changes and `rust-core` through `48126eec5`. Both backends were built from that snapshot with portable Cargo release defaults, Rust 1.95.0 / LLVM 22.1.2 and GCC 15.3.0. All timings ran on the Google Cloud `squatter-benchmark` e2-standard-4 VM, pinned to CPU 1.

The deterministic medium corpus contains 264 files across 11 languages, totaling 3.39 MiB. Coverage includes all eight API operations, all 15 lifecycle operations, all 267 scan workloads across four profiles, and all 120 supported registry queries. API/tree/scan operations use the full eligible corpus; input-independent grammar and query lifecycle operations use representative files.

**Speedup is C time divided by Rust time; above 1 favors Rust.** Times below sum representative per-file milliseconds. These API results precede the packing change.

| Operation | Files | C ms | Rust ms | Rust speedup |
| --- | ---: | ---: | ---: | ---: |
| cold-parse | 264 | 1468.830 | 1415.938 | 1.04× |
| warm-parse | 264 | 635.309 | 644.189 | 0.99× |
| cursor-forward | 264 | 18.293 | 10.701 | 1.71× |
| scan-forward | 264 | 73.957 | 55.672 | 1.33× |
| seek-byte | 264 | 2.522 | 2.126 | 1.19× |
| seek-point | 264 | 3.519 | 3.298 | 1.07× |
| query-matches | 264 | 929.724 | 773.902 | 1.20× |
| query-captures | 262 | 1126.098 | 920.705 | 1.22× |

The scan workload medians are 1.19× with frequent kinds, 1.18× with sparse kinds, 1.40× with absent kinds, and 1.18× without the symbol index. Each profile has four workloads at least 5% slower in Rust. The C-backed public scan facade also uses Rust scan loops; these are backend comparisons.

Commit `4650f3ab1` stores only pending ancestor boundaries during reverse-preorder packing. Leaves use the current position directly, avoiding stack writes/reads and unnecessary heap-stack growth at the inline boundary. Slab bytes and public APIs are unchanged.

The separate packing experiment compares C, baseline Rust, and two candidates in forward/reverse process order. Each process uses five samples targeting 10 ms; each cell averages two process medians per file. All 264 files contribute to the default profile:

| Operation | C ms | Rust before ms | Rust after ms | Rust time reduction | C / final Rust |
| --- | ---: | ---: | ---: | ---: | ---: |
| pack-cold | 68.819 | 76.726 | 74.504 | 2.90% | 0.924× |
| pack-reuse | 68.420 | 75.893 | 73.919 | 2.60% | 0.926× |
| pack-trim | 68.616 | 76.407 | 74.319 | 2.73% | 0.923× |

The retained change improves all 11 language aggregates. The 22-file alternate profiles also improve: packing time falls 1.7–2.0% without points and 1.5–2.2% without presence indexes. An additional equal-depth fast path produced no consistent benefit and was not retained. Median same-build repetition ratios in the default comparison range from 0.998 to 1.001.

Rust packing still takes about 8% longer than C in the default profile. Other remaining targets are safety/backed loading (0.94×/0.93×), capture disabling (0.91×), and field scans: four-field counting is 0.80× and two-field enumeration is 0.85×. Query construction is near parity; small query-mutation timings need focused follow-up before attributing their ratios to specific code.

The baseline uses C / Rust / Rust / C independent processes, five API repeats, and three scan/lifecycle samples targeting 20/10 ms. The final packing experiment uses C / baseline Rust / candidate A / candidate B / B / A / baseline Rust / C, reversing file and language order on odd rounds. No benchmarks ran on the laptop.

Validation covers byte-exact C packing/cross-loading, deep/wide/error trees, points/presence/repack options, and a new sibling-tree fixture crossing the inline boundary. All 23 focused release tests pass. The full-corpus checks retain two pre-existing Go capture exclusions shared by C and Rust: `values_test.go` and `index_test.go` from Helm. The other 2,110 file/operation combinations pass with the committed implementation; timed API records have no failures.

The [formatted report](build/rust-performance/cloud/report.html) and [Markdown report](build/rust-performance/cloud/report.md) contain all operation tables, language breakdowns, candidate comparisons, repeat controls, and exact exclusion paths. [Packing results](build/rust-performance/packing-full/cloud/summary.json) retain all per-file measurements. Source/input archives, binaries, patches, logs, hashes, and scripts are under `build/rust-performance/`; 1,545 baseline, 591 follow-up, and 63 final-validation artifact hashes verified. The initial full-disk preflight failed before timing and is excluded.

The sections below retain earlier measurements and their original baselines.

## Baseline and measurement scope

The original measurements use reference `0c3f79ab5`; `83d962cdd` differs only in
`todo.md`. The current reference is `main` at `9a3f0292c`, which merges `iteration`
through `dae674ea2`. The first iteration refresh below uses `8bb827f73` /
`29a192f92`; the sparse-cursor refresh uses `51ecbfafb` / `9cf1cdb35`. Earlier ratios
remain tied to their stated baseline and must not be read as comparisons against
current `main`. The port itself does not modify reference production
sources. Both cores use one resolved Tree-sitter runtime and disjoint
Squatter/tree-feller native symbols.

Original measurements used an Intel Core Ultra 7 165U, plugged in, Linux x86_64,
Rust 1.95.0 / LLVM 22.1.2, and GCC 15.3.0. Release builds retain the existing
optimization settings and portable x86_64 target; SSE2 is available to both cores.
Timing runs pin a performance core, usually CPU 2; lifecycle pilot runs also used
CPU 0. These are different physical cores. Background correctness checks used
efficiency cores. Pre-plug timings are preliminary and excluded here.

Local raw samples, input/grammar/query hashes, copied executables, profiles, and
logs are under `/tmp/squatter-rust-core-bench`. They are machine-local artifacts,
not checked-in fixtures. Results, logs, heap profiles, and selected executables
are also preserved in
[`build/rust-core-comparison/b69d19e92`](build/rust-core-comparison/b69d19e92),
with a SHA-256 manifest. `registry.json` references the pre-existing staged
grammars and queries in `../main/build/squat-query-corpus`.
`paired-inputs.json` selects one input under 50 KB for each of 11 grammars: bash,
C, C++, CSS, Go, HTML, JSON, Python, TSX, TypeScript, and YAML. Timed queries come
from the staged grammar repositories and Zed.

The probes preserve raw samples and binary identities. Some development runs
overlapped later source edits, so their runtime checkout/patch identity must not
be mistaken for an immutable build snapshot. The copied binary identifies those
runs. Use `cargo xtask squat` snapshots for final acceptance runs.

## First iteration refresh

The first refresh carries over the scan changes through `29a192f92`: two-sided
range seeking, forward byte-subtree pruning, bounded sparse/bitmap symbol-index
traversal, prepared small dynamic kind sets, and separate dense count loops.
The candidate derives index offsets from its Rust-owned slab; no native getter
or slab-format change is needed. Singleton predicates also retain their prepared
column offset and shift.

Query root and descendant-presence experiments prepare only column masks. Their
existing traversal, index policy, cancellation, budgets, and cooldown stay
separate. This refresh does not establish new query-execution timing results.

Artifacts are in
[`build/rust-core-comparison/iteration-refresh`](build/rust-core-comparison/iteration-refresh).
`final/` contains that implementation's full scan comparison; the parent
directory retains the initial port and focused diagnostics. Patches identify
the candidate changes relative to `c378a6368`; copied executables and SHA-256
manifests identify the measured builds. The reference is `8bb827f73`.

These runs followed laptop resume, with fresh warmups and AC reported offline.
They compare separately linked processes on CPU 2 using the same eleven inputs
(28,299 nodes), seven samples targeting 20 ms each, and 91 selected workloads.
Each profile brackets the candidates with reference/reference controls; candidate
order alternates between profiles. The benchmark validates results against
scalar traversal before timing, and input/output counts agree across backends.
Do not compare their absolute timings with the earlier plugged-in runs.

Ratios are elapsed time divided by the comparison backend's time; smaller is
faster. Each median is across workloads, not aggregate application throughput.
The reference column uses the median throughput of the two reference processes.

| Kind selection | Final / old Rust median | Final / reference median | Reference-repeat ratio range |
|---|---:|---:|---:|
| Frequent | 0.685 | 0.739 | 0.821–1.436 |
| Rare | 0.239 | 0.748 | 0.772–1.586 |
| Absent | 0.052 | 0.636 | 0.749–1.562 |
| Frequent, symbol index disabled | 0.915 | 0.975 | 0.788–1.273 |

The wide reference-repeat ranges make these provisional results. Unrelated
build activity was observed on the host. The initial port's matrix had less
drift and also showed broad gains, but predates the singleton metadata change;
its numbers must not substitute for final-build measurements. No no-regression
claim follows from either matrix.

Remaining outliers include:

- Unfiltered preorder folds take 1.36–2.24 times the old Rust candidate's time
  and 1.60–2.33 times the reference's time across profiles.
- Rare fixed sixteen-kind counts and folds take 1.42 and 1.34 times the old
  candidate's time. The initial port also regressed these cases, so selective
  index preparation and probing need further examination.
- Dynamic four-kind folds improve over the old candidate but remain 1.33–1.71
  times the reference's time for frequent kinds, with and without the index.
  Rare dynamic sixteen-kind folds reach 1.75 times the reference's time.
- Without the symbol index, reverse byte-range traversal and dynamic singleton
  folds take 1.83 and 1.50 times the old candidate's time in this noisy matrix.
  They need focused repeats before attributing those differences to the code.

Earlier focused singleton repeats showed that retaining column metadata improves
its fold by about 10% over the initial port, leaving it about 4–5% slower than
the old candidate (`singleton-focus-*`). This narrower result motivated retaining
the change; it does not settle every consumer or index configuration.

The unfiltered preorder fold is sensitive to code placement. In the initial port,
its hot function has the same 85 instructions after normalizing addresses, but
the inner loop crosses a 64-byte boundary. Three focused repeats take
1.77–1.98 times the old candidate's time. A diagnostic build with
`-C llvm-args=-align-all-blocks=5` reduces that to 0.91–1.03. This supports an
alignment explanation but does not establish a portable fix; production build
flags are unchanged. The normal-build outlier remains an acceptance issue.

That implementation passes 28 all-feature tests: six library tests, four
query-execution tests, two scan-pattern tests, and sixteen shared scanning tests.
The new unit tests cover clipped sparse/bitmap searches in both directions and
verify that query-only preparation produces the same masks without preparing an
index. Before the singleton metadata adjustment, all six library and sixteen
scanning tests also passed with 32-slot groups and 64-byte column alignment.
Formatting and whitespace checks pass. The sanitizer and cross-target checks
below belong to the earlier implementation and were not repeated for this port.

## Sparse-cursor refresh

The sparse-cursor refresh includes scan changes through `46828abdd`, with
reference `main` at `51ecbfafb` / iteration `9cf1cdb35`. Sparse index jumps and
exact masks share per-target posting cursors. Each seek verifies the hint, probes
at most four nearby entries, then binary-searches the remaining interval. Fixed
arrays keep one cursor per ID; dynamic sets keep sixteen, with uncached searches
for further targets. Bitmap traversal keeps its existing policy.

Predicates pass mutable references through composed scans, without interior
mutability or cursor heap allocation. The inline arrays add four bytes per fixed
target and 64 bytes per dynamic predicate, before struct padding. Cursor hints
remain valid after clipping, skipped groups, and reversal. Query-only preparation
still encodes column targets without preparing symbol-index traversal. Slab bytes
and the default backend are unchanged.

Thirty all-feature tests pass: seven library tests, four query-execution tests,
two scan-pattern tests, and seventeen shared scanning tests. The cursor test
compares ascending, descending, and shuffled seeks against an independent linear
partition, including empty lists, padding, and arbitrary initial hints. Shared
pipeline tests cover clipping, subtrees, filter order, partial consumption,
reversal, and disabled indexes. All seven library and seventeen scanning tests
also pass with 32-slot groups and 64-byte column alignment. Sanitizers and
cross-target checks were not repeated for this scan-only update.

Artifacts are in
[`build/rust-core-comparison/sparse-cursors`](build/rust-core-comparison/sparse-cursors).
The before/after patches are relative to `a49db511d`; the pre-cursor build includes
the first refresh above. The reference executable includes the new iteration
cursor implementation. All three use the updated benchmark and input manifest.

The focused pilot uses the same eleven files and 28,299 nodes on CPU 2, with AC
reported offline, default build flags, and seven samples targeting 30 ms each.
It selects 21 workloads covering forward/reverse kind scans, counts, folds,
ranges, fields, intersections, and unfiltered controls. Builds finish before
timing. Each profile brackets both Rust candidates with two reference processes;
Rust process order alternates across profiles. Input metadata, slab sizes, and
input/output counts agree across all twelve reports.

| Selection | New / pre-cursor Rust median | New / current reference median | Reference-repeat ratio range |
|---|---:|---:|---:|
| Frequent | 1.001 | 0.835 | 0.938–1.031 |
| Sparse | 0.791 | 0.770 | 0.991–1.051 |
| Frequent, symbol index disabled | 1.010 | 0.846 | 0.968–1.097 |

These are elapsed-time ratios across the selected workloads. Sparse four-ID
array counts take 0.735 times the previous Rust time, reverse array enumeration
0.652, dynamic four-ID counts 0.786, and eight-ID intersections 0.739. The larger
cursor state still warrants checking dense consumers: common dynamic four-ID
folds and some range counts are slower in this pilot. These local measurements
do not reproduce iteration's cloud matrix or establish the promotion gate.

Focused repeats use nine samples targeting 60 ms on four suspected outliers
(`focus-summary.json`). Common-selection reference-repeat ratios span
0.812–0.844, so that repeat is inconclusive. With the index disabled, reference
repeats differ by less than 1%: range counts remain 4–5% slower than pre-cursor
Rust and singleton array enumeration remains 6% slower. Dynamic four-ID folds
do not repeat the earlier slowdown against pre-cursor Rust, but take 1.016 times
the current reference's time. Retain these as open dense-scan costs rather than
trading them against the sparse gains. Separating cursor storage from dense
predicate state remains a follow-up, as noted in iteration's own findings.

## Flat-predicate refresh

The candidate carries over `a1392880a` and `9fc350ea9` in `b5842d2aa`, with
follow-ups `4e67e1080` and `89e9f0622`. Reference `main` at `9a3f0292c` also includes
iteration's profiling notes from `dae674ea2`; its production code matches
`7a5dc4a93`. Flat scans use comparison-only predicate views; fixed-size views copy
their encoded IDs, while composed counts move comparison state into the flat
loop. Posting hints initialize only when index preparation selects indexed
traversal. Dynamic singleton selections use one hint; larger selections retain
the bounded sixteen-hint cache. The enum and option
payloads still reserve inline storage, so this does not claim smaller owning
predicates.

Standalone counts choose singleton, two-ID, and four-ID kernels before visiting
groups. `Filtered::count` forwards directly so the default `Identity` wrapper
cannot hide that specialization, as described in iteration's latest profiling
notes. Three targets duplicate one ID in the four-ID kernel. The candidate keeps
its cached singleton column parameters and query-only `prepare_columns` path.
`IdSet::intersection` matches the reference API and filters the smaller set through
the other's membership bitmap. Slab layout and the default backend are unchanged.

Thirty-one all-feature tests pass: seven library tests, four query-execution
tests, two scan-pattern tests, and eighteen shared scanning tests. They cover the
new ID-set intersections, empty and invalid selections, size-specialized counts,
filter composition, ranges, partial consumption, reversal, and disabled indexes.
Formatting and whitespace checks pass. Alternate slab layouts, sanitizers, and
cross-target builds were not repeated for this update.

Artifacts are in
[`build/rust-core-comparison/flat-predicates`](build/rust-core-comparison/flat-predicates).
`final/` contains the `89e9f0622` executable, source and binary hashes, patch,
raw reports, tests, and build log. The reference executable was built at
`7a5dc4a93`; the subsequent merge adds only profiling notes. The old Rust
executable is the retained `b3baaf1fb` sparse-cursor build, whose Rust sources
match the pre-port `7fbf135ef`. Intermediate builds and assembly diagnostics
remain in the parent directory and `comparison-only/`.

The pilot uses the same eleven inputs and 28,299 nodes, CPU 2, AC online, default
build flags and slab layout, and seven samples targeting 30 ms each. Twenty-four
workloads cover counts, forward/reverse enumeration, folds, ranges, fields,
intersections, and unfiltered controls. Reference runs bracket the two Rust
processes; Rust order alternates between profiles. Builds and tests finish before
timing. Input metadata, grammar hashes, and input/output counts agree across
backends. The intersection workloads compose filters; they do not measure
construction of the new reusable `IdSet` intersection.

| Selection | Final / old Rust median | Final / reference median | Reference-repeat ratio range |
|---|---:|---:|---:|
| Frequent | 1.012 | 0.852 | 0.994–1.019 |
| Sparse | 1.010 | 0.811 | 0.986–1.038 |
| Frequent, symbol index disabled | 0.996 | 0.846 | 0.967–1.015 |

These elapsed-time ratios are medians across selected workloads. Frequent
dynamic one- and two-kind counts take 0.919 and 0.932 times the old Rust time;
without the index, 0.914 and 0.918. Sparse eight-kind counts and sixteen-kind
folds also improve. These gains do not offset the remaining regressions:

- Frequent fixed four- and sixteen-kind counts take about 1.16 times the old
  Rust time; without the index, 1.13–1.14. Both remain faster than the reference.
- Frequent fixed four-kind reverse enumeration takes 1.44–1.45 times the old
  Rust time, with and without the index. Dynamic four-kind forward enumeration
  takes 1.11–1.15 times the old Rust time and about 1.10 times the reference time.
- Sparse dynamic one- and two-kind counts take 1.14 and 1.08 times the old Rust
  time, while remaining faster than the reference.
- Unfiltered reverse folds take 1.48–1.51 times the old Rust time and 1.53–1.56
  times the reference time. Forward folds are close to the old Rust build.

Focused repeats use nine samples targeting 60 ms, with process order
old/new/new/old (`final/focus-summary.json`). Without the index, fixed four- and
sixteen-kind counts take 1.146 and 1.150 times the old Rust time, fixed four-kind
reverse enumeration 1.411, and the unfiltered reverse fold 1.523. Dynamic one-
and two-kind counts take 0.921 and 0.938. Sparse dynamic one- and two-kind counts
repeat at 1.140 and 1.095. Process repeats differ by at most 2.4% for these cases.
Dynamic four-kind enumeration is inconclusive in this repeat: the old binary's
throughput repeat ratio is 0.851, so the near-equal aggregate ratio does not clear
the pilot regression.

The initial port's fixed four-kind reverse slowdown repeated at 2.45 times the
old Rust time. Copying its comparison record reduced the regression, but did not
remove it. Unfiltered fold code is sensitive to placement: final forward/reverse
hot functions have the same 85/91 instructions as the old Rust build after
normalizing addresses and relocations. An initial-port diagnostic with
`-C llvm-args=-align-all-blocks=5` removed the forward-fold slowdown but left
other regressions. Production flags are unchanged; this is evidence of layout
sensitivity, not a portable fix. The performance acceptance gate remains open.

## Query execution

The paired probe shares source, grammar library, and mainline input tree. Each
core owns its grammar, slab, query, and cursor. It compares slab bytes and complete
matches before timing. Timings include cursor construction, allocations, complete
stream consumption, and destruction. Backend order alternates across samples.
Capture streaming uses the existing coverage contract rather than requiring
identical provisional streams.

At `fd3835b0b`, all 240 query/workload combinations pass with and without query
optimizations. Nine samples per combination target 30 ms of inner-loop work.
Ratios below are Rust elapsed time divided by C elapsed time; smaller is faster.

| Run | Median ratio across combinations | Slowest ratio |
|---|---:|---:|
| Optimized (`sorted.json`) | 0.648 | 1.009 |
| Unoptimized (`sorted-unoptimized.json`) | 0.679 | 1.000 |
| C/C noise control (`sorted-baseline.json`) | 1.000 | 1.041 |

These medians describe the selected query cases, not aggregate application
throughput. The slowest optimized case is HTML brackets in match mode. Separate
process repeats for that case give 0.999, 1.015, and 0.987; HTML indents gives
1.004, 0.994, and 1.001 (`separate-sorted-*`).

After scan layout cleanup, all 240 comparisons also pass with stored points off
and with presence indexes off (`layout-no-points.json`,
`layout-no-presence.json`). A C++ outline outlier in `layout-control.json` has
strong within-run timing drift. Its focused rerun gives 0.815 for matches and
0.797 for captures (`cpp-outline-rerun.json`); the drifting run is inconclusive.

At `b69d19e92`, 24 further comparisons pass on Python's `dict_huge.py` (1.1 MB),
C++'s `entt.hpp` (3.6 MB), and TypeScript's `lib.dom.d.ts` (2.3 MB). These cover
brackets, highlights, indents, and outline queries in both consumption modes.
Seven samples target 40 ms each; the median paired ratio is 0.791, with a slowest
case of 0.915 (`large-query-final.json`). They extend size coverage but are still
one paired process run, not the final acceptance matrix.

Assembly and `perf` guided changes to state deduplication, borrowed step access,
capture-list access, early-exit sorting, and allocation/heap setup. Query mutation
now updates only affected indexes and root masks. Capture removal retains valid
plans and native descriptors. The initial full rebuild caused large disabling
slowdowns; it is gone. Mutation timings are sub-microsecond for several small
queries and need longer batched comparisons before acceptance.

## Scans and packing

Scan groups now borrow the existing tree descriptor and layout. They no longer
copy a second layout and grammar view. `NonNull` records the live-tree invariant
and shrinks `Option<Node>` from 24 to 16 bytes on x86_64; assembly confirms that
`next_preorder` no longer returns through a stack pointer.

The 89-workload scanning pilot has a median candidate/reference ratio of 0.853
after that cleanup (`scan-nonnull.json` versus `scan-reference.json`). Focused
repeats confirm reverse preorder folds at 1.344 and single-kind node iteration
at 1.123 (`reverse-fold-*`).

The slab's first column has a fixed offset after its aligned header. Using that
constant avoids a descriptor load without making scan state larger. Two focused
runs with this change reduce reverse-fold ratios to 1.02–1.04 and single-kind
iteration to 1.03–1.04, while forward folds remain slower at 1.06–1.08
(`scan-offset-*`). Caching additional views or changing inlining did not give a
consistent improvement and was not retained. This is not a scanning-suite
acceptance result.

The complete 89-workload rerun at `b69d19e92` has median ratio 0.839
(`scan-final-final.json` versus `scan-final-reference.json`). Forward folds remain
at 1.074, single-kind iteration at 1.034, and reverse folds at 1.020. Multi-kind
counting gives another 1.087 outlier in this run. Those cases remain open.

Three independent experiments remain disabled by default:

| Feature | Replacement | Observed limitation |
|---|---|---|
| `typed-query-scan` | Small root unions use typed kind masks. | Some queries improve, others regress relative to the Rust control; remeasured after layout cleanup in `layout-root.json`. |
| `typed-presence-scan` | Bounded descendant kind/field checks share scan kernels. | Mixed results versus the control (`layout-presence.json`); retain the existing budgets, cache, and cooldown. |
| `typed-seek` | Coordinate masks inside indexed descendant lookup. | Point seeks improve in several grammars, but byte seeks consistently regress (`seek-typed-seek-*`). |

The seek experiment retains binary search, immediate candidate return, empty-node
tie handling, and the distant point-search fallback. It does not replace singular
lookup with a full containment scan. Direct-plan fusion and sibling-mask searches
have not been justified by profiles and remain deferred experiments.

Isolated packing profiles identified register spilling in the encoder. Borrowing
events and updating group bounds in place improves the preceding Rust encoder
by about 3–5%. The first focused rerun spans 0.977–1.023 versus C
(`pack-borrow-*`). Separate repeats show substantial host timing variation;
packing is not yet a settled acceptance result. Shallow subtree boundaries now
stay inline to avoid repeated allocation growth after `trim`.

`core-lifecycle-bench` separately covers cold/reused/trimmed conversion,
full/safety/borrowed/backed loading, compact copying, repacking, grammar creation
and cache loading, query creation, destruction, and disabling. The initial
`lifecycle-*` results exposed the mutation and metadata-allocation costs fixed
since that run. Small-grammar preparation, some safety-only loads, and query
destruction still need controlled follow-up; improvements elsewhere do not
cancel those cases.

Separate-process pressure pilots cover all eight existing workloads and 11 inputs
with zero comparison failures. Each profile has one process pair, seven repeats,
and 25 inner iterations for navigation. Runs alternate backend order between
profiles, pin the benchmark to CPU 2, and use 16 MiB of pressure data. The tenant
runs on sibling CPU 3 at 10% duty. Median Rust/C thread-CPU ratios across inputs:

| Profile | Cold parse | Warm parse | Cursor | Scan | Byte seek | Point seek | Query matches | Query captures |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| None | 1.000 | 1.029 | 0.327 | 0.634 | 0.890 | 0.874 | 0.800 | 0.804 |
| Wash | 0.980 | 1.024 | 0.323 | 0.648 | 0.762 | 0.885 | 0.823 | 0.802 |
| Carousel | 0.983 | 1.004 | 0.332 | 0.653 | 0.884 | 0.862 | 0.803 | 0.794 |
| Tenant | 0.977 | 1.004 | 0.331 | 0.737 | 0.864 | 0.876 | 0.798 | 0.784 |

These pilots are not independent process repetitions of each profile. Warm parse
has outliers of 1.264 for bash without pressure, 1.660 for YAML under wash, and
1.110 for Go under carousel (`pressure-*`). They need focused repeats before
acceptance; the medians do not settle packing or parser performance.

## Allocation measurements

Heaptrack runs use separately linked executables and equal operation counts.
Artifact hashing streams through a stack buffer, so binary/grammar file sizes
do not dominate the heap peak. Earlier `heap-*` profiles predate this correction;
use `memory-*` and `inline-*` profiles for memory comparisons.

For the selected C packing input, both processes peak at approximately 1.68 MB,
including setup, grammar preparation, and retained native input. HTML query
construction peaks at approximately 748 KB, and holding up to 16 programs for
destruction peaks at approximately 808 KB, with the candidate slightly smaller.
These are process peaks, not isolated scratch maxima.

Allocation counts still expose differences: after the inline-boundary change,
64 C packing operations after `trim` give 7,074 process allocation calls for the
reference and 7,143 for the candidate, including common setup. Query creation
also has additional allocation calls. Retained capacities and larger capture
histories need separate accounting; these small profiles do not establish the
memory gate. Both profiles report the same remaining process-lifetime allocations.

## Correctness and portability

- Byte-for-byte packing and cross-loading against C, including deep/wide/error
  trees, both point modes, presence options, repacking, borrowed/backed storage,
  and direct parsing.
- Navigation, byte/point seeks, typed scan predicates/consumers, query metadata,
  optimized/general execution, disabling, ranges, cancellation, limits, cursor
  reuse, and mixed match/capture consumption.
- Original Rust binding tests and persistence tests against the candidate,
  including compile-fail lifetime checks.
- The mutated 44-file query corpus passes both implementations with zero
  comparison failures (`mutated-small-*`). The original 53-file corpus has the
  same six failing file/workload rows in both implementations; the first failure
  is a mainline Tree-sitter timeout on TypeScript injections. The other failure
  reasons were not retained individually and must not all be called timeouts.
- Rust plus native ASan passes binding, storage, navigation, and query tests with
  all scan experiments enabled, including the latest preparation/allocation
  changes (`asan-final.log`).
- i686 Linux-musl storage, navigation, and query comparisons pass with all
  experiments enabled (`cross-i686.log`). Both C and Rust compile for big-endian
  PowerPC64, but execution is unverified: the available Zig GNU target rejected
  static libc linking (`cross-ppc64.log`).
- Storage, navigation, query, and scanning comparisons also pass with 32-slot
  groups and 64-byte column alignment (`layout-final.log`).

`cargo xtask squat test quick` runs the reference and candidate Rust checks and
the unchanged native checks. `cargo xtask squat test sanitize` currently covers
the native reference. Candidate ASan was run separately, instrumenting its Rust
code and native bridge; that distinction matters when interpreting sanitizer
coverage. Miri and big-endian runtime comparisons remain open.

## Before promotion

Keep the reference default until the design's full acceptance matrix passes on
immutable snapshots. In particular:

- Repeat the individual scan, lifecycle, and query-mutation outliers with C/C
  controls; extend independent process repetitions of the large-input and
  pressure comparisons.
- Resolve the flat-predicate refresh's repeated count, reverse-enumeration, and
  unfiltered-fold regressions; retain the specialized dispatch and test across
  binary layouts rather than accepting a favorable workload median.
- Complete retained-scratch/capacity accounting, parser-memory and large capture
  histories, and compare allocation counts independently of timing.
- Resolve the corpus failures individually and complete supported configuration
  and endian checks. Re-run sanitizer coverage after any ownership changes.
- Preserve the independent control paths for all three scan experiments.

No overall no-regression claim or default-backend switch is implied by the
selected query gains.
