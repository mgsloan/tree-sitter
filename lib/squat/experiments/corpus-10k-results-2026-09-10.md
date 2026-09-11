# Byte-only corpus benchmark — partial results, 2026-09-11

Stopped at the user’s request at 2026-09-11T06:59:28.965751+00:00. Recovery was cancelled. This is the aggregate of saved results, not a completed 10,000-file run.

All ten byte-only `squatter-bench` workloads were scheduled on 10,000 selected files, both original and deterministically mutated, with three repeats. 18,550/20,000 file cases completed across 373/400 operations.

9,300 distinct files have saved results: 9,300 originals and 9,250 mutations. 35 completed file cases have comparison or resource-limit failures; 1,450 additional file cases lack complete measurements. Failed workload rows and incomplete repeats are excluded from the performance summaries below; their details remain in the JSON and raw results. The missing cases are Go files, so these partial aggregates underweight Go relative to the planned sample.

## Timing

Ratios are Squatter/mainline; lower is better. “Median” is the median across files of each file’s median paired-repeat CPU-time ratio. “Total” divides the sums of per-file median CPU times, giving larger workloads more weight. Snapshot recording is included in traversal/query measurements; query compilation and correctness comparison are outside those measurements. Cold parse includes mainline parsing plus packing for Squatter.

| Workload | Original valid files | Median | Total | Mutated valid files | Median | Total |
|---|---:|---:|---:|---:|---:|---:|
| cold-parse | 9,300 | 1.460× | 1.473× | 9,249 | 1.349× | 1.417× |
| cursor-forward | 9,300 | 0.566× | 0.654× | 9,250 | 0.550× | 0.645× |
| iterator-forward | 9,300 | 0.473× | 0.578× | 9,250 | 0.458× | 0.574× |
| iterator-forward-cached | 9,300 | 0.479× | 0.578× | 9,250 | 0.464× | 0.575× |
| query-captures | 9,296 | 0.407× | 0.245× | 9,243 | 0.384× | 0.277× |
| query-matches | 9,292 | 0.386× | 0.177× | 9,226 | 0.362× | 0.203× |
| seek-byte | 9,300 | 1.095× | 1.749× | 9,250 | 1.115× | 1.829× |
| walk-forward | 9,300 | 0.720× | 0.714× | 9,250 | 0.713× | 0.711× |
| walk-iterator | 9,300 | 0.714× | 0.696× | 9,250 | 0.710× | 0.692× |
| walk-iterator-cached | 9,300 | 0.655× | 0.656× | 9,250 | 0.653× | 0.651× |

### Combined originals and mutations

| Workload | Valid file cases | Median CPU ratio | Total CPU ratio |
|---|---:|---:|---:|
| cold-parse | 18,549 | 1.402× | 1.443× |
| cursor-forward | 18,550 | 0.559× | 0.650× |
| iterator-forward | 18,550 | 0.466× | 0.576× |
| iterator-forward-cached | 18,550 | 0.472× | 0.577× |
| query-captures | 18,539 | 0.395× | 0.260× |
| query-matches | 18,518 | 0.374× | 0.189× |
| seek-byte | 18,550 | 1.104× | 1.787× |
| walk-forward | 18,550 | 0.717× | 0.713× |
| walk-iterator | 18,550 | 0.712× | 0.694× |
| walk-iterator-cached | 18,550 | 0.654× | 0.653× |

Elapsed-time summaries and results by language are also in the JSON. Hardware counters were unavailable (`Permission denied`), so instruction and cache-event values are absent.

## Storage

| Input | Nodes | Slab bytes | Bytes/node | Live-slot occupancy |
|---|---:|---:|---:|---:|
| original | 21,849,285 | 307,393,064 | 14.07 | 93.9% |
| mutated | 20,994,271 | 295,232,200 | 14.06 | 94.0% |

These are serialized Squatter slab sizes, not a measurement of retained mainline memory.

## Correctness

- mutated `cold-parse`: 1 failing files.
- mutated `query-captures`: 7 failing files.
- mutated `query-matches`: 24 failing files.
- original `query-captures`: 4 failing files.
- original `query-matches`: 8 failing files.

Known seek differences were counted separately (1,863) under the existing benchmark policy. Expected visible-child field differences were also counted separately (4,104). Neither category is reported as a new comparison failure.

Failure paths and affected workloads are listed in `failed_rows` in the JSON. Missing measurements and wall-clock timeouts are listed separately. The first detailed mismatch per failing batch is retained under `unsuccessful_operations`. Every raw batch has its log and run manifest.

Recovery was cancelled before it ran. Two earlier batches exceeded the 30-minute outer timeout, the active batch was stopped by request, and the remaining batches were not started. Their buffered or unmeasured results are absent from the aggregates.

