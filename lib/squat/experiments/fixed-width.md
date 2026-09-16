# Fixed-width layout

The opt-in `SQ_FIXED_WIDTH=1` build uses 16 bits for symbols, grammar-symbol
overrides, fields, supertypes, and group waste. Empty field and supertype columns
remain allocated. Coordinates and boolean flags keep their existing widths.
The Cargo equivalent is `tree-sitter-squatter/fixed-width`.

## Measurement

Local measurements on 2026-09-15, Intel Core Ultra 7 165U, Linux x86-64,
benchmark thread pinned to CPU 0, with no deliberate cache pressure. Both builds
use the same working-tree sources based on `06c1764ce`, Cargo release defaults,
and the same grammar libraries and inputs. Hardware counters were available.

Traversal: 33 files, three per grammar, 3,608,690 source bytes total. Grammars:
Bash, C, C++, CSS, Go, HTML, JSON, Python, TSX, TypeScript, YAML. Files were
selected deterministically from the existing staged corpus, closest to 5 KB,
50 KB, and 300 KB per grammar (actual range 4,665–450,924 bytes). Queries use the
22 files below 60 KB and the registry's query suite. Larger query pilots were
interrupted after identifying expensive mainline capture comparison; those
partial runs are excluded.

Each layout runs three passes of three repeats, alternating layout order
between passes. Allocation-free digest/scan measurements perform ten traversals
per sample. Ratios compare fixed/default medians per file, then take the
geometric mean across files; lower is faster. Each pass records its median of
three repeats, and the comparison uses the median of those three pass medians.
Normal benchmark runs reuse grammar and pack contexts, retain spare slab
capacity, and include points and the presence index. Setup parsing includes
parsing plus packing and excludes grammar preparation.

Raw results, source and grammar hashes, commands, input selection, and local
reproduction scripts are in `build/benchmark/`. Run `run.py` for traversal,
`queries.py` for queries, `layout.py` for compacted packing/layout diagnostics,
`seek.py` for seeks, and `summarize.py` for the cross-build summary. The
`default-0` and `perf-default-0` files are interrupted pilot artifacts.

## Results

Fixed width trades roughly a quarter more slab storage for small speed gains on
this workload. Uncached attribute walks benefit most; cached iterators and queries
improve only slightly. This is a local isolated-cache result, not evidence that
the larger representation wins under memory pressure.

| Workload | Files | Time change | Instruction change |
| --- | ---: | ---: | ---: |
| Cursor navigation | 33 | -2.7% | -2.1% |
| Uncached attribute digest | 33 | -4.2% | -6.5% |
| Cached iterator digest | 33 | -1.0% | -1.2% |
| Uncached attribute scan | 33 | -4.1% | -6.8% |
| Cached iterator scan | 33 | -1.0% | -1.3% |
| Query matches | 22 | -1.9% | -2.1% |
| Query captures | 22 | -1.4% | -1.7% |
| Parse + pack (reused context) | 33 | -1.3% | -0.5% |

Total retained slab bytes increased from **14,263,512 to 17,787,672 (+24.7%)**.
Per-grammar increases range from 15.9% (C++) to 43.9% (HTML); grammars with
few or no fields/supertypes pay most for the newly allocated 16-bit columns.

The native layout probe (`-O2`, seven packing samples per file, median; grammar
preparation excluded, compaction enabled) measured **4.2% less packing time**
by geometric mean, or 2.2% less summed time. Its compacted slab totals grew
23.7%. This single native pass is diagnostic evidence, less robust than the
repeated top-level timings.

Separate seek runs (three repeats, 30 non-CSS files) measured 3.5% less byte-seek
time and 2.5% less point-seek time, with no comparison failures.

Changes around 1–2% should be treated as small on this shared laptop. There is
no broad speedup that compensates for the size increase in every workload.
Compiler versions: GCC 15.3.0, rustc/Cargo 1.95.0.

