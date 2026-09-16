# Combined display and grammar IDs

A u16 code stores the public display ID immediately above a grammar selector:

```text
[ unused zero bits | public display ID | grammar selector ]
code = (display << selector_bits) | selector
```

The public IDs remain Tree-sitter IDs, with builtin errors encoded after the
language's symbol range. They are not densely renumbered. Display bits are not
aligned to the word's MSB: leaving spare bits at the top keeps codes small enough
to use directly as dictionary indexes. Consecutive display IDs still occupy
consecutive intervals, and masked equality ignores the selector bits.

## Selection

The encoding is grammar-wide and deterministic; trees share immutable tables.

1. **Byte pairs:** when both literal IDs fit in u8, store display in the high
   byte and grammar in the low byte. Both accessors can read bytes directly.
2. **Shared selectors:** zero means the display ID uniquely determines the
   grammar ID, using a display-to-grammar table. Nonzero selectors index a
   shared grammar-ID dictionary, independent of the display ID.
3. **Local selectors:** if shared selectors need too many bits, use an ordinal
   within each display ID's possible grammar IDs. The entire u16 code indexes
   one grammar-ID table. Its holes cost shared memory, not per-node memory.
4. **Separate column:** only when the local split also exceeds 16 bits, keep
   public display IDs in the symbol column and add a dense u16 grammar-ID column.
   It participates in ordinary growth, compaction, persistence, and validation.

Byte alignment takes priority over minimizing selector bits. With literal public
IDs and the current error encoding, all measured grammars whose display IDs fit
in u8 also have literal grammar IDs that fit in u8. A grammar dictionary therefore
would not buy another byte-readable case here. Compacting public IDs could make
more grammars fit in two bytes, but would add a mapping to public-ID reads.

For shared selectors, the required width is `ceil(log2(ambiguous_grammar_ids + 1))`.
For local selectors, it is `ceil(log2(max_variants_per_display))`. A particularly
ambiguous display ID sets the local width for the whole language. If it fits,
the shared scheme usually saves substantial table memory at the cost of a branch
between the unique-default and dictionary paths. Local decoding uses one direct
lookup but may need a large, sparsely populated table.

The runtime alias map covers nonterminals only. Terminal aliases are reconstructed
by walking structural shifts/gotos backwards from aliased reductions. Merged LR
states can conservatively add pairs. The dictionary does not depend on the input
corpus, and byte-pair grammars skip this analysis entirely.

## Grammar measurements

Pinned staged grammars from the preceding public-display experiment. Widths
include the remapped error IDs. Decode bytes exclude packing/validation tables;
all tables are shared per prepared grammar, not stored in each tree.

| Grammar | Maximum variants | Shared selector bits | Selected encoding | Selector bits | Decode bytes |
| --- | ---: | ---: | --- | ---: | ---: |
| Bash | 8 | 7 | Shared | 7 | 716 |
| C | 5 | 7 | Shared | 7 | 860 |
| C++ | 5 | 7 | Local | 3 | 8,960 |
| CSS | 9 | 6 | Byte pair | 8 | 0 |
| Go | 3 | 4 | Byte pair | 8 | 0 |
| HTML | 4 | 4 | Byte pair | 8 | 0 |
| JSON | 1 | 0 | Byte pair | 8 | 0 |
| Python | 7 | 5 | Shared | 5 | 612 |
| TSX | 25 | 7 | Shared | 7 | 940 |
| TypeScript | 24 | 6 | Shared | 6 | 896 |
| YAML | 36 | 8 | Local | 6 | 38,144 |

All eleven fit combined codes. C++ needs ten display bits and YAML nine, so their
shared selectors would require seventeen bits. Local selectors fit in thirteen
and fifteen bits respectively. For TSX, switching from local to shared selectors
reduces decode tables from 25,728 to 940 bytes. Across all eleven grammars, the
selected encoding retains 65,694 bytes of symbol tables, including packing and
validation metadata but excluding the fixed descriptor and existing public map.

Preparation in one native diagnostic pass ranged from 0.006–0.020 ms for byte
pairs to roughly 0.3–6.4 ms for dictionary grammars. Preparation is reusable and
excluded from the traversal/parse measurements below.

## Performance and storage

Compared with `7322290b3` (public display IDs plus sparse original grammar IDs),
2026-09-16, Intel Core Ultra 7 165U, Linux x86-64. Release `squatter-bench`, CPU 0,
isolated cache mode, three alternating passes, nine repeats per pass. Ratios
compare each file's median of pass medians, then take the geometric mean across
files. Traversals perform thirty iterations per timed measurement.

