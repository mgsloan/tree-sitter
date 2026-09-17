# Squatter checks and benchmarks

Rust `xtask` owns corpus selection, staging, grammar builds, and execution:

```sh
cargo xtask squat test quick
cargo xtask squat test corpus --output build/squat-check
cargo xtask squat test sanitize --output build/squat-sanitize
cargo xtask squat bench --output build/squat-bench --repeat 5
```

`quick` runs hermetic Rust and native C tests, including binding tests against the
packaged JSON grammar. `corpus` stages one selection and builds grammars once,
then runs the C internal checks and Rust public-API comparisons against it.
`sanitize` uses the same staging path for C checks under ASan/UBSan. C query edge
cases use their bounded synthetic fixtures; Rust checks use actual grammar/Zed
queries. The inherited-field and hidden-seek regression fixtures are also staged.

Corpus runs require Podman, the code-corpora checkout (default
`../../code-corpora`), and its cached build image. Use `--image` to select another
installed build image. Containers run offline. Every output directory must be new.
`--repo` and `--grammar` are repeatable selectors. `--per-bucket` defaults to one
file per split/language/size bucket for checks, four for benchmarks.
`--max-file-bytes` defaults to 4 MiB. Selection is seeded and independent of
filesystem enumeration order; the intentional 100 KiB–1 MiB gap remains.
Rust checks and benchmarks include original and deterministically mutated inputs; `--skip-mutated`
omits the latter. Structural sampling remains a separate `corpus-analysis sample`
command and is not run automatically.

## Measurement contract

Nine workloads remain:

- `query-matches`, `query-captures`
- `cursor-forward`, `iterator-forward`
- `scan-forward`, `scan-iterator` (full constant-time attributes)
- `seek-byte`, `seek-point`
- `cold-parse` (mainline parse versus parse plus one-shot packing)

Select workloads with repeated `--benchmark NAME`. Cursor/iterator construction
is timed. Read kernels consume results with `black_box`, without benchmark result
vectors, identity-map lookups, or checksums. Query engine allocations remain part
of the workload. Exact traversal/attribute/seek comparisons and query snapshots
are produced and checked before timing, then discarded. Timed traversal counts
and execution failures are checked after timing. Checks are not timing results.

`--traversal-iterations N` repeats navigation/attribute traversals within a
measurement. Workload and backend order rotate. Pressure is applied after
validation and before each timed operation. Byte/point seeks consume all sampled
positions; queries consume the complete selected stream. Parse results are
validated after their construction, outside the timer.

Timing schema 3 replaces the earlier allocating-walk and digest workloads;
its navigation, query, and seek timings are not directly comparable with old
results. Without `cold-parse`, prerequisite parsing reuses a grammar pack context
and is labeled `setup-parse`.

## Pressure, layouts, and results

The matrix in `matrix.toml` defines isolated, carousel, wash, and bursty pressure.
Use repeated `--pressure-profile NAME`, with optional `--pressure-bytes`,
`--benchmark-cpu`, and `--pressure-cpu`. Carousel rotates through source batches;
wash traverses a randomized buffer; bursty runs a concurrent tenant. Default
pressure size is twice the detected LLC, falling back to 32 MiB. Put a tenant on
a different physical core sharing the benchmark core's LLC.

`--layouts` additionally builds and measures the matrix's storage layouts.
The remaining C probes measure actual layout/packing, retained allocations and
construction peaks (`memory.c`/`memory.py`), and slab loading (`load.c`).

`container-run.json` records source/input/grammar hashes, selection coverage,
commands, logs, and operation status. The benchmark writes per-file, per-language,
and aggregate JSONL plus a run manifest. Metrics include wall time, thread CPU
time, and hardware counters when permitted. Incomplete/failed runs remain marked.

```sh
python3 tools/squatter/summarize.py build/squat-bench --output build/summary.json
```

The summarizer rejects failed runs and mismatched pressure comparisons. Completed
query matches compare exactly; capture checks require coverage of completed
matches while permitting provisional events and different order. The known seek
difference is accepted only for `hidden-seek.css` (direct harness runs can use
`--strict-seeks`). Field differences are accepted only when Squatter agrees with
mainline's visible-child field lookup.
