# Independent field, symbol, and supertype widths — 2026-09-14

This experiment separates field-ID rounding from symbol rounding, then tests
variable-width supertype storage independently. It measures complete real
highlighting/tags queries and full walks on GCP. These are frozen experimental
builds; rounding and supertype policies remain experimental. Field-column
omission was applied separately in `65e50c348`.

## Recommended policies

For this workload mix, use different 8-bit thresholds for the two columns:
**field IDs at 6 bits, symbols at 5 bits**. Field 6→8 improves queries by
1.3–1.9% and full walks by 3.2–5.6%, for 1.3% / 1.9% more slab space
(points / byte-only). Field 5→8 adds 2.1% / 3.1% space, gives no clear query
improvement, and improves walks by 2.6–6.0%; it is an additional option for
traversal-heavy workloads. Do not widen the 2-bit field column by default:
its 5.6% / 9.0% space cost buys little query improvement.

Symbols at 5→8 and 6→8 have stronger measured returns: 2.4–4.4% less
highlighting time and 5.7–14.5% less walk time. Their slab costs are
2.7% / 4.5% and 1.6% / 2.6%, respectively. These transitions cover JSON
and HTML; the unmeasured widths remain explicitly identified below.

Keep **13 bits as the conservative 16-bit threshold**, because 13→16,
14→16, and 15→16 have no column-space penalty. This recommendation rests
on layout/access properties; the corpus does not measure those query cases.
The observed 9→16 and 10→16 transitions are useful speed/space options:
walks improve by 5.9–14.4%, but their slab costs are 5.6% / 8.3% and
4.1% / 5.8%. The C++-only 10→16 highlighting gain is 3.8% / 5.3%; its
tags gain is below 1%. These results do not establish an optimum for absent
11- and 12-bit workloads.

For **supertypes**, omitting an unused column is attractive, but arbitrary
bit packing is not a general performance win. Ordinary highlighting/tags
change little, while the C#/SML supertype queries slow by about 9%. The
specialized 4-bit getter cuts that penalty to 1.2–4.4%, at the same 3.2–5.2%
slab saving for those grammars, but membership-heavy walks remain 28.5–38.9%
slower. Keep 7-bit direct masks in a byte: saving only 0.6–1.0% of the slab
costs roughly 50–57% in the membership walk. One-bit masks save substantially
more space and have nearly flat real-query results; they are a reasonable
memory-oriented choice, with a measured membership-walk cost in the points
build. General 4-bit compression should likewise be an explicit space tradeoff.

These are workload-specific choices, not a proof of a universal optimum.
Omission of the field column for fieldless grammars is measured separately in
[the omission experiment](fieldless-columns-results-2026-09-14.md).

## Baseline query time

With points enabled and trees already prepared, the compact-width baseline has
the following CPU times. Small batches contain three files; each large job
contains one complete file. These ranges cover different workloads and result
counts, not repeated measurements of a single query.

| Workload | Three-file small batch | Large file |
|---|---:|---:|
| Highlighting | 0.579–7.147 ms | 38.326–236.229 ms |
| Tags | 0.201–0.580 ms | 3.401–111.917 ms |

## Policies and interpretation

Each exact-width policy changes just one column at one transition. The matrix
contains field IDs at 2→8, 5→8, and 6→8 bits, and symbols at 5→8, 6→8, 9→16,
and 10→16 bits. Already-native symbol width 8 is a baseline, not a rounding
candidate. The combined policy rounds fields of at least 6 bits to 8 and symbols
of at least 9 bits to 16; interaction measurements use the grammars where both
columns change. Each row averages only its affected grammars; different rows
therefore have different populations and should not be ranked as whole-corpus
policies. Memory percentages are grammar-balanced slab-size ratios.

The real corpus has no widths 3, 4, 7, or 11–15 in these columns. No query-time
cutoff for those transitions can be inferred from these measurements. In
particular, the 13–15→16 case needs a representative grammar/workload before a
query performance recommendation. Symbol and field thresholds are independent.

### Transition coverage

An em dash means no representative grammar in this corpus; it is not a zero
speedup. Each transition is listed separately, including unmeasured candidates.

| Transition | Symbol evidence | Field-ID evidence |
|---|---|---|
| 2→8 | — | CSS, HTML, JSON, YAML |
| 3→8 | — | — |
| 4→8 | — | — |
| 5→8 | JSON | Bash |
| 6→8 | HTML | C, C++, Go, Python, TSX, TypeScript |
| 7→8 | — | — |
| 9→16 | Bash, C, Python, TSX, TypeScript, YAML | — |
| 10→16 | C++ | — |
| 11→16 | — | — |
| 12→16 | — | — |
| 13→16 | — | — |
| 14→16 | — | — |
| 15→16 | — | — |

