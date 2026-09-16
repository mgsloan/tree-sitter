# Public display IDs with sparse grammar IDs

The dense u16 symbol column stores public display IDs. Sparse u16 overrides
preserve original grammar IDs whenever they differ, including aliases and
public-symbol canonicalization. Display reads and presence-index construction
no longer map IDs. Query filters use public IDs directly, and required-symbol
presence checks need one symbol rather than a set of raw equivalents.

## Measurements

Local comparison on 2026-09-16 against `2f14fd80b`, Intel Core Ultra 7 165U,
Linux x86-64. Release `squatter-bench`, CPU 0, isolated cache mode, three
alternating baseline/candidate passes with three repeats per pass. Each ratio
compares the median of pass medians per file; the table gives geometric means
across files. Negative changes mean less time or fewer instructions.

Traversal: 39 staged corpus files, 317,464 source bytes, capped at 128 KiB per
file; 30 traversals per measurement. Queries: 20 files capped at 16,000 bytes,
at most two per grammar, using each grammar's upstream highlights query.
Setup parsing includes parsing and packing with a reused context; it excludes
grammar preparation. Trees retain spare capacity, points, and the presence index.

| Workload | Files | Time change | Instruction change |
| --- | ---: | ---: | ---: |
| Cursor attribute digest | 39 | +0.44% | +0.47% |
| Cached iterator digest | 39 | -0.45% | +0.27% |
| Cursor attribute scan | 39 | +0.60% | +0.50% |
| Cached iterator scan | 39 | +0.41% | +0.28% |
| Query matches | 20 | -5.74% | -0.99% |
| Query captures | 20 | -1.10% | -0.98% |
| Parse + pack | 39 | +0.13% | +0.34% |

Traversal is effectively unchanged. Query instruction counts improve modestly;
the larger match timing improvement needs confirmation on other workloads.
Mainline controls changed by -1.0% to +0.4% in traversal/query time; setup parsing
was noisier at -4.6%. These small local samples do not establish a general
performance win, particularly under cache pressure.

## Override cardinality and storage

The native layout probe covers all 48 staged files (20,417,561 source bytes,
5,634,993 nodes), including large files excluded from timing comparisons.
Compacted slab bytes increase from 111,359,008 to 111,812,776 (**+0.41%**).
Overrides increase from 129,698 to 332,026 nodes (**2.30% to 5.89%**).
The sparse section grows from 772,336 to 1,226,104 bytes.

| Grammar | Nodes overridden | Distinct override IDs across files | Maximum distinct IDs in one file | Slab growth |
| --- | ---: | ---: | ---: | ---: |
| Bash | 8.1% | 13 | 11 | +0.62% |
| C | 6.8% | 7 | 6 | +0.50% |
| C++ | 20.1% | 11 | 10 | +0.80% |
| CSS | 13.6% | 5 | 5 | +0.17% |
| Go | 9.8% | 5 | 4 | +0.07% |
| HTML | 11.7% | 4 | 4 | +2.42% |
| JSON | 0% | 0 | 0 | 0% |
| Python | 0% | 0 | 0 | 0% |
| TSX | 3.4% | 14 | 14 | +0.15% |
| TypeScript | 13.1% | 17 | 15 | +0.17% |
| YAML | 73.4% | 40 | 32 | +8.11% |

Distinct-value cardinality is low in this sample, even where overrides are
frequent. A per-tree dictionary could encode all observed override values with
at most five bits, or one byte for simpler reads, plus the dictionary. This is
an opportunity to measure separately, not a bound guaranteed by the format.
The current representation keeps direct u16 values. YAML deserves particular
attention: its overrides are frequent despite their low distinct-value count.
JSON and Python dominate node totals, so the aggregate storage growth hides
substantial language variation.

The native probe's seven-sample median packing time increased 2.7% by geometric
mean across files. That single unpinned pass uses one-shot packing with compaction;
it is diagnostic and differs from the repeated context-reusing benchmark above.

## Reproduction and validation

Raw results, relocated grammar registries, selected paths, binaries, layout
histograms, and local reproduction scripts are in `build/public-display/`:
`bench.py` runs the alternating measurements; `summarize.py` computes the table.
The input registry comes from `build/squat-nonquery-final/`, with upstream
queries from `build/squat-query-corpus/`. Run manifests record input/grammar
hashes and measurement settings. The incomplete `baseline` run is an abandoned
pilot and is excluded from the summary.

Build each revision with `cargo build --release -p squatter-bench` and preserve
`target/release/squatter-bench`. For storage, build `layout-bench` with the native
Makefile at `-O2`; pass a grammar library, its export name, and the source paths
listed in `layout-inputs.json`. The probe reports distinct override IDs per file
on stdout and aggregate ID frequencies on stderr. Older raw CSVs call the
node-override count `aliases`; its current name is `grammar_overrides`.

Native unit/supertype tests and node/cursor/query comparisons passed across ten
grammars, including persistence, malformed slabs, errors, aliases, and query
optimization modes. Benchmarks reported zero comparison failures. Comparisons
also assert that stored display IDs equal Tree-sitter's public IDs, and loaders
reject noncanonical display IDs.
