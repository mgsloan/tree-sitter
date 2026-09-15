# Independent power-of-two column widths — 2026-09-14

This matrix compares each observed source width with larger widths in
{2, 4, 8, 16}, changing only one column at a time. Highlighting/tags query
execution and full tree walks are the primary measurements. Results are
collected on GCP; no production width policy changes are part of this experiment.

## Findings and policy choices

Rounding an awkward width often helps; widening an already convenient width
is not a general speed win. Keep 1-, 2-, and 4-bit fields as the space-oriented
default. Their highlighting changes are generally below 1%, and widening can
slow scalar walks. Field 1→8, for example, changes points-enabled highlighting
by +0.1%, cursor walks by +2.9%, and cached walks by −2.7%, while adding 7.2%
to the slab. This is a workload tradeoff, not free speed.

Field 3→4 is a useful inexpensive candidate: BibTeX highlighting improves
about 2%, cursor walks 4.4–5.3%, and cached walks 2.4–3.2%, for 1.0% / 1.8%
more slab space (points / byte-only). It is not a uniform win: points-enabled
BibTeX tags slow 2.4%; byte-only tags change +0.4%. Field 3→8 improves cached
walks more, but adds 5.3% / 9.2% space. One grammar does not establish a universal
3-bit cutoff.

Field 5→8, 6→8, and 7→8 offer more consistent query/walk gains at modest space
cost. The expanded corpus makes 5→8 more attractive than the earlier Bash-only
measurement. Prefer 8 over 16 for these widths unless a particular workload
justifies the much larger slab: the extra speed is usually small or absent.

For supertype masks, 3→4 reduces membership-walk time by **21.8% / 22.3%** for
only **0.9% / 1.5%** extra slab space. Going straight to 8 improves membership
25.7% / 25.4%, but costs 4.7% / 7.7% space. This is strong support for a 4-bit
representation at three supertypes. Counts 5–7→8 similarly reduce membership
work by roughly 23–27% with little effect on ordinary highlighting/tags.
Keep 1-bit masks compact. For 2→4, membership improves 4.6% with points but
slows 7.4% without points; that is not a general reason to widen. For 4→8,
membership improves 6.5% / 1.7% at a 3.5% / 5.5% slab cost, making it a
membership-heavy-workload option rather than an obvious default.

Symbol 5→8, 6→8, and 7→8 have clear traversal benefits. Symbol 8→16 adds
7.8% / 12.6% slab space and slows highlighting 0.9% / 1.4%; retain 8 bits by
default. The 9→16, 10→16, and 11→16 transitions are useful speed/space options.
The newly covered SystemVerilog 11→16 case improves highlighting 3.2% / 2.7%,
uncached walks 12.5% / 11.7%, and cached walks 5.2% / 6.8%, for only 1.8% /
2.4% more slab space. This makes 11→16 an attractive measured option. Width 12
has the same five-values-per-word storage density as 11, but remains unmeasured.
Widths 13, 14, and 15→16 have no column-space penalty; their query/walk benefits
remain unmeasured.

These recommendations apply to the shared flexible-width reader below. They do
not claim optimal dispatch or SIMD kernels, nor a universal workload weighting.

## Baseline and scope

The frozen runtime revision is `ab6162834`. Every build has the same experimental
empty-column omission and exact-width supertype storage. Field widths use the
actual maximum field ID, allowing one-bit fields. All builds use the same
specialized byte extraction for 1-, 2-, and 4-bit values on this little-endian
host. Consequently this experiment compares widths with suitable accessors,
including 3-bit word packing versus 4-bit byte extraction. It does not compare
against the production runtime's byte-wide supertype baseline.

The shared getter dispatches among supported widths. Disassembly confirms
several width comparisons before the 8-bit supertype load, whereas the older
fixed-byte baseline has a simpler getter. Thus widening within this matrix
is not equivalent to removing flexible-width support. These are measured
policies for this implementation, not upper bounds on native-width performance.
The earlier supertype report measures the separate cost of adding that support.

Group equality retains the baseline SWAR scanner, including its per-hit lane
index calculation. The separate [PEXT/cache probes](kernel-probes-results-2026-09-14.md)
are not combined into this matrix. Faster group-mask extraction or retained
decoding could change the balance, especially for narrow columns; scalar byte
extraction alone does not establish an optimal query implementation.

