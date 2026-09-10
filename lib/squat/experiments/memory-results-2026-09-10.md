# Measured tree memory — 2026-09-10

**Default Squatter retains 71–77% less memory than mainline on the original samples. Compact packing saves 76–81%; byte-only compact packing saves 83–87%.** These are sums of actual requested live allocation bytes, including all per-tree overhead. Construction peaks are higher than mainline because conversion temporarily retains the mainline tree.

## Retained memory

Original inputs; MiB = 1,048,576 bytes. All Squatter configurations use 16-slot groups and the default symbol-presence index. “Compact” means `SQPackOptions.repack = true`. Byte-only builds omit row/column information; mainline retains its normal point information.

| Representation | 88 bounded files | 9 files ≥1 MiB | B/node, bounded | B/node, large |
|---|---:|---:|---:|---:|
| Mainline Tree-sitter | 12.39 MiB | 402.69 MiB | 75.85 | 90.50 |
| Squatter, default | 3.59 MiB | 93.31 MiB | 22.00 | 20.97 |
| Squatter, compact | 3.00 MiB | 76.55 MiB | 18.37 | 17.21 |
| Squatter, byte-only | 2.43 MiB | 63.46 MiB | 14.88 | 14.26 |
| Squatter, byte-only compact | 2.06 MiB | 52.22 MiB | 12.62 | 11.74 |

The bounded sample has 171,306 visible nodes. The nine large files have 4,665,634. Bytes per node divides total retained bytes by public visible-node count; mainline allocations also preserve hidden grammar structure. No old tree or incremental sharing is used.

`malloc_usable_size` includes allocation rounding/slack but excludes allocator metadata and free arenas. Its totals for bounded originals are 12.81 MiB, 3.61 MiB, 3.00 MiB, 2.44 MiB, 2.06 MiB in the same row order. The full JSON includes both metrics for every file.

### Mutations and the complete mixed sample

| Input sample | Files | Mainline | Default | Compact | Byte-only | Byte-only compact |
|---|---:|---:|---:|---:|---:|---:|
| Bounded, mutated | 88 | 12.21 MiB | 3.51 MiB | 2.93 MiB | 2.37 MiB | 2.01 MiB |
| Mixed, original | 53 | 408.68 MiB | 94.95 MiB | 77.94 MiB | 64.58 MiB | 53.19 MiB |
| Mixed, mutated | 53 | 391.78 MiB | 89.31 MiB | 73.45 MiB | 60.94 MiB | 50.27 MiB |
| ≥1 MiB, mutated | 9 | 385.88 MiB | 87.72 MiB | 72.11 MiB | 59.86 MiB | 49.34 MiB |

The complete mixed sample contains 53 files, including the nine ≥1 MiB files. Its 44 smaller files overlap the bounded sample: these are 97 unique original files, not 141 independent files. Mutations use the existing three-edit, seed-42 corruption procedure. Samples are reported separately rather than double-counted in a combined headline.

### Per-language reduction

Bounded originals, eight files per grammar. Percentages are reductions from mainline, using the ratio of byte totals rather than an average of per-file percentages.

| Grammar | Default | Compact | Byte-only | Byte-only compact |
|---|---:|---:|---:|---:|
| bash | 76.3% | 80.2% | 83.5% | 86.1% |
| c | 73.3% | 77.0% | 81.2% | 83.6% |
| cpp | 69.1% | 74.2% | 77.4% | 80.8% |
| css | 71.4% | 76.8% | 81.0% | 84.6% |
| go | 73.8% | 76.1% | 82.0% | 83.6% |
| html | 61.7% | 68.9% | 76.3% | 80.9% |
| json | 64.8% | 73.3% | 80.5% | 84.4% |
| python | 75.5% | 80.5% | 82.9% | 86.1% |
| tsx | 73.9% | 77.3% | 81.4% | 83.6% |
| typescript | 75.4% | 78.9% | 82.3% | 84.6% |
| yaml | 78.0% | 82.3% | 86.6% | 88.5% |

### Small-tree overhead

Default point-enabled Squatter is larger on 3/88 bounded originals (also 3/88 mutations). The original exceptions are:

| Source | Source bytes | Mainline | Squatter default |
|---|---:|---:|---:|
| `test/helm/pkg/cmd/testdata/helmhome/helm/plugins/exitwith/exitwith.sh` | 28 | 984 B | 1032 B |
| `test/Chart.js/node_modules/.pnpm/tailwindcss@3.4.19/node_modules/tailwindcss/screens.css` | 19 | 344 B | 736 B |
| `test/less.js/packages/test-data/tests-unit/impor/impor.css` | 48 | 536 B | 736 B |

