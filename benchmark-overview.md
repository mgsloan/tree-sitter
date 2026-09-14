# Squatter vs. mainline: current status

Snapshot at `2891d5e98`, measured today on `squatter-benchmark` (GCP
e2-standard-2, Intel Xeon Broadwell 2.20 GHz, pinned to CPU 0). Every number
here is from **one consistent configuration**: `SQ_INCLUDE_POINTS=0` (no
row/column tracking) and default packing (`repack=false`, not compacted). No
mixing of build configs. Latency and memory only — no history.

Two samples, both fresh today:
- **200 typical files**: 100 C + 100 Python, randomly sampled (seeded) from
  `code-corpora`'s `train/`+`test/` trees, 500 B–150 KB, median ~870 nodes.
  This is the representative "ordinary source file" sample, used for cold
  parse, walk, seek, and query below.
- **9 large files**: 1.1–3.6 MB, six grammars, median ~500K nodes. Kept from
  an earlier run to show how the picture changes with file size — it does,
  notably for the walk workload (see below).

All comparisons are the same tree, byte-for-byte: Squatter is a lossless
re-encoding, so every difference below is representation/access-path cost,
never correctness (checked on every file, every workload: 200/200 + 9/9 exact
match).

## Latency

### Cold parse (parse + convert vs. plain mainline parse)

Squatter isn't a parser — using it means parsing with mainline, then packing.
Best-of-9 per phase, whole batch timed together.

| Sample | Mainline parse | Squatter parse+pack | Ratio |
|---|---:|---:|---:|
| 100 C files | 228.6 ms | 257.5 ms | 1.13× |
| 100 Python files | 202.6 ms | 229.8 ms | 1.13× |
| **200 files, combined** | 431.3 ms | 487.4 ms | **1.13×** |
| 9 large files | 3,189.0 ms | 3,640.8 ms | 1.14× |

Consistent regardless of file size: **~13–14% more time than a plain parse.**

### Forward walk (checked traversal, every node, all attributes)

Cursor-based preorder walk reading start/end byte, symbol, named/error flags,
and child count at every node — mainline `TSTreeCursor` vs. Squatter
`SQCursor`, same tree, same attributes, best-of-9.

| Sample | Mainline | Squatter | Ratio |
|---|---:|---:|---:|
| 100 C files | 27.57 ms | 28.21 ms | 1.02× |
| 100 Python files | 25.01 ms | 24.13 ms | 0.96× |
| 9 large files | 556.3 ms | 485.5 ms | **0.87×** |

**This does not point one direction consistently — it depends on file size.**
On typical small files, Squatter's walk is a wash: C is slightly *slower*
(92/100 files slower, up to 24%; median 1.05×), Python is roughly even
(54/100 faster, median 0.995×). On the large files it's a clear 13% win.
Reasonable explanation: per-call/per-node fixed overhead (cursor bookkeeping,
attribute-struct construction) is a bigger fraction of a short walk; the
representation's real advantage — fewer, denser cache lines — only pays off
once trees are big enough for cache effects to dominate. Do not assume a
walk speedup on a typical-sized file; it may go either way by a few percent.

### Descendant lookup (zero-length byte-range seek)

`ts_node_descendant_for_byte_range` vs. `sq_node_descendant_for_byte_range`,
same deterministic zero-length offsets, both correct (checksums matched on
every file). 5,000 rounds/file for the 200-file sample, 10,000 for the large
files. Best-of-9.

| Sample | Mainline | Squatter | Ratio |
|---|---:|---:|---:|
| 100 C files | 131.10 ms | 40.27 ms | **0.31×** |
| 100 Python files | 151.41 ms | 41.79 ms | **0.28×** |
| 9 large files (excluding the outlier below) | 147.0 ms | 48.7 ms | 0.33× |

**A clean, unambiguous win here — 100/100 C files and 100/100 Python files
were faster with Squatter**, no exceptions, ratio range 0.19×–0.72×
(1.4×–5.2× faster per file). This matches the earlier large-file result once
its one outlier is excluded.

**The one outlier does not appear in this 200-file sample**, and is now
understood to be narrow: on `dict_huge.py` (a single ~41,000-entry flat dict
literal) Squatter's seek was **5.4× slower** than mainline, confirmed
reproducible; a second wide-flat-JSON-object file was 1.56× slower. Root
cause, from `seek_byte()` in `lib/squat/node.c`: the zero-length-range path
walks backward through every physical slot sharing the queried start byte
before it can rule out an ambiguous shared boundary, falling back to an
O(width) linear sibling scan in the worst case — cheap for ordinary trees,
real for one node with tens of thousands of direct children. None of the 200
sampled files (max 23,675 nodes, ordinary shapes) triggers this. It's a real,
unfixed edge case for a specific tree shape, not a general seek regression —
this sample is the evidence for that distinction.