The common experimental layout adds eight bytes of runtime metadata per tree.
Slab percentages exclude that common metadata. Packing, parsing, and query
compilation are outside the query timer. Query execution includes text predicates
and ordered result consumption. Full walks include cursor, uncached attribute,
cached attribute, and supertype-membership walks.

The original matrix covers 62 real query jobs and 40 walk jobs in 21 grammars. A target
policy can affect several source widths, but reporting separates them: e.g.
field4 yields distinct 1→4, 2→4, and 3→4 rows. Each row includes only affected
grammars, and each column is varied independently. Empty columns are excluded
from widening. There is no synthetic grammar substitution.

## Corpus coverage and limitations

The initial 11-grammar query corpus is supplemented with INI, Thrift, BibTeX,
SCSS, OpenSCAD, XML, GLSL, Nushell, SystemVerilog, and Java from
`../../code-corpora`. Grammar parser/scanner files, compiled libraries, queries,
and selected source files are hashed in the audit data.

This gives field-ID widths 1–7; direct supertype masks of widths 1–8; and symbol
widths 5–11. No measured claim is made for 1–4-bit symbols, 8–15-bit field IDs,
or 12–15-bit symbols. This matrix concerns direct supertype masks; the earlier
[column report](column-probes-results-2026-09-14.md) separately covers C#/SML
dictionary IDs.

For the added grammars, select from files between 1 KiB and 2 MiB, testing up to
the largest 100 candidates per language for error-free parsing. Choose a batch
of three spread across that clean subset and its largest file as a second job.
The labels "small" and "large" are relative within a grammar; they are not
fixed byte-size classes. These additions favor larger available files and do
not constitute a random sample of the corpus.

GLSL is particularly limited: only three candidates over 1 KiB parsed cleanly,
totaling 7,332 bytes. Its largest-file job repeats one member of its batch.
SystemVerilog contributes four distinct clean files totaling 15,144 bytes,
with a largest file of 6,687 bytes. Nushell contributes four files totaling
27,818 bytes, largest 21,259 bytes. These cases establish width coverage,
not large-file performance. Thrift has usable volume but limited repository
diversity. Larger GLSL, SystemVerilog, and Nushell projects would improve coverage.

Thrift and GLSL highlight queries use Lua-pattern predicates. The patterns used
here have direct regex equivalents: replace `#lua-match?` with `#match?` and
prefix `(?s)` to preserve Lua's dot-matches-newline semantics. Thrift/OpenSCAD
`#set! "priority"` directives are omitted; these affect editor display priority,
not capture filtering. Both query engines execute the identical adapted text.
Original and adapted hashes are retained; this is not an editor-rendering benchmark.

## Additional repositories supplied during the run

Keep the original paired jobs unchanged and add eight query/eight walk jobs
in a separate serial batch using identical binaries. The new repositories are
`godotengine--godot`, `lowRISC--ibex`, `openhwgroup--cva6`, and
`nushell--nu_scripts`. The same clean-parse selection rule yields SystemVerilog
files up to 272,242 bytes and Nushell up to 1,300,699 bytes. These additions
substantially improve those two previously limited cases.

GLSL remains constrained by grammar compatibility. Only four of the largest
100 new candidates parse cleanly, largest 8,478 bytes. Three of the largest
Godot shaders produce an ERROR spanning the complete file; they are excluded
from this clean-parse matrix. The audit includes the candidate outcomes and
an error-span diagnostic. No shader source is rewritten to make it parse.

The combined matrix has 70 query jobs and 48 walk jobs. Report the additional
large-file cases separately as well as in the grammar-balanced aggregate.

Not every real query exercises every column. INI highlighting has no explicit
field constraints; Thrift highlighting checks its `type` field. BibTeX
highlighting checks fields but its tags query does not. Near-zero query changes
are therefore not evidence that the corresponding scalar load costs are equal.
Attribute walks provide the complementary direct-access workload.

## Measurement and validation

