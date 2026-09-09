# Cursor and packed-read refresh — 2026-09-09

The simpler cursor and inlined, specialized packed reads substantially improve
navigation, attribute walks, seeks, and queries on these samples. The improvement
also holds on the files over 1 MiB. Cold parse plus conversion changes much less.

[Machine-readable results](cursor-refresh-results-2026-09-09.json) include
per-file timings, CPU timings, ratio quantiles, input and grammar hashes, run
metadata, and exact commands. The measured runtime is the other session's
uncommitted working tree based on `16cee0f81`, captured before this benchmark.
This report measures that bundle of changes; it does not isolate the benefit of
removing cursor bookkeeping from the benefit of faster packed reads.

## Reading the numbers

Lower ratios are faster. `0.600` means 40% less elapsed time, or 1.67× throughput
for this workload. New/old compares the new ordinary Squat cursor against the
previous ordinary Squat cursor. Mainline means the vendored upstream Tree-sitter
runtime, not the packed engine in `../main`.

Each file has five measured repeats per workload and runtime. New/old is the
ratio of the two separately measured per-file medians, then the median of those
ratios across files. New/mainline uses the harness's paired-repeat ratios, then
the median across files. These are equal-file summaries, not node-weighted totals
or confidence intervals. The JSON also includes ratios of summed timing medians
and the unchanged mainline runtime's timing drift as a control.

## Bounded sample: 88 files, 11 grammars

Saved training and holdout inputs, selected with a 100 KiB limit. Both variants
use the exact same bytes, including deterministic mutations with seed 42.
The query workloads exercise 120 grammar/Zed query sources with 1,407 patterns.

| Workload | New/old, original | New/old, mutated | New/mainline, original | New/mainline, mutated |
|---|---:|---:|---:|---:|
| Cursor navigation | 0.561 | 0.556 | 0.513 | 0.496 |
| Walk with attributes | 0.471 | 0.475 | 0.568 | 0.561 |
| Query matches | 0.689 | 0.686 | 0.373 | 0.363 |
| Query captures | 0.649 | 0.645 | 0.396 | 0.388 |
| Byte seeks | 0.389 | 0.378 | 0.986 | 1.030 |
| Point seeks | 0.289 | 0.291 | 0.837 | 0.899 |
| Cold parse + pack | 0.981 | 0.970 | 1.632 | 1.516 |

## Mixed sample: 53 files, 11 grammars

This sample adds nine inputs over 1 MiB, selected with a 4 MiB limit. It contains
4,741,921 visible nodes on original inputs and 4,426,809 on mutated inputs.
The smaller files still dominate an equal-file median, so see the next table for
the larger inputs alone. Large-file query workloads were not part of this run.

Three original-input navigation rows in this mixed sample regressed by 6–15%
(19-byte CSS, 1,366-byte C, and 4,726-byte Python files). Their mainline controls
also slowed by 15–105%, and all three improved in the separate bounded run.
That suggests run noise, but the regressions remain in the recorded results.
None of the nine large-file navigation or attribute rows regressed by over 5%.

| Workload | New/old, original | New/old, mutated | New/mainline, original | New/mainline, mutated |
|---|---:|---:|---:|---:|
| Cursor navigation | 0.599 | 0.564 | 0.575 | 0.524 |
| Walk with attributes | 0.503 | 0.498 | 0.597 | 0.601 |
| Cold parse + pack | 0.973 | 0.968 | 1.695 | 1.510 |

## Files over 1 MiB: 9 files, 6 grammars

Membership uses original file size for both original and mutated comparisons.

| Workload | New/old, original | New/old, mutated | New/mainline, original | New/mainline, mutated |
|---|---:|---:|---:|---:|
| Cursor navigation | 0.595 | 0.568 | 0.683 | 0.645 |
| Walk with attributes | 0.621 | 0.637 | 0.699 | 0.697 |
| Cold parse + pack | 0.964 | 0.956 | 1.849 | 1.839 |

Absolute five-repeat median times for those nine original inputs:

