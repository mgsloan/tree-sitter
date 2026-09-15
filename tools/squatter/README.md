# Squatter tests and benchmarks

The supported infrastructure has one public command surface:

```sh
cargo xtask squat test quick
cargo xtask squat test corpus --output build/squat-check
cargo xtask squat test sanitize --output build/squat-sanitize
cargo xtask squat bench -- --output build/squat-run --repeat 5
```

`quick` runs the hermetic Rust, native C, and Python tests. `corpus` runs both
native and Rust comparisons against bounded, deterministically staged inputs.
`sanitize` runs the native corpus contract under ASan and UBSan. Corpus and
benchmark output directories must be new.

The implementation consists of:

- `corpus-analysis`, which owns corpus inventory, grammar registries,
  deterministic sampling and mutation, and the memory Pareto model.
- `squatter-check`, the untuned correctness entry point shared with the
  benchmark workloads.
- `squatter-bench`, the paired mainline/Squatter measurement executable.
- `run.py`, the sole Podman/staging driver.
- `summarize.py`, the sole supported result summarizer.
- `matrix.toml`, the declarative grammar, repository, layout, and pressure
  matrix.

One-off encoding probes and revision-specific cloud packaging scripts are kept
in Git history rather than maintained as repository tools.

## Corpus runner

Run the default isolated matrix with the corpus checkout at
`../../code-corpora`:

```sh
python3 tools/squatter/run.py --output build/squat-run --per-bucket 4 --repeat 3
```

The runner snapshots tracked tool sources, stages deterministic source files,
compiles pinned grammar libraries in the corpus build image, and runs offline
containers. `container-run.json` records input hashes, source and grammar
identities, the exact copied matrix, commands, and operation status. `--repo`,
`--grammar`, and `--benchmark` are repeatable selectors.

The matrix defines four pressure profiles:

- `isolated`: no deliberate cache disturbance.
- `carousel`: rotate actual workloads through files totaling twice the LLC size.
- `wash`: one traversal of a randomized working set before each measurement.
- `bursty`: a randomized concurrent tenant active for 10% of each 10ms quantum.

Run a paired sensitivity matrix with:

```sh
python3 tools/squatter/run.py --output build/squat-pressure --skip-layouts \
  --pressure-profile isolated --pressure-profile carousel \
  --pressure-profile wash --pressure-profile bursty \
  --benchmark-cpu 0 --pressure-cpu 2 --repeat 7
python3 tools/squatter/summarize.py build/squat-pressure \
  --output build/squat-pressure-summary.json
```

Unless `--pressure-bytes` is supplied, pressure modes use twice the LLC size
reported by Linux sysfs, falling back to 32 MiB. Carousel uses aggregate source
bytes as a stable proxy for its logical resident set; wash and tenant allocate
their buffer once. The summarizer reports each backend's pressured/isolated
slowdown and `Squatter slowdown / mainline slowdown`; values below one mean that
Squatter retained more performance under pressure.

Use different physical cores that share an LLC. Do not place the tenant on an
SMT sibling when the goal is shared-cache rather than execution-unit pressure.
Affinity is Linux-only and is recorded in every run manifest.

`--checks-only` is an internal runner mode used by `xtask`: it invokes
`squatter-check`, forces one repeat, and supports only the isolated profile.

## Workloads and measurement

The shared workload set is:

- `query-matches` and `query-captures`
- `walk-forward` and `cursor-forward`
- cached and uncached iterator navigation/attribute walks
- allocation-free cursor and cached-iterator attribute digests
- `seek-byte` and, in point-enabled builds, `seek-point`
- `cold-parse`, comparing parse against parse plus one-shot packing

The digest workloads avoid result vectors and identity maps so cache experiments
measure tree traversal rather than benchmark bookkeeping. Use
`--digest-iterations` to put repeated editor-like passes inside one measurement,
after a single pressure event. Without an explicit `cold-parse` selector,
prerequisite parsing uses a reusable
per-grammar `PackContext` and is reported as `setup-parse`. Workload and backend
order rotate across batches and repeats. Comparisons use visible preorder
ordinals, and query results remain strict.

The single known seek discrepancy is permitted only for the checked-in
`hidden-seek.css` fixture unless `--strict-seeks` is used. Other seek differences
fail; there is no blanket seek-error suppression.

Every measurement reports wall time, benchmark-thread CPU time, and, when Linux
permits them, thread-scoped userspace instructions, cache references, and cache
misses. Pressure-worker instructions and CPU time are not charged to the
benchmark thread. Hardware cache-event definitions remain CPU-specific.

Each invocation writes `NAME-{files,languages,aggregate}.jsonl` and
`NAME-run.json`. File ratios are paired within repeats. Incomplete and failed
runs retain partial data and explicit status.

## Native tests and retained microbenchmarks

`make -C lib/squat check` runs hermetic packed-column and supertype tests. The
container corpus check additionally runs structural comparison, queries, exact
seeks, and pack-context reuse for each staged grammar.

Only durable native probes remain under `lib/squat/experiments`:

- `layout.c`: packing time, occupancy, and actual layout bytes.
- `memory.c`/`memory.py`: retained allocations and construction peaks.
- `load.c`: deserialization cost from an existing slab.
- `scan.c` and `unpack.c`: explanatory scan/decoder microkernels.

Top-level performance decisions should be based on `squatter-bench`; native
microbenchmarks are diagnostic evidence.