GCP project `mgsloan-compute`, zone `us-central1-a`, instance
`squatter-benchmark`: e2-standard-2, Intel Xeon 2.20 GHz, family 6/model 79
(Broadwell), BMI2/AVX2, little-endian. Timed processes run serially on CPU 0.
Three randomized paired rounds use five calibrated CPU-time samples per variant,
with points-enabled and byte-only builds measured separately.

The identical-code `control` build accompanies every block. The predeclared
summary excludes a whole block if any control timing is outside 0.85–1.15 of
`exact`; unfiltered summaries remain available. Use median samples, then median
paired ratios across rounds; balance jobs within grammars and grammars within
each transition. Negative time percentages mean faster. Very small changes need
caution, especially on the limited corpus cases above.

All 24 generated C builds pass unit and persistence checks. All 62 final jobs
pass local baseline comparison of complete ordered query results against
Tree-sitter. GCP repeats that validation for every variant in its first round
and checks count/checksum agreement throughout. The final table generator also
asserts that actual layout widths match each policy and that other columns retain
their baseline widths.

## Reproduction

From the repository root, build with `tools/squatter/prepare-pow2-widths.py` using
`exact,control,field2,field4,field8,field16,symbol8,symbol16,super2,super4,super8,super16`
and `--points 1,0`. `prepare-pow2-corpus.py` inventories/builds additional grammars
and creates candidate jobs. Compile `lib/squat/experiments/parse-quality.c`
against the baseline runtime as `build/pow2-widths/parse-check`, then run
`refine-pow2-corpus.py` to select clean inputs and adapt queries. Generate runtime
grammar widths and validate jobs; the audit retains these commands/scripts.
`package-pow2-widths.py` creates the GCP bundle and its serial `run.sh`.

Summarize each run with `summarize-kernel-probes.py --allow-layout-changes`,
repeat with `--keep-all` for sensitivity, and run `summarize-pow2-widths.py`
for the source-to-target tables.

## Complete measured transition tables

Negative percentages mean faster or smaller; positive means slower or larger.
A dash means no corresponding query workload. Each row balances only the listed
grammars; rows with different widths need not have the same population.

### Points enabled

#### Field IDs

| Transition | Highlight | Tags | Cursor | Uncached | Cached | Membership | Slab | Grammars |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| 1→2 | -0.0% | — | +1.0% | +0.3% | +0.0% | +0.3% | +1.0% | ini, thrift |
| 1→4 | -0.1% | — | +1.4% | +0.6% | -0.5% | +0.1% | +3.1% | ini, thrift |
| 1→8 | +0.1% | — | +2.9% | +0.3% | -2.7% | +0.3% | +7.2% | ini, thrift |
| 1→16 | +0.2% | — | +1.1% | +1.0% | -2.7% | +0.0% | +15.5% | ini, thrift |
| 2→4 | +0.5% | — | +0.7% | +0.7% | +1.3% | -0.3% | +2.0% | json, xml-xml, yaml |
| 2→8 | +0.4% | — | +0.8% | -0.3% | -1.1% | -0.2% | +5.9% | json, xml-xml, yaml |
| 2→16 | +0.4% | — | +0.1% | +0.8% | -1.5% | -0.1% | +13.7% | json, xml-xml, yaml |
| 3→4 | -2.1% | +2.4% | -4.4% | -1.2% | -2.4% | +1.0% | +1.0% | bibtex |
| 3→8 | -0.9% | -0.1% | -4.0% | -1.3% | -4.7% | +0.6% | +5.3% | bibtex |
| 3→16 | -1.5% | +0.1% | -4.7% | -0.9% | -5.1% | +1.1% | +13.8% | bibtex |
| 4→8 | +0.6% | — | -0.3% | -1.0% | -0.8% | +1.2% | +3.4% | scss |
| 4→16 | +0.9% | — | -1.2% | -0.1% | -0.4% | +0.5% | +10.1% | scss |
| 5→8 | -1.0% | — | -3.5% | -3.5% | -3.4% | +0.2% | +2.1% | bash, openscad, systemverilog |
| 5→16 | -1.2% | — | -4.8% | -2.8% | -3.5% | -0.0% | +8.5% | bash, openscad, systemverilog |
| 6→8 | -0.9% | -1.4% | -3.5% | -4.3% | -4.0% | +0.1% | +1.3% | c, cpp, glsl, go, java, python, tsx, typescript |
| 6→16 | -0.5% | -1.5% | -4.7% | -3.5% | -3.9% | -0.0% | +7.7% | c, cpp, glsl, go, java, python, tsx, typescript |
| 7→8 | -1.2% | — | -3.5% | -3.3% | -5.1% | -0.1% | +0.7% | nu |
| 7→16 | -0.8% | — | -4.4% | -2.8% | -5.5% | +0.2% | +7.0% | nu |