| Workload | Files | Time change | Instruction change |
| --- | ---: | ---: | ---: |
| Cursor attribute digest | 39 | -3.58% | -4.96% |
| Cached iterator digest | 39 | -3.97% | -7.02% |
| Cursor attribute scan | 39 | -4.11% | -5.22% |
| Cached iterator scan | 39 | -6.41% | -7.47% |
| Query matches | 20 | +0.75% | +0.64% |
| Query captures | 20 | -0.61% | +0.55% |
| Parse + pack, reused context | 39 | -0.92% | -0.23% |

Query speed is essentially preserved. The scanner's per-word masked comparison
is unchanged; cursor execution shifts the compiled masks once for its tree's
encoding. Ordinary node reads gain display extraction and decoder selection,
while grammar reads lose the sparse bitmap/rank lookup. Mainline controls changed
by -0.1% to -1.0% for queries/traversals; setup parsing was noisier at -2.4%.

An initial three-repeat comparison measured captures 1.3% slower and matches
4.0% faster. The longer run did not reproduce those timing differences; query
instruction changes stayed below 1%. Treat small query timing changes as noise
on these short workloads, not evidence of a consistent speedup or slowdown.
The initial local-only prototype also preserved query instruction counts, but
used larger shared dictionaries and could not use direct byte reads.

The 48-file layout probe covers 20,417,561 source bytes and 5,634,993 nodes.
Compacted slabs shrink from 111,812,776 to 110,586,672 bytes (**-1.10%**), with
shared tables accounted for separately above. YAML shrinks 7.52%; the other
languages shrink 0–2.61%. JSON and Python had no sparse overrides in this corpus,
so their slab sizes are unchanged. Real workloads with few small trees may pay
more for shared dictionaries than they save in slabs, particularly with YAML.

## Removing local selectors

A separate experimental build sends C++ and YAML to dense grammar columns.
All other sampled grammars keep their encodings. Compared with the selected
mixed encoding:

| Grammar | Shared table bytes saved | Extra compacted slab bytes | Slab growth |
| --- | ---: | ---: | ---: |
| C++ | 10,080 | 10,336 | +9.44% |
| YAML | 38,740 | 462,272 | +10.69% |

The added column costs two bytes per allocated slot, including group waste and
spare capacity. The table savings are once per prepared grammar. Local encoding
wins total memory above about 5,040 C++ slots or 19,370 YAML slots across trees
sharing that grammar. The decode arrays themselves contain 4,480 C++ entries
(570 actual pairs) and 19,072 YAML entries (310 pairs).

A three-pass, nine-repeat comparison on the eight affected corpus files used
three traversals per measurement, including the larger YAML inputs. Removing
local selectors reduced traversal times by 2.2–3.1%, with instruction reductions
of only 0.4–0.5%; controls moved between -1.9% and +0.6%. Four bounded query files
measured matches -0.47% and captures +0.67%, with instruction changes below 0.1%.
Parse-plus-pack time increased 3.97% (instructions +0.09%, control time +1.15%).
All comparisons passed. This provides no query-speed reason to retain the local
scheme; its benefit is denser trees. Cache-pressure effects remain unmeasured.
Production retains local selectors so the extra grammar column is absent when
the information can still fit in sixteen bits.

A further alternative is compact public-ID numbering: the grammars have 508
and 141 distinct public IDs respectively, including errors. That could let the
shared scheme fit both languages, and make YAML byte-aligned. It would introduce
a mapping back to public IDs on node reads; the current implementation preserves
literal public IDs instead.

## Reproduction and checks

Artifacts are under `build/symbol-pairs/`. `bench.py` compares the sparse baseline,
local-only prototype, and mixed prototype. `final-bench.py` and
`summarize-final.py` produce the final nine-repeat table in `results-final/`.
`fanout-final.csv` includes terminal aliases; the earlier `fanout.csv` omits them
and must not guide encoding decisions. Layout inputs and staged grammar/query
registries come from `build/public-display/`; run manifests record their hashes.
`bench-no-local.py` and `summarize-no-local.py` cover the separate-column
experiment in `results-no-local/`; `no-local-block.txt` preserves the temporary
`SQ_NO_LOCAL_SYMBOLS` build change. The local scripts, preserved executables,
layout CSVs, and ID histograms support
repeating the comparisons without rebuilding the corpus.

Tests cover all four modes, the combined/fallback boundary, aliases absent from
the runtime alias map, invalid codes, persistence, growth, compaction, and the
actual fallback emitter. Native node/cursor/query comparisons passed across ten
grammars. ASan/UBSan checks cover unit/supertype tests, Go node comparisons, and
TSX query comparisons. All completed benchmarks reported zero comparison failures, including mutated
inputs for the final traversal and query selections.