Focused diagnostics (excluded from timing summaries):

- Mutated `is-glob/package.json`: at ordinal 468, a missing zero-width number, mainline returns the following missing `]` as the previous sibling; Squatter returns the preceding comma. All span bases already equal their actual minima, so zero-base packing does not change this input. The exact mutated SHA256 is `48d092804d4d00eb33b210b44f1044ffb3e173db8f08aaafd1b71d179695dd2a`.
- A mutated `date-fns` JavaScript query mismatch also reproduces with `--unoptimized-query`.
- A local debugger probe of Helm `action_test.go` sampled mainline query advancement and capture comparison; both engines ultimately reported a `go/runnables.scm` timeout. Cooperative cancellation can be delayed inside expensive capture comparisons.
- The observed first mismatches in the initial failing Python batches concern empty-capture matches emitted by mainline in `query-matches`. These are recorded as mismatches, not silently ignored.

## Configuration and sample

- Release build: `cargo build --release --locked --no-default-features -p squatter-bench`.
- `SQ_INCLUDE_POINTS=0`; all run manifests verify `point_positions: false`. Row/column storage and APIs are absent; `seek-point` is omitted. Mainline remains its ordinary representation.
- Other defaults: group size 16, column alignment 8, iterator cache mode 2, unpack window 16, automatic ID and coordinate kernels (BMI2 and AVX2 on this Intel VM). No end-column-255 experiment is enabled.
- Google Cloud `squatter-benchmark`, e2-standard-2, Intel Xeon Broadwell, pinned to CPU 0. Backend order alternates; originals/mutations alternate batch order.
- Compiler: gcc (GCC) 15.3.0; rustc 1.95.0 (59807616e 2026-04-14).
- Source snapshot SHA256: `52bf3e5679ba1767459360405aecdb743340adeb83b587f4ed8c4c133614fcda`.
- Binary SHA256: `552654058c5f979b7bbcc9177ca774bf981c9f232703c29ae8fbe8ae79a96a91`.
- Sample: 93,295,261 source bytes; 5,226 training and 4,774 test files from 17 repositories.
- Seed 42; equal quotas per grammar with shortages redistributed, selecting the lowest SHA256(seed NUL path) ranks. Files up to 4 MiB were eligible, including the 100 KiB–1 MiB range. Symlinks and newline paths were excluded. Vendored dependencies are included; file weighting is not project or node weighting.
- Fifty files per saved operation. The first 500 files use hash order; remaining batches group languages to reduce untimed query compilation. Queries retain the 30-second cooperative cancellation threshold and four-million captured-node snapshot budget per file/operation; violations count as failures. Cancellation can be delayed inside expensive capture comparisons. The driver also enforces a 30-minute wall-clock limit per 50-file operation.

| Grammar | Files |
|---|---:|
| bash | 192 |
| c | 252 |
| cpp | 958 |
| css | 1,165 |
| go | 1,147 |
| html | 466 |
| json | 1,164 |
| python | 1,164 |
| tsx | 1,164 |
| typescript | 1,164 |
| yaml | 1,164 |

## Available compile-time options

| Option | Default | Supported settings |
|---|---:|---|
| `SQ_INCLUDE_POINTS` | 1 | 0 omits point data and APIs; 1 includes them |
| `SQ_GROUP_SIZE` | 16 | 16, 32, 64 |
| `SQ_COLUMN_ALIGNMENT` | 8 | 8, 64 |
| `SQ_ITERATOR_CACHE_ALL` | 2 | 0: IDs only; 2: IDs and absolute coordinates; mode 1 was removed |
| `SQ_ITERATOR_UNPACK_SLOTS` | group size | Power of two, at least the group size |
| `SQ_UNPACK_KERNEL` | 0 | 0: auto; 1: scalar; 2: SWAR; 3: BMI2; 4: AVX2 |
| `SQ_COORDINATE_KERNEL` | 0 | 0: auto; 1: scalar; 2: SSE2; 4: AVX2 |

For C builds, use `-DSQ_INCLUDE_POINTS=0` consistently for the library and callers. For Cargo builds, disable the default `points` feature with `--no-default-features`; the build script supplies the matching C definition. Group size and alignment select distinct serialized layouts. Iterator and kernel options affect decoding without changing the stored layout.

`TS_QUERY_EXEC_STATS` and `DEBUG_*` query logging are separate optional diagnostics; they are disabled in this run.

## Artifacts

- Summary and failure details: `corpus-10k-results-2026-09-10.json` beside this report.
- Local inputs, hashes, source snapshot, exact commands, preparation/driver/summary scripts, diagnostics, and raw batch results: `build/squat-corpus-10k/`.
- VM bundle: `/home/mgsloan/squatter-benchmark/corpus-10k/`.