#### Symbols

| Transition | Highlight | Tags | Cursor | Uncached | Cached | Membership | Slab | Grammars |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| 5→8 | -2.0% | — | -11.0% | -19.8% | -6.2% | -0.2% | +2.9% | ini, json |
| 5→16 | -3.1% | — | -13.5% | -21.7% | -6.7% | -0.2% | +11.7% | ini, json |
| 6→8 | -2.6% | -14.9% | -10.4% | -18.5% | -6.3% | +0.4% | +1.7% | bibtex, html |
| 6→16 | -2.1% | -16.2% | -12.9% | -21.1% | -6.3% | +0.8% | +10.4% | bibtex, html |
| 7→8 | -1.3% | — | -8.4% | -11.2% | -5.9% | -0.1% | +0.9% | openscad |
| 7→16 | -1.6% | — | -9.2% | -14.8% | -5.1% | +0.2% | +8.6% | openscad |
| 8→16 | +0.9% | -0.7% | -1.0% | -2.0% | -0.2% | +0.1% | +7.8% | css, go, thrift, xml-xml |
| 9→16 | -1.6% | -1.8% | -7.8% | -11.6% | -5.1% | +0.7% | +5.8% | bash, c, glsl, java, nu, python, scss, tsx, typescript, yaml |
| 10→16 | -3.1% | -0.8% | -7.2% | -9.9% | -4.4% | +0.0% | +4.2% | cpp |
| 11→16 | -3.2% | — | -7.8% | -12.5% | -5.2% | +0.3% | +1.8% | systemverilog |

#### Supertype masks

| Transition | Highlight | Tags | Cursor | Uncached | Cached | Membership | Slab | Grammars |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| 1→2 | -0.0% | — | +0.2% | -0.4% | -0.1% | +6.9% | +1.0% | bash, json |
| 1→4 | +0.1% | — | +0.5% | -0.2% | -0.5% | +1.7% | +2.9% | bash, json |
| 1→8 | +0.4% | — | -0.3% | -0.3% | +0.8% | +5.1% | +6.8% | bash, json |
| 1→16 | +0.1% | — | -0.0% | -0.3% | +1.2% | +5.4% | +14.5% | bash, json |
| 2→4 | -0.4% | — | -0.9% | +0.5% | +0.0% | -4.6% | +1.9% | thrift |
| 2→8 | -0.1% | — | +0.6% | -0.1% | +1.6% | -4.0% | +5.7% | thrift |
| 2→16 | -0.3% | — | +0.7% | +0.4% | +1.1% | -1.1% | +13.4% | thrift |
| 3→4 | +0.3% | — | -0.5% | -0.1% | -0.3% | -21.8% | +0.9% | openscad |
| 3→8 | -0.1% | — | -0.1% | -0.7% | +1.1% | -25.7% | +4.7% | openscad |
| 3→16 | -0.2% | — | -0.7% | -0.1% | +1.7% | -22.0% | +12.4% | openscad |
| 4→8 | +0.0% | -0.0% | -0.1% | +0.0% | +1.4% | -6.5% | +3.5% | go, python |
| 4→16 | +0.2% | +0.1% | +0.0% | -0.0% | +1.6% | -1.3% | +10.6% | go, python |
| 5→8 | +0.5% | — | -0.5% | +0.2% | +0.4% | -27.1% | +2.5% | xml-xml |
| 5→16 | -0.1% | — | -0.5% | +0.2% | -0.5% | -23.2% | +10.1% | xml-xml |
| 6→8 | +0.4% | — | -0.2% | -0.1% | +0.8% | -26.4% | +1.2% | glsl |
| 6→16 | +0.3% | — | -0.1% | -0.2% | +0.9% | -25.9% | +7.5% | glsl |
| 7→8 | +0.1% | -0.0% | -0.0% | -0.6% | +1.2% | -27.1% | +0.7% | c, cpp, tsx, typescript |
| 7→16 | +0.3% | +0.0% | -0.2% | -0.4% | +0.9% | -26.3% | +6.9% | c, cpp, tsx, typescript |
| 8→16 | +0.7% | +0.4% | -0.4% | -1.3% | +1.1% | +1.9% | +6.4% | java |

