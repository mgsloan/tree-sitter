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

The reference is `0c3f79ab5`, including the merged iteration work. `main` at
`83d962cdd` differs only in `todo.md`. The reference's production sources have
not been edited. Both cores use one resolved Tree-sitter runtime and disjoint
Squatter/tree-feller native symbols.

Measurements below used an Intel Core Ultra 7 165U, plugged in, Linux x86_64,
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
