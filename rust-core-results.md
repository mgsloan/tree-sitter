# Rust core implementation and comparisons

The candidate implements storage, packing, navigation, typed scans, and query
execution in Rust. C retains grammar preparation, private Tree-sitter traversal,
tree-feller, and query compilation. Compiled query records stay in their native
allocation; Rust borrows them and releases the owner on drop. Release execution
trusts compiler output; debug builds validate it.

Public Rust signatures are unchanged. `tree-squatter-rust` is an unpublished
comparison crate. The default remains C-backed `tree-squatter`, and the C facade
is deferred. Persistence and benchmarks can select the candidate with their
`rust-core` feature. There is no native query execution fallback.

## Baseline and measurement scope

The original measurements use reference `0c3f79ab5`; `83d962cdd` differs only in
`todo.md`. The current reference is `main` at `51ecbfafb`, which merges `iteration`
through `9cf1cdb35`. The first iteration refresh below uses `8bb827f73` /
`29a192f92`; the sparse-cursor refresh uses the current reference. Earlier ratios
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

The candidate now includes the scan implementation through `46828abdd`, with
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
- Complete retained-scratch/capacity accounting, parser-memory and large capture
  histories, and compare allocation counts independently of timing.
- Resolve the corpus failures individually and complete supported configuration
  and endian checks. Re-run sanitizer coverage after any ownership changes.
- Preserve the independent control paths for all three scan experiments.

No overall no-regression claim or default-backend switch is implied by the
selected query gains.