| Input | MiB | Navigation, old → new (ms) | Attributes, old → new (ms) |
|---|---:|---:|---:|
| test/Chart.js/…/registry.json (json) | 1.06 | 249.48 → 144.27 | 499.12 → 315.20 |
| test/Chart.js/…/parser-typescript.mjs (tsx) | 1.20 | 229.67 → 154.83 | 522.33 → 365.27 |
| test/act/…/lib.dom.d.ts (typescript) | 1.25 | 57.18 → 32.12 | 146.47 → 84.87 |
| test/entt/…/entt.hpp (cpp) | 3.45 | 326.09 → 202.44 | 761.44 → 482.11 |
| train/black/…/dict_huge.py (python) | 1.07 | 203.63 → 121.15 | 464.06 → 271.81 |
| train/nodebb/…/icon-families.yml (yaml) | 1.09 | 59.29 → 38.47 | 166.93 → 97.62 |
| train/nodebb/…/lib.dom.d.ts (typescript) | 2.24 | 71.86 → 36.52 | 201.12 → 124.92 |
| train/nodebb/…/worker-xquery.js (tsx) | 3.34 | 395.14 → 223.62 | 892.38 → 545.70 |
| train/nodebb/…/senticon_en.json (json) | 1.81 | 636.34 → 385.26 | 1132.73 → 794.71 |

All rows include benchmark result recording. Navigation records each visited node's
identity; attribute walks also collect bulk attributes, field IDs, and depth.
Identity-map construction and correctness comparisons are outside the timers.
These timings therefore describe the checked end-to-end traversal workloads,
not isolated C cursor calls. Cold parse includes a fresh mainline parse followed
by slab conversion for Squat, versus a fresh mainline parse alone for mainline.

## Validation and limitations

All eight runs completed with zero comparison failures. Both revisions completed
all selected workloads, five repeats each, on original and mutated inputs.
The report generator checks matching input/tested hashes, grammars, query
registries, arguments, node counts, slab sizes, and group capacities. No slab-size
change occurred between revisions on these inputs.

Known seek differences were counted and ignored under the existing user-requested
policy: 30 original and 70 mutated differing observations per revision in the
bounded sample. Those counts include five repeats; they are not counts of unique
seek positions. The counts agree between revisions. No other comparison failures
were suppressed. The mixed sample did not run seeks.

Measurements ran sequentially on an Intel Core Ultra 7 165U, x86-64 Linux, using
Cargo release builds with Rust 1.95.0 and GCC 15.3.0. Containers had four CPUs and
8 GiB limits. Old/new run order alternated across samples and mutation states;
mainline/Squat order alternated within each run. These convenience samples and
five repeats do not eliminate scheduling, thermal, or tiny-input timer noise.
Hardware instruction and cache counters were unavailable (`Operation not
permitted`); their absence must not be read as zero events.

## Source identity and reproduction

The previous runtime came from the immutable `build/squat-cursors-large/source`
snapshot used for the earlier cursor experiment. Its runtime C sources and Rust
bindings match `16cee0f81`; remaining snapshot differences are documentation and
reports. Both runtime variants were rebuilt with the **current**
`crates/squatter-bench` source, avoiding old/new measurement-harness differences.
The previous cached cursor is not exercised or restored by this experiment.

The new snapshot includes the smaller parent-slot stack, removal of reverse-sibling
bookkeeping and the cached cursor, and the inlined 1/8/16/32-bit packed reads.
Both use the default 16-slot format and ordinary forward cursor.
Full source and binary SHA-256 identities are in the JSON manifest; the working
tree patch was captured with SHA-256
`ce7ad0ff7856b50b3527eb3c75a4c576e1c200851aeb712d11357d5eff09de62`.

Local raw artifacts are retained under
[`build/squat-cursor-refresh`](../../../build/squat-cursor-refresh):

- `current/source` and `previous/source`: exact source snapshots.
- `current/target/release/squatter-bench` and the corresponding previous binary.
- `working-tree.patch`, `driver.py`, and the build logs.
- `revision-run.json`: command lines, source/binary identities, input selection,
  pinned grammars, and operation completion status.
- `bench-outputs/*-files.jsonl`: per-file medians and paired mainline ratios.
- `bench-outputs/*-run.json`: completeness, query, machine, and counter metadata.

The commands mount the saved inputs from `build/squat-cursors` and
`build/squat-cursors-large`; they do not resample the mutable corpus checkout.
Both use the previously cached image identified in the manifest. The image's
loader invokes the host-built binaries explicitly so the host interpreter path
does not select the host's incompatible glibc inside the container.

Regenerate the checked summary into a fresh output file:

```sh
python3 tools/squatter/summarize-revisions.py build/squat-cursor-refresh \
  --output /tmp/cursor-refresh-results.json
```

The saved manifest contains every benchmark command. To repeat measurements,
rebuild the retained snapshots with `cargo build --release --locked -p
squatter-bench`, supplying each snapshot's manifest and target directory, then
replay those commands with fresh output names. The local `driver.py` preserves
the original snapshot/build/run procedure; its output directory must be changed
before rerunning because it refuses to overwrite an existing experiment.