### Byte-only

#### Field IDs

| Transition | Highlight | Tags | Cursor | Uncached | Cached | Membership | Slab | Grammars |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| 1→2 | +0.1% | — | +1.1% | +1.4% | +0.2% | -0.1% | +1.8% | ini, thrift |
| 1→4 | +0.1% | — | +1.1% | +1.5% | -0.6% | +0.4% | +5.3% | ini, thrift |
| 1→8 | +0.0% | — | +1.6% | +3.2% | -2.8% | -0.1% | +12.4% | ini, thrift |
| 1→16 | +0.2% | — | +0.7% | +1.2% | -2.8% | +0.4% | +26.6% | ini, thrift |
| 2→4 | -0.1% | — | +0.6% | +0.8% | +0.4% | -0.2% | +3.2% | json, xml-xml, yaml |
| 2→8 | -0.1% | — | +0.8% | +2.1% | -2.9% | -0.4% | +9.7% | json, xml-xml, yaml |
| 2→16 | +0.1% | — | -0.1% | -2.7% | -3.0% | -0.2% | +22.7% | json, xml-xml, yaml |
| 3→4 | -2.0% | +0.4% | -5.3% | -5.9% | -3.2% | +0.1% | +1.8% | bibtex |
| 3→8 | -1.7% | +0.2% | -4.5% | -3.0% | -6.0% | -0.1% | +9.2% | bibtex |
| 3→16 | -1.6% | +0.0% | -5.5% | -6.2% | -6.5% | +0.1% | +24.1% | bibtex |
| 4→8 | +0.4% | — | +0.4% | +0.2% | -0.9% | +0.1% | +5.1% | scss |
| 4→16 | +0.0% | — | +0.2% | -0.2% | -1.2% | +0.2% | +15.3% | scss |
| 5→8 | -1.3% | — | -3.4% | -3.0% | -4.6% | -0.4% | +3.2% | bash, openscad, systemverilog |
| 5→16 | -1.3% | — | -4.2% | -4.4% | -4.7% | -0.4% | +12.7% | bash, openscad, systemverilog |
| 6→8 | -1.4% | -1.5% | -3.3% | -3.0% | -4.7% | -0.7% | +1.9% | c, cpp, glsl, go, java, python, tsx, typescript |
| 6→16 | -1.2% | -1.5% | -3.7% | -4.1% | -4.3% | -0.6% | +11.4% | c, cpp, glsl, go, java, python, tsx, typescript |
| 7→8 | -1.9% | — | -3.5% | -3.0% | -6.3% | +0.1% | +1.0% | nu |
| 7→16 | -0.9% | — | -4.6% | -4.6% | -5.7% | +0.4% | +10.3% | nu |

#### Symbols

| Transition | Highlight | Tags | Cursor | Uncached | Cached | Membership | Slab | Grammars |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| 5→8 | -2.9% | — | -11.3% | -15.6% | -6.2% | -0.0% | +5.2% | ini, json |
| 5→16 | -4.1% | — | -15.0% | -19.3% | -6.7% | -0.0% | +20.9% | ini, json |
| 6→8 | -3.4% | -20.1% | -10.4% | -17.8% | -7.6% | -0.0% | +3.0% | bibtex, html |
| 6→16 | -2.8% | -24.4% | -12.1% | -19.7% | -7.3% | +0.1% | +18.3% | bibtex, html |
| 7→8 | -1.8% | — | -8.5% | -11.9% | -6.4% | -1.2% | +1.4% | openscad |
| 7→16 | -1.7% | — | -10.1% | -12.7% | -6.4% | -0.7% | +13.9% | openscad |
| 8→16 | +1.4% | -1.5% | -1.2% | -3.2% | -0.1% | -0.1% | +12.6% | css, go, thrift, xml-xml |
| 9→16 | -2.4% | -1.3% | -7.4% | -10.9% | -6.1% | -0.2% | +8.6% | bash, c, glsl, java, nu, python, scss, tsx, typescript, yaml |
| 10→16 | -4.0% | -0.4% | -6.9% | -9.0% | -5.3% | -0.9% | +5.9% | cpp |
| 11→16 | -2.7% | — | -6.7% | -11.7% | -6.8% | +0.0% | +2.4% | systemverilog |

