# Reverse byte positions and walk follow-up, 2026-09-13

Byte-only packing now starts each frame at its subtree's end byte and subtracts
child sizes and padding while visiting children right-to-left. It no longer
allocates or fills a child-position arena, and frames no longer store position
arena marks/offsets. The forward pass still counts non-extra children for alias
and field indexes; it does not read their sizes or padding. Points-enabled builds
retain the forward coordinate calculation.

The subtraction runs before skipping invisible leaves. First-child padding is
not subtracted: it belongs to the parent's incoming padding. Hidden single-child
wrapper descent retains the original child's start, as before. Packed bytes,
format, group capacities, and retained tree allocations are unchanged.

## Cloud measurements

Measured on the existing `squatter-benchmark` GCP `e2-standard-2`, Broadwell Xeon
2.20 GHz, pinned to CPU 0. GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`, 16-slot
groups, presence enabled. Baseline is `7734a5741`; only the packer differs between
runtime variants. The candidate also decodes byte size/padding in the same
inline/heap branch as traversal metadata. All input and uploaded artifact hashes
were verified before timing. The driver ran sequentially under `squatter-idle`.

Nine large files, five alternating process pairs, seven timed conversions per
process after a warmup. Totals sum per-input medians across processes. Parsing
and output deletion are outside the conversion timer.

| Points | Packing | Before total | After total | Change |
|---|---|---:|---:|---:|
| Disabled | Default | 531.32 ms | 521.10 ms | −1.92% |
| Disabled | Compact | 539.84 ms | 530.85 ms | −1.67% |
| Enabled | Default, control | 608.29 ms | 609.51 ms | +0.20% |
| Enabled | Compact, control | 619.96 ms | 619.91 ms | −0.01% |

All nine byte-only default rows improve. Eight compact rows improve, but compact
YAML regresses **9.45%**; do not interpret the aggregate as a universal speedup.
Every pair has identical serialized hashes, sizes, groups, capacities, and nodes.

The existing 854-file tiny corpus uses calibrated whole-batch timings (at least
about 10 ms per sample), seven samples per process and five alternating process
pairs. Output allocation/deletion are inside packing timing; parsing is outside.
The table reports medians of paired aggregate percentage changes per size bucket.

| Source bytes | Files | Ordinary byte-only pack | Context byte-only pack |
|---|---:|---:|---:|
| 1–64 | 126 | −3.10% | −2.21% |
| 65–256 | 328 | −3.39% | −2.98% |
| 257–1024 | 400 | −4.29% | −2.81% |

Points-enabled tiny packing controls move between −1.04% and +1.23%. Byte-only
tiny O(1) walks move +1.63%, +1.01%, and +0.24% across those buckets; the runtime
read code is unchanged, so these rows do not establish a walk benefit from the
packer. The shared VM and separate executable layouts limit sub-percent claims.

### Compact YAML and allocator sensitivity

Separate instrumented binaries split conversion into setup, traversal, finalization,
and presence construction. Five alternating pairs of 15 conversions reproduce
an 11.58% compact-YAML regression (23.86 to 26.63 ms). Median phase times:

| Phase | Before | After |
|---|---:|---:|
| Setup | 0.320 ms | 0.040 ms |
| Traversal | 21.868 ms | 23.565 ms |
| Finalization | 0.649 ms | 1.656 ms |
| Presence | 0.994 ms | 1.354 ms |

Holding `MALLOC_MMAP_THRESHOLD_=131072` fixed reduces that diagnostic gap to
1.21% (26.63 to 26.95 ms), principally by slowing the baseline. Finalization then
costs 1.754 versus 1.761 ms. This supports an allocator/placement interaction when
position-arena allocations disappear; it does not isolate which allocation or
cache effect is responsible. The fixed threshold is a diagnostic, not a proposed
fix. Production allocation settings are unchanged, and the default-allocator
9.45% regression remains the relevant uninstrumented result.

### O(1) walks against mainline

Both runtimes read start/end bytes, symbol, named/error flags, and optional
start/end points. Neither reads child counts or calls the full attribute getter.
Five processes per input, alternating backend order, best of nine walks within
each process. The table sums per-file medians. Parsing, packing, and checks are
outside walk timing; cursor creation/deletion and checksumming are inside.

| Sample | Points | Mainline | Squatter | Squatter/mainline |
|---|---|---:|---:|---:|
| 100 typical C files | Disabled | 25.14 ms | 17.83 ms | 0.709× |
| 100 typical Python files | Disabled | 23.04 ms | 15.32 ms | 0.665× |
| Nine large files | Disabled | 527.88 ms | 308.41 ms | 0.584× |
| 100 typical C files | Enabled | 27.12 ms | 19.34 ms | 0.713× |
| 100 typical Python files | Enabled | 24.94 ms | 16.56 ms | 0.664× |
| Nine large files | Enabled | 566.98 ms | 339.70 ms | 0.599× |

All 418 file/configuration rows pass mainline checksum comparison. These are a
new workload baseline, not a measured runtime improvement over the old bulk walk.
The C probe reads fewer O(1) attributes than the Rust benchmark (which also reads
names, grammar IDs, and the remaining flags).

[Complete cloud results, input/artifact hashes, field diagnostics, and summaries](byte-reverse-cloud-results-2026-09-13.json).

## Validation

- All nine existing large inputs: exact serialized equality against `7734a5741`,
  default/forced-growth capacity and compact/noncompact output (36 comparisons).
  Ordinary/context equality also passes all eight combinations of capacity,
  compaction, and presence on each input, including trimming and output lifetime.
- Eleven grammars: mainline differential traversal and persistence checks, built-in
  edge cases, and two corpus files plus deterministic edits where the four-grammar
  tiny corpus is available. Repeated under ASan/UBSan with leak detection.
  Expected field mismatches are zero; existing ignored seek differences retain
  the harness policy.
- Allocation-failure recovery under sanitizers for the four tiny-corpus grammars;
  synthetic 0-, 9-, and 65-supertype TypeScript contexts also pass.
- Byte-only and points-enabled packed-column unit checks pass. The points build
  also passes Python differential edge cases and the revised C tiny-walk check.
- Revised Rust cursor/iterator walks: 32 files in four grammars, original and
  mutated, points enabled/disabled, three walk selectors: 384 file/workload cases,
  zero failures. Cargo tests pass with and without points.

## Field-map caching investigation

Instrumentation on the nine large files (4,665,634 visible nodes) counts:

| Operation | Count |
|---|---:|
| Frame initializations | 2,473,267 |
| Frames rebuilding direct-field scratch | 607,346 |
| Field-map entries in those frame setup lookups, including inherited entries | 1,242,140 |
| Field scratch slots cleared | 1,607,776 |
| Field-map lookups in hidden-wrapper descent | 1,679,301 |

Scratch rebuilding occurs in 51.5% of C++ frames and 14.3% of Python frames.
The scratch stores u16 field IDs, so total clearing is only 3.22 MB across the
large sample, excluding reads/writes while populating and consuming it. The
opportunity is repeated work, not a large scratch-memory bandwidth demand.
Map-entry counts measure slice lengths, not individual loop comparisons, and
hidden-wrapper calls include empty maps.

A compact cache indexed by production ID, containing a u32 offset and length
plus dense u16 direct-field IDs, would require these requested payload bytes:

| Grammar | Bytes |
|---|---:|
| JSON | 22 |
| YAML | 106 |
| C | 1,796 |
| C++ | 3,186 |
| Python | 2,358 |
| TypeScript | 5,314 |
| TSX | 5,450 |

These estimates exclude allocation headers and context pointer fields; languages
without fields need no cache. A prototype should populate it once in
`SQPackContext`, reuse it in both frame setup and hidden-wrapper descent, and
preserve first-direct-entry precedence, child-index bounds, and runtime field
inheritance. Ordinary one-off packing needs a control because cache construction
may outweigh its benefit there. No field cache is added by this change, and these
counts are not a measured cache speedup.

## Walk contract and profile

The timed Rust cursor and iterator walks now read only O(1) node attributes:
coordinates, names, symbol IDs, and flags, using individual accessors. Child,
named-child, and descendant counts are excluded, along with field/depth snapshots.
Full attributes remain checked at sampled nodes outside timing, and cursor fields
and depths are checked at every node. Cached iterator variants use the same node
accessors, so their optional unpack cache is idle; these rows no longer measure
cached attribute decoding. Existing bulk APIs retain their semantics.

The C tiny-layout harness likewise reads start/end bytes, symbol, named/error
flags, and optional points directly, without child counts or a bulk attribute
call. Historical walk reports use a different workload; they are not rewritten.

Cloud software sampling (`cpu-clock:u`, 997 Hz) covers 500 complete batch walks
per language with parsing, packing, and validation disabled from recording via
`prctl`. There are roughly 9K C samples and 8K Python samples, with no lost samples.
`perf` required elevated access; the VM's `perf_event_paranoid` setting was not
changed. Self sample percentages:

| Function | C | Python |
|---|---:|---:|
| `sq_node_symbol` | 28.01% | 27.99% |
| `sq_node_is_named` | 14.60% | 14.78% |
| `ts_language_symbol_metadata` | 5.77% | 6.07% |
| `ts_language_public_symbol` | 2.08% | 1.75% |
| Next-sibling lookup | 9.91% | 9.59% |

The four symbol/metadata functions account for about half of samples. A separate
O(1) bulk getter could share the packed-symbol decode between symbol and named
flag requests, while retaining the new workload's exclusion of count scans.
That is the strongest next read-path experiment from this profile. No runtime
cursor cache or attribute API change is included here, and this profile does
not measure a proposed bulk-getter speedup.

## Artifacts

Local reproduction scripts, compiler logs, exact-output probes, diagnostic source,
raw perf data, full timing samples, and correctness logs are under
`build/reverse-bytes/`. `check.py` and `sanitize.py` validate the packer;
`check-walks.py` exercises all revised Rust walk variants. `diagnostic.py` generates
an instrumented packer (also preserved as [an instrumentation patch](byte-reverse-field-diagnostics-2026-09-13.patch)), and `field-layout.c` measures the hypothetical table size.
`measure.py` measures packing, `profile.py` captures conversion hardware counters,
and `measure-walk.py` runs the walk comparison/profile. Benchmark-only sources
`walk.c` and `walk-cheap.c` preserve the two workload definitions.

The cloud bundle remains at `/home/mgsloan/reverse-bytes-cloud-20260913` on the
VM and locally at `build/reverse-bytes/cloud-upload`. Its `run.py` verifies hashes
and checkpoints the paired measurements. Use a fresh results path when repeating
it. The durable O(1) C probe is [walk.c](walk.c), built with `make -C lib/squat
../../build/squat/walk-bench`; its arguments are `LIBRARY SYMBOL SOURCE REPEATS
SEEK_ROUNDS` (zero seek rounds for walks only). `SQ_SQUAT_FIRST=0/1` selects the
backend measured first. Only its include path and usage label differ from the
uploaded copy.

The follow-up cloud profiles and YAML phase probes are saved in the results JSON;
raw perf files and logs are also in `build/reverse-bytes/cloud-diagnostics`.
`build-phases.py` generates the instrumented before/after sources. These probes
ran after the timing driver finished. The VM was initially stopped and is
returned to that state after downloading the results.