Build the benchmark pair with:

```sh
cargo build --release -p squatter-bench
cp target/release/squatter-bench build/benchmark/default-bench
cargo build --release -p squatter-bench --features tree-sitter-squatter/fixed-width
cp target/release/squatter-bench build/benchmark/fixed-bench
```

## Validation

Both native `make check` configurations pass, including width boundaries,
empty columns, supertype dictionary overflow, growth/compaction, and persistence.
All four loaders reject oversized 16-bit group waste before reading nodes.
Fixed-width ASan/UBSan checks pass. Both Cargo feature configurations pass the
Rust tests and doctests. Slabs from the other layout are explicitly rejected.
Native structural comparisons pass for one small source per grammar with
`SQ_SKIP_EDGE_CASES=1`. The generic edge-case suite aborts in both builds at
`compare.c:422` (`safety && error == SQ_OK`) when parsing its JSON sample with
the Bash grammar; this existing failure is recorded in `edge-cases-*.log`.
The benchmark executables predate the final load-only waste validation and
error-message changes; neither change is on a measured code path.

The initial default-layout pilot found byte/point seek differences in
`bootswatch/dist/vapor/bootstrap.css`. Seek timing therefore runs separately
without CSS; it is not silently accepted in the main comparison.

## Further optimization

The fixed-width path now stores IDs directly instead of updating a packed word
and advancing a bit cursor. Each packing cursor shrinks from 24 to 8 bytes.
Cached iterators read the slab's IDs directly: their unpack function pointer,
field-fill state, and symbol/field arrays disappear. With the default 16-slot
window on x86-64, the cache shrinks from 472 to 400 bytes (72 bytes, 15.3%).
Point expansion reads 16-bit keys directly, avoiding a temporary buffer and an
indirect decoder call; its output cannot alias the immutable slab. The point
simplification also applies to the variable-width build.

Group equality compares 16 halfwords at once with baseline x86-64 SSE2, producing
the final lane bitmap without iterating matching bits. Other targets use a
portable halfword comparison loop. No slab encoding or width limit changes.

The follow-up uses the previous fixed-width implementation as its baseline,
with the same machine, input set, affinity, and comparison contracts as above.
Raw results and commands are in `build/fixed-optimization/`. The initial combined
comparison has three alternating passes of three repeats: queries took 2.0–2.4%
less time and executed 1.7–2.0% fewer instructions. A second query comparison
measured 3.2–5.4% less time with the same instruction reduction. Timing differences
in unchanged controls show that small wall-time changes remain noisy here.

The native packing probe (seven samples per file, compacting) measured 5.3% less
time by per-file geometric mean, or 6.4% less summed time. Every file retained
exactly the same slab size. This is diagnostic evidence from one native pass.

The direct point expansion initially increased instruction counts; adding the
valid non-aliasing annotation brought them back to the buffered version's level.
Its timing differences went in both directions, so no separate speedup is
claimed for removing that buffer.

The final cache comparison uses two passes of three repeats in before/final,
final/before order, with thirty traversals per sample across all 33 files:

| Cached workload | Time change | Instruction change |
| --- | ---: | ---: |
| Attribute digest | -0.9% | -1.1% |
| Attribute scan | -0.9% | -1.1% |

These are modest CPU gains, alongside the definite 72-byte cache reduction.
The final executable is `restrict-bench`; `confirmed-*-files.jsonl` and
`confirmed-summary.json` contain this comparison. The earlier `after-bench`
is the buffered prototype, and `final-bench` is the intermediate direct-read
version without the non-aliasing annotation.

Native checks cover default/fixed builds and a 64-slot fixed build with cache
mode 0. Added equality cases exercise high-bit values, no/all/alternating matches,
every group-waste value, and invalid groups/targets. Fixed-width sanitizer tests,
Rust tests for both feature configurations, and the eleven native structural
comparisons pass, with the previously documented edge-case exclusion.