#### Supertype masks

| Transition | Highlight | Tags | Cursor | Uncached | Cached | Membership | Slab | Grammars |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| 1→2 | +0.2% | — | +0.3% | +0.5% | +0.1% | +6.5% | +1.6% | bash, json |
| 1→4 | +0.5% | — | +0.6% | +0.0% | +0.2% | +12.2% | +4.8% | bash, json |
| 1→8 | -0.3% | — | +0.5% | +0.0% | -0.3% | +11.0% | +11.2% | bash, json |
| 1→16 | -0.4% | — | +0.4% | +0.0% | -0.5% | +7.9% | +24.0% | bash, json |
| 2→4 | +0.1% | — | +0.3% | -0.6% | +0.0% | +7.4% | +3.1% | thrift |
| 2→8 | -0.1% | — | +0.4% | -0.2% | -0.0% | +4.3% | +9.3% | thrift |
| 2→16 | -0.2% | — | +0.4% | +0.3% | +0.3% | +0.2% | +21.7% | thrift |
| 3→4 | +0.3% | — | +0.4% | +0.3% | -0.3% | -22.3% | +1.5% | openscad |
| 3→8 | -0.6% | — | +0.3% | +0.3% | -0.6% | -25.4% | +7.7% | openscad |
| 3→16 | -0.5% | — | -0.1% | -0.3% | -0.9% | -25.2% | +20.2% | openscad |
| 4→8 | +0.3% | -0.3% | +0.8% | +0.8% | +0.8% | -1.7% | +5.5% | go, python |
| 4→16 | +0.1% | -0.1% | +0.2% | +1.4% | +0.4% | +1.4% | +16.4% | go, python |
| 5→8 | +0.2% | — | -0.4% | -0.1% | +0.3% | -24.8% | +4.1% | xml-xml |
| 5→16 | +0.0% | — | -0.5% | -6.1% | -0.8% | -25.1% | +16.4% | xml-xml |
| 6→8 | +0.1% | — | +0.1% | +0.1% | -0.4% | -23.1% | +1.8% | glsl |
| 6→16 | +0.0% | — | +0.2% | -0.1% | -0.6% | -23.1% | +11.0% | glsl |
| 7→8 | -0.5% | +0.2% | +0.3% | -0.2% | -0.2% | -24.9% | +1.0% | c, cpp, tsx, typescript |
| 7→16 | -0.4% | +0.0% | +0.2% | +0.2% | -0.2% | -25.3% | +10.0% | c, cpp, tsx, typescript |
| 8→16 | +0.2% | -0.0% | +0.6% | -0.8% | -0.1% | +1.6% | +9.5% | java |


## Added-repository large-file checks

These rows are individual files, not the grammar-balanced averages above.
GLSL remains a small clean shader despite the relative `large` label.