### Query (compiled `.scm` pattern matching)

Same 100 C + 100 Python files, real query files from `code-corpora`'s grammar
definitions (`highlights.scm` — syntax highlighting, tens of patterns, matches
nearly every token; `tags.scm` — symbol/definition extraction, ~9–14
patterns, matches only specific structural nodes). Two workloads per query:
draining `next_match` vs. draining `next_capture`. Same compiled query run
against both representations; match/capture counts matched exactly on all 8
combinations (zero mismatches). Best-of-9, whole 100-file batch timed
together.

| Language | Query type | Workload | Matches found | Mainline | Squatter | Ratio |
|---|---|---|---:|---:|---:|---:|
| C | highlights | matches | 673,604 | 95.68 ms | 40.40 ms | 0.42× |
| C | highlights | captures | 168,417 | 98.62 ms | 49.09 ms | 0.50× |
| C | tags | matches | 2,909 | 64.17 ms | 3.35 ms | **0.05×** |
| C | tags | captures | 1,934 | 64.71 ms | 3.53 ms | **0.05×** |
| Python | highlights | matches | 914,655 | 88.24 ms | 46.25 ms | 0.52× |
| Python | highlights | captures | 203,980 | 104.68 ms | 57.20 ms | 0.55× |
| Python | tags | matches | 50,714 | 62.77 ms | 19.85 ms | 0.32× |
| Python | tags | captures | 21,404 | 64.76 ms | 21.20 ms | 0.33× |

**Squatter wins every combination, but by very different margins depending on
query selectivity.** `highlights` (matches almost every token — hundreds of
thousands of matches across 100 small/medium files) is a solid **2×–2.4×**
win. `tags` (a handful of structural patterns — definitions, calls — orders
of magnitude fewer matches) is **3×–19× faster**, most dramatically for C
(19×). The `matches`-vs-`captures` workload choice barely matters; which
query and how selective it is matters far more.

## Bytes

Retained allocation bytes (not serialized-file size), byte-only build,
default (non-compact) packing — a separate, previously-measured 88-file
bounded sample (different files than the 200 above, same conclusion class):

| Representation | 88 bounded files | 9 files ≥1 MiB | B/node, bounded | B/node, large |
|---|---:|---:|---:|---:|
| Mainline | 12.39 MiB | 402.69 MiB | 75.85 | 90.50 |
| Squatter, byte-only | 2.43 MiB | 63.46 MiB | 14.88 | 14.26 |

**~80–84% less memory.** Construction *peaks* are higher than mainline's
steady state (conversion briefly holds both trees); this is retained/
steady-state bytes, not peak. (Not re-measured against the new 200-file
sample today — no reason to expect it differs.)

## Bottom line (byte-only, non-compact)

- **Building a Squatter tree costs ~13% more time** than just parsing with
  mainline — consistent from tiny files to 3.6 MB files.
- **Reading it back**: byte-range seeks are a clean win everywhere tested
  (100/100, 100/100, and 8/9 large files faster, typically 1.4×–5× faster) —
  except for trees containing one extremely wide flat container, which can
  regress up to 5×. Forward-walk speed is a wash on typical small files and
  only becomes a clear win (~13%) on large trees.
- **It uses about a fifth of the memory.**
- **Queries are faster in every case tested, 2×–19× depending on
  selectivity** — the more selective the query (fewer, more structural
  matches), the bigger Squatter's advantage.
- Net: the memory, seek, and query wins are close to universal; don't expect
  a walk speedup unless the trees involved are large. Check whether your
  workload does many zero-length-range lookups against very wide, flat
  containers before relying on seek performance in that specific case.

## What isn't captured here

- Points-enabled and compact-packing configurations: measured in earlier
  reports, not reproduced in this snapshot.
- Non-zero-length range seeks, named-descendant seeks, point-range seeks:
  not part of this run.
- Only `highlights.scm`/`tags.scm` for C/Python: not `locals.scm`,
  `injections.scm`, or other query types some grammars ship.
- Persistence / load-from-disk latency: tracked separately.
- Memory wasn't re-measured against the new 200-file corpus sample.