The 2-bit field row includes two grammars with no fields (CSS and HTML) and
two with two fields (JSON and YAML). Rounding the unused field columns changes
storage and addresses but does not provide evidence about field checks there.

The 13-, 14-, and 15-bit columns already store four values per 64-bit word,
so each transition to 16 has zero column-space cost. This is a
structural reason to consider them, not a measured end-to-end query speedup.

The `super` policy retains direct bit masks for grammars with up to eight
supertypes, using exactly one bit per supertype. Zero supertypes require no
column. For larger grammars it retains the existing grammar-wide dictionary and
uses `ceil(log2(dictionary_count))` bits per node. Native widths retain primitive
accesses; non-native widths use the packed getter. This tests variable bit width
per grammar, not per-node varints. The format stores the actual supertype width
and rejects incompatible policies on load.

A follow-up `superpow2` probe extracts 2- and 4-bit values directly from bytes,
avoiding the generic getter's division. It uses the same variable-width storage.
This byte-reader probe assumes little-endian layout, as on the benchmark host.
Its timing comparison covers the four affected grammars: Go, Python, C#, and
SML, all using 4-bit values. The 2-bit case has correctness coverage but no
representative grammar in this matrix.

C# has nine supertypes and 14 dictionary entries; SML has twelve and 16 entries.
Both fit in 4-bit dictionary IDs instead of 8. A synthetic 512-entry dictionary
fixture additionally verifies 9-bit storage, growth, membership, and persistence.
It is a correctness fixture, not a substitute for a real query benchmark.

## Measured transitions

Negative percentages mean less CPU time or fewer slab bytes. Highlighting and
tags cover the primary 11 grammars; supertype queries cover C#/SML. The overall
supertype walk/space rows cover all 13 grammars. The membership walk checks every
grammar supertype at every visible node; it is separate from ordinary attribute
walks and exposes costs that those walks do not exercise. For a grammar with no
supertypes it reduces to preorder traversal, so its changes can reflect code
placement and layout effects rather than membership decoding.

### Points enabled

| Only this transition | Highlight | Tags | Cursor walk | Uncached walk | Cached walk | Slab bytes |
|---|---:|---:|---:|---:|---:|---:|
| Field ID 2→8 | -0.6% | — | -4.4% | -3.8% | -2.3% | +5.6% |
| Field ID 5→8 | +1.2% | — | -2.6% | -6.0% | -4.5% | +2.1% |
| Field ID 6→8 | -1.4% | -1.7% | -3.8% | -5.6% | -4.6% | +1.3% |
| Symbol 5→8 | -2.4% | — | -10.4% | -14.5% | -5.7% | +2.7% |
| Symbol 6→8 | -4.1% | — | -9.4% | -13.8% | -6.5% | +1.6% |
| Symbol 9→16 | -1.9% | -3.8% | -8.5% | -14.4% | -6.4% | +5.6% |
| Symbol 10→16 | -3.8% | -0.8% | -8.7% | -13.8% | -5.9% | +4.1% |
| Field ≥6→8 + symbol ≥9→16 | -3.5% | -3.9% | -13.4% | -19.5% | -9.0% | +6.5% |

### Points disabled

| Only this transition | Highlight | Tags | Cursor walk | Uncached walk | Cached walk | Slab bytes |
|---|---:|---:|---:|---:|---:|---:|
| Field ID 2→8 | -0.6% | — | -3.1% | -5.4% | -2.3% | +9.0% |
| Field ID 5→8 | -0.6% | — | -5.7% | -4.5% | -4.5% | +3.1% |
| Field ID 6→8 | -1.3% | -1.9% | -3.2% | -5.0% | -5.0% | +1.9% |
| Symbol 5→8 | -3.5% | — | -12.2% | -11.8% | -7.3% | +4.5% |
| Symbol 6→8 | -4.4% | — | -10.2% | -11.2% | -6.6% | +2.6% |
| Symbol 9→16 | -2.2% | -3.4% | -9.2% | -13.0% | -6.7% | +8.3% |
| Symbol 10→16 | -5.3% | -0.7% | -8.6% | -11.7% | -5.9% | +5.8% |
| Field ≥6→8 + symbol ≥9→16 | -3.8% | -4.2% | -13.6% | -19.9% | -11.0% | +9.6% |

### Variable-width supertypes