Byte-only packing has two such original exceptions, both CSS files. A minimum slab, the tree object, and a supertype array sized for the grammar impose fixed costs. Across all 88 bounded originals, non-slab allocations total 59,696 B with points and 56,880 B without. They are included in every number above.

Compaction reduces retained point-enabled bytes by 16.5% on bounded originals and 18.0% on the nine large originals compared with default packing. It removes unused group capacity, not padding inside live groups. Byte-only packing also slightly improves group occupancy: bounded live groups fall from 11,954 to 11,778, and large-file groups from 309,193 to 305,292.

## Construction peaks

The public `sq_tree_parse` path parses a mainline tree, packs it while retaining the parser, then deletes the mainline tree. The caller then deletes the parser. Compact packing can temporarily hold both the old and resized slab. Consequently, lower retained memory does **not** imply a lower parsing peak.

The largest measured construction peak among original inputs occurs on `train/nodebb/node_modules/ace-builds/src/worker-xquery.js` for every configuration:

| Representation | Peak requested runtime/Squatter allocations |
|---|---:|
| Mainline Tree-sitter | 114.57 MiB |
| Squatter, default | 132.66 MiB |
| Squatter, compact | 141.98 MiB |
| Squatter, byte-only | 127.32 MiB |
| Squatter, byte-only compact | 132.49 MiB |

These are tracked live-allocation high-water marks, not RSS or total allocator footprint. They exclude direct libc allocations inside precompiled grammar scanners and any temporary copying internal to libc `realloc`. The scanner is destroyed with the parser before retained measurements; serialized scanner state owned by mainline trees is allocated through the runtime and is counted. The JSON also records staged packing peaks with the parser already released, under `pack_peak_with_mainline`.

## Method and validation

- Instrumentation: [`memory.c`](memory.c), with GNU linker wrapping of `malloc`, `calloc`, `realloc`, `aligned_alloc`, and `free` in runtime/Squatter objects. A separate fixed pointer table preserves real allocation sizes and alignment. Its storage is excluded.
- Tree-only retained memory excludes source text, shared grammar code/data, parsers, cursors, query execution, allocator metadata, free arenas, and measurement machinery. Both implementations receive the same source and grammar. Squatter includes its object, slab capacity, symbol index/dictionary, and supertype allocation.
- Every packed tree has exactly three retained allocations. Requested totals independently match `sizeof(SQTree) + slab_bytes + grammar_symbol_count * sizeof(TSSymbol)` (including alias symbols). Node counts match mainline. All lifecycles return to zero tracked allocations.
- Two fresh-process repetitions of each point mode for 282 input cases: **1,128 probe executions**, all exactly reproducible, including usable bytes and peaks. Mainline counts/bytes match between point modes for every input. The 88 overlapping original/mutated cases also match between corpus samples.
- Independent Rust results match node counts, slab bytes, groups, and capacity for all 176 bounded original/mutated inputs. There are 132 mutation hash checks against the saved Rust output, including overlaps.
- Build: GCC 15.3.0, `-O3 -g`, default 16-slot layout; `SQ_INCLUDE_POINTS=1` or `0`. Implementation revision `b1e0debf2`; benchmark revision `6c9b3d1fa`. Mainline runtime is the unmodified `lib/src` tree `dd2a80a141532d56c297bf1f266b0fcd7892a6a8`.
- Run: x86-64, glibc 2.39, pinned corpus container `sha256:5ff72f3e7f62492123f122a0ecc46a880adc7a71f2566231aee99b7009104fb5`. These allocation counts do not depend on timing stability; no cloud VM restart was needed. Binaries, grammar hashes, source hashes, and input hashes are in the JSON metadata.

## Artifacts and reproduction

- [Full measurements and aggregate totals](memory-results-2026-09-10.json). Each row includes source hashes, both point modes, default/compact slabs, allocation counts, retained requested/usable bytes, and construction peaks.
- [Build and single-file instructions](../README.md#memory-benchmark). The old `tools/memory-pareto` reports are storage estimates and are not the source of these results.
- [`memory.py`](memory.py) runs both saved corpora, verifies grammar/input hashes, prepares mutations, and checks repetitions. Raw local output and binaries are under `build/squat-memory/`.

After the two builds in the README, the measured container invocation is:

```sh
podman run --rm --network=none --cap-drop=all --memory=8g \
  -v "$PWD:/repo:rw" --workdir /repo --entrypoint /usr/bin/python3 \
  sha256:5ff72f3e7f62492123f122a0ecc46a880adc7a71f2566231aee99b7009104fb5 \
  lib/squat/experiments/memory.py --root /repo \
  --loader /lib64/ld-linux-x86-64.so.2 \
  --output /repo/build/squat-memory/results.json
```