| Points | Case | Bytes | Variant | Baseline highlight ms | Highlight | Cached walk | Membership walk | Slab |
|---|---|---:|---|---:|---:|---:|---:|---:|
| 1 | glsl-godotengine--godot-large | 8,478 | super8 | 0.316 | +1.0% | +0.8% | -26.0% | +1.2% |
| 1 | systemverilog-lowRISC--ibex-large | 77,078 | symbol16 | 5.503 | -2.5% | -6.6% | +1.9% | +1.8% |
| 1 | systemverilog-openhwgroup--cva6-large | 272,242 | symbol16 | 21.907 | -3.3% | -5.2% | +0.1% | +1.9% |
| 1 | nu-nushell--nu_scripts-large | 1,300,699 | field8 | 45.635 | -0.8% | -5.1% | -0.9% | +0.7% |
| 0 | glsl-godotengine--godot-large | 8,478 | super8 | 0.296 | +0.7% | -0.9% | -22.9% | +1.7% |
| 0 | systemverilog-lowRISC--ibex-large | 77,078 | symbol16 | 5.308 | -2.0% | -8.9% | +0.4% | +2.4% |
| 0 | systemverilog-openhwgroup--cva6-large | 272,242 | symbol16 | 20.833 | -1.4% | -8.0% | -0.1% | +2.5% |
| 0 | nu-nushell--nu_scripts-large | 1,300,699 | field8 | 43.128 | -2.3% | -8.9% | +0.5% | +1.1% |

## Unmeasured transitions

Each omitted source→target pair is listed explicitly. Wider dictionary IDs are
separate from the direct masks exercised here. No synthetic timing fills these gaps.

| Column | Transition | Coverage |
|---|---|---|
| Field ID | 8→16 | No suitable query/corpus workload selected |
| Field ID | 9→16 | No suitable query/corpus workload selected |
| Field ID | 10→16 | No suitable query/corpus workload selected |
| Field ID | 11→16 | No suitable query/corpus workload selected |
| Field ID | 12→16 | No suitable query/corpus workload selected |
| Field ID | 13→16 | No suitable query/corpus workload selected; widening adds no column bytes |
| Field ID | 14→16 | No suitable query/corpus workload selected; widening adds no column bytes |
| Field ID | 15→16 | No suitable query/corpus workload selected; widening adds no column bytes |
| Symbol | 1→2 | No suitable query/corpus workload selected |
| Symbol | 1→4 | No suitable query/corpus workload selected |
| Symbol | 1→8 | No suitable query/corpus workload selected |
| Symbol | 1→16 | No suitable query/corpus workload selected |
| Symbol | 2→4 | No suitable query/corpus workload selected |
| Symbol | 2→8 | No suitable query/corpus workload selected |
| Symbol | 2→16 | No suitable query/corpus workload selected |
| Symbol | 3→4 | No suitable query/corpus workload selected |
| Symbol | 3→8 | No suitable query/corpus workload selected |
| Symbol | 3→16 | No suitable query/corpus workload selected |
| Symbol | 4→8 | No suitable query/corpus workload selected |
| Symbol | 4→16 | No suitable query/corpus workload selected |
| Symbol | 12→16 | No suitable query/corpus workload selected |
| Symbol | 13→16 | No suitable query/corpus workload selected; widening adds no column bytes |
| Symbol | 14→16 | No suitable query/corpus workload selected; widening adds no column bytes |
| Symbol | 15→16 | No suitable query/corpus workload selected; widening adds no column bytes |
| Supertype dictionary ID | 9→16 | No suitable query/corpus workload selected |
| Supertype dictionary ID | 10→16 | No suitable query/corpus workload selected |
| Supertype dictionary ID | 11→16 | No suitable query/corpus workload selected |
| Supertype dictionary ID | 12→16 | No suitable query/corpus workload selected |
| Supertype dictionary ID | 13→16 | No suitable query/corpus workload selected; widening adds no column bytes |
| Supertype dictionary ID | 14→16 | No suitable query/corpus workload selected; widening adds no column bytes |
| Supertype dictionary ID | 15→16 | No suitable query/corpus workload selected; widening adds no column bytes |

## Audit

All **2,886 query runs** and **1,974 walk runs** completed successfully.
The combined control filter rejects 3 of 420 query blocks and 7 of 288 walk
blocks. Keeping every block changes no reported transition/operation aggregate
by as much as one percentage point (maximum 0.885 percentage points). This
supports the broad conclusions; sub-percent differences remain weak evidence.

The [raw audit artifact](pow2-widths-results-2026-09-14.json) contains the four
finished runs, verified build/binary hashes, generated patches, unit/persistence
results, corpus selection and parse outcomes, query adaptations, local validation,
filtered/unfiltered summaries, transition tables, disassembly, and reproduction
scripts. Benchmark tooling is committed in `9703bfbee`; production runtime
changes from other sessions were not included.