| Build | Highlight | Tags | Supertype query | Cursor walk | Uncached walk | Cached walk | Membership walk | Slab bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Points | +0.5% | +1.0% | +9.0% | +0.1% | -0.1% | +0.1% | +36.7% | -3.9% |
| Byte-only | -0.6% | -1.6% | +9.0% | -0.7% | +1.9% | +0.1% | +28.6% | -6.2% |

### Supertype results by grammar — points

| Grammar | Supertypes / dictionary entries | Width | Highlight | Tags | Supertype query | Membership walk | Slab bytes |
|---|---:|---:|---:|---:|---:|---:|---:|
| bash | 1 / 0 | 8→1 | +0.5% | — | — | +11.8% | -5.6% |
| c | 7 / 0 | 8→7 | -0.6% | +1.6% | — | +56.9% | -0.7% |
| cpp | 7 / 0 | 8→7 | -0.2% | +2.3% | — | +55.6% | -0.6% |
| csharp | 9 / 14 | 8→4 | — | — | +8.2% | +63.4% | -3.2% |
| css | 0 / 0 | 8→0 | +1.4% | — | — | +6.3% | -7.2% |
| go | 4 / 0 | 8→4 | +0.9% | +0.8% | — | +52.9% | -3.5% |
| html | 0 / 0 | 8→0 | +0.6% | — | — | +5.9% | -7.9% |
| json | 1 / 0 | 8→1 | +1.2% | — | — | +12.3% | -7.1% |
| python | 4 / 0 | 8→4 | +0.9% | +0.4% | — | +52.8% | -3.3% |
| sml | 12 / 16 | 8→4 | — | — | +9.9% | +71.7% | -3.4% |
| tsx | 7 / 0 | 8→7 | +1.0% | +1.1% | — | +56.3% | -0.7% |
| typescript | 7 / 0 | 8→7 | +0.0% | +0.1% | — | +54.7% | -0.7% |
| yaml | 0 / 0 | 8→0 | +0.4% | — | — | +6.5% | -6.7% |

### Supertype results by grammar — byte-only

| Grammar | Supertypes / dictionary entries | Width | Highlight | Tags | Supertype query | Membership walk | Slab bytes |
|---|---:|---:|---:|---:|---:|---:|---:|
| bash | 1 / 0 | 8→1 | -0.7% | — | — | -2.4% | -8.2% |
| c | 7 / 0 | 8→7 | -0.6% | -2.0% | — | +51.0% | -1.0% |
| cpp | 7 / 0 | 8→7 | -0.8% | -1.1% | — | +50.5% | -0.9% |
| csharp | 9 / 14 | 8→4 | — | — | +9.5% | +60.8% | -4.8% |
| css | 0 / 0 | 8→0 | -0.3% | — | — | -5.9% | -11.3% |
| go | 4 / 0 | 8→4 | -1.5% | -3.4% | — | +53.1% | -5.4% |
| html | 0 / 0 | 8→0 | -0.2% | — | — | -6.5% | -13.1% |
| json | 1 / 0 | 8→1 | -1.9% | — | — | -1.1% | -11.9% |
| python | 4 / 0 | 8→4 | -0.3% | -0.9% | — | +52.2% | -5.0% |
| sml | 12 / 16 | 8→4 | — | — | +8.4% | +68.4% | -5.2% |
| tsx | 7 / 0 | 8→7 | +0.5% | -1.0% | — | +50.2% | -1.0% |
| typescript | 7 / 0 | 8→7 | +0.1% | -1.0% | — | +50.9% | -1.0% |
| yaml | 0 / 0 | 8→0 | -0.4% | — | — | -5.1% | -10.1% |

### Direct-byte supertype reads

Paired follow-up on the four grammars with 4-bit supertype values. Each cell
shows **generic packed getter / direct-byte getter**, both relative to the
same 8-bit baseline. Their slab sizes are identical. These rounds are separate
from the main width matrix.

| Build | Grammar | Highlight | Tags | Supertype query | Membership walk | Cached walk |
|---|---|---:|---:|---:|---:|---:|
| Points | go | +2.0% / +3.6% | -0.4% / -0.2% | — | +52.0% / +25.7% | +7.1% / -1.4% |
| Points | python | +0.3% / +1.2% | +0.9% / +0.4% | — | +51.7% / +25.3% | +1.6% / +1.9% |
| Points | csharp | — | — | +9.1% / +4.4% | +64.0% / +30.9% | -1.3% / -1.5% |
| Points | sml | — | — | +8.9% / +3.9% | +70.7% / +38.9% | -0.3% / -0.2% |
| Byte-only | go | -0.5% / -0.6% | -1.9% / -2.7% | — | +51.5% / +21.6% | -0.6% / -1.4% |
| Byte-only | python | +0.0% / +0.2% | -0.1% / -0.3% | — | +51.8% / +21.9% | +1.7% / +0.3% |
| Byte-only | csharp | — | — | +9.2% / +3.2% | +60.2% / +28.5% | +0.1% / +0.3% |
| Byte-only | sml | — | — | +8.1% / +1.2% | +67.3% / +34.4% | -0.5% / -0.4% |

## Method

- GCP `squatter-benchmark`, `us-central1-a`, e2-standard-2, Intel Xeon 2.20 GHz
  (Broadwell), serial runs pinned to CPU 0. Five calibrated CPU-time samples,
  three paired randomized rounds, separately with and without points.
- Frozen baseline `ab6162834`, which includes the committed grammar-wide
  supertype dictionary. This is newer than the separate kernel/SWAR matrix's
  `ab9143b2` baseline. Absolute timings across the two matrices are not paired.
- GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`; Rust 1.95.0 release. The same
  source transformations apply to exact/control; policy macros select changes.
- The primary suite has 20 real highlighting jobs and 12 real tags jobs across
  11 grammars. Tags for TypeScript/TSX include the full JavaScript base. Query
  timing includes predicates, cursor creation, and consuming all ordered
  captures/matches; parsing and query compilation are outside the timer.
- Four additional C#/SML jobs use explicit generated supertype queries on tiny
  and approximately 78–98 KB source files. These are labeled `supertype`, not
  highlighting or tags. They test the dictionary representation directly.
- Full walks measure cursor attributes, uncached attributes, and the cached
  iterator. A fourth walk visits every visible node and tests every grammar
  supertype, exposing membership costs that ordinary attribute walks omit.
- Exact and identical-code control are present in every paired job. A job runs
  only the applicable rounding variants, plus `super`, so unchanged policies
  do not dilute a transition's aggregate. Variant order is randomized.
- Compare medians within pairs, median ratios across rounds, then geometric
  means within each grammar and across grammars. Reject a whole paired block
  if any identical-code timing control differs by more than 15%; retain raw
  rejected data and unfiltered summaries. Small differences remain subject to
  host noise and code-layout effects.
- Every first-round query compares complete ordered output against Tree-sitter.
  All timed counts/hashes and full-walk checksums must agree across policies.
  Slab-byte differences are allowed and reported independently of timing.
- Memory tables report serialized slab bytes. The prototype adds eight bytes of
  cached supertype decoding metadata per tree, identically in every variant.
  A production variable-width change would need to count that overhead against
  the slab savings; it can erase the saving for a tiny tree. Raw results also
  retain complete allocation sizes.

## Control audit

| Phase | Paired blocks | Rejected |
|---|---:|---:|
| Main query matrix | 216 | 4 |
| Main full walks | 144 | 2 |
| Direct-byte query follow-up | 60 | 0 |
| Direct-byte walk follow-up | 42 | 2 |

These are three attempted rounds per job/configuration; the summaries retain
accepted sample counts per row. No timing rejection removes the corresponding
raw data or changes the storage measurements. Keeping rejected blocks changes
main-query aggregates by less than 0.19 percentage points and main-walk
aggregates by at most 1.15 points. The unfiltered summaries are included.

## Reproduction and validation

```sh
python3 tools/squatter/prepare-column-probes.py --points 1,0 \
  --variants exact,control,field2,field5,field6,symbol5,symbol6,symbol9,symbol10,both,super
python3 tools/squatter/benchmark-kernel-probes.py BUNDLE --kind query \
  --variants exact,control,field2,field5,field6,symbol5,symbol6,symbol9,symbol10,both,super \
  --job-variants BUNDLE/job-variants.json --points 1,0 --rounds 3 --repeat 5 \
  --tag column-queries
python3 tools/squatter/summarize-kernel-probes.py RUN.json \
  --allow-layout-changes --output summary.json
```

The source generator and measurement support are committed as `4ffba2b22`;
the power-of-two getter probe and width fixture are `073ebd225`.
Each build runs packed-column, relocation, dictionary, and persistence tests.
ASan/UBSan query tests pass on JSON, C, C++, Python, and TypeScript.
The variable-supertype build also passes comparisons on JSON, Python, C++, TSX,
C#, and SML source fixtures, including edge cases. All 32 real query jobs also
pass local validation for every applicable policy.
Validation logs and raw results accompany the measurements.

[Raw measurements and audit](column-probes-results-2026-09-14.json).
