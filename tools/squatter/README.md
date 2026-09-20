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

Seven workloads remain:

- `query-matches`, `query-captures`
- `cursor-forward`
- `scan-forward` (full constant-time attributes)
- `seek-byte`, `seek-point`
- `cold-parse` (mainline parse versus parse plus one-shot packing)

Select workloads with repeated `--benchmark NAME`. Cursor construction
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

## Pressure and results

The matrix in `matrix.toml` defines isolated, carousel, wash, and bursty pressure.
Use repeated `--pressure-profile NAME`, with optional `--pressure-bytes`,
`--benchmark-cpu`, and `--pressure-cpu`. Carousel rotates through source batches;
wash traverses a randomized buffer; bursty runs a concurrent tenant. Default
pressure size is twice the detected LLC, falling back to 32 MiB. Put a tenant on
a different physical core sharing the benchmark core's LLC.

`container-run.json` records source/input/grammar hashes, selection coverage,
commands, logs, and operation status. The benchmark writes per-file, per-language,
and aggregate JSONL plus a run manifest. Metrics include wall time, thread CPU
time, and hardware counters when permitted. Incomplete/failed runs remain marked.

```sh
python3 tools/squatter/summarize.py build/squat-bench --output build/summary.json
```

The summarizer rejects failed runs and mismatched pressure comparisons. Completed
query matches compare exactly; capture checks require coverage of completed
matches while permitting provisional events and different order. Descendant seeks
compare exactly with mainline. Field differences are accepted only when Squatter
agrees with mainline's visible-child field lookup.

## Group-scan throughput

`scanning-bench` measures the Rust scan prototype in nodes/s, including forward
and reverse preorder/postorder, `all`, native traversal baselines, population
counts, kind/field filters, and byte-range scans:

```sh
cargo run --release -p squatter-bench --bin scanning-bench -- \
  --registry build/scanning-bench/registry.json \
  --inputs build/scanning-bench/inputs.json \
  --corpus ../main/build/squat-corpus-10k/corpus \
  --cpu 2 --samples 7 --sample-ms 60 \
  --output build/scanning-bench/results.json
```

The input manifest is a JSON array with `path`, `grammar`, and `sha256` fields.
Paths are relative to `--corpus`; the registry uses the corpus-analysis format.
The benchmark checks hashes and traversal/filter results before timing. It
includes scan construction, excludes parsing and packing, and consumes each
enumerated node with `black_box`. Count workloads consume the aggregate only.

Scalar filter workloads use `next_preorder()` with per-node property checks.
The scalar traversal and mainline cursor provide independent baselines; the
removed C iterator is no longer benchmarked. Scalar-filter timings are not
directly comparable with earlier results that used that iterator.

Each timed iteration cycles through all input trees. A pilot chooses the iteration
count targeting `--sample-ms`; workload order rotates across samples. CSV goes to
stdout, while JSON retains individual timings and input/grammar metadata.
Use `--reverse-workloads` to reverse the order for a repeat run.
Filtered throughput uses all input nodes as its denominator, including nodes
skipped by range/group operations; output nodes/s and match counts are also saved.
The kind filter selects each file's most frequent named kind; `multi_kind` uses
its four most frequent named kinds, adjustable with `--kind-count N`. The field filter
selects its most frequent nonzero field, or zero if none exists. The byte range
covers the middle 1% of each source by default. `--range-start-percent` selects
its start (0–100), and `--range-percent` selects its width (1–100), clipped at EOF.
For example, `--range-start-percent 0 --range-percent 100` scans the full source.
Byte and point queries cover the same interval. Use repeated `--workload NAME`
arguments to time selected operations. Results describe this selected corpus
and cache behavior, not parsing performance.

`range` and `point_range` workloads measure overlap. `within`, `starting_in`,
and `starting_at` (also prefixed with `point_`) provide `.nodes`, `.fold`, and
`.count` consumers. Exact-start queries use the range's start. Overlap also has
`.reverse_nodes` workloads. Setup validates selected nodes against scalar
accessors before timing.

Forward/reverse fold and grouped-fold workloads also consume every node with `black_box`.
`flags.count` excludes extra and missing nodes; `combined.count` additionally
intersects the selected kind and field. Supertype workloads select the grammar's
first supertype, or an invalid ID when the grammar has none. Their input-node
denominator includes those grammars; per-file supertype IDs and match counts are
recorded so this early-rejection effect is visible.

`fixed_N.{nodes,count,fold}` and `dynamic_N.{nodes,count,fold}` compare arrays and
reusable sets for N = 1, 2, 4, 8, 16 frequent named kinds. Field variants are
`fixed_field_N`, `dynamic_field_N`, and `scalar_field_N`, for N = 1, 2, 4 frequent
nonzero fields (or zero when none exists). If fewer distinct IDs are present,
arrays repeat the most frequent ID; dynamic sets deduplicate the same selection.
Per-file arrays and match counts are recorded. Both paths include scan/predicate
preparation in timing; constructing reusable dynamic sets is setup work.

`range.fixed_N.{nodes,count}` and `range.dynamic_N.{nodes,count}` combine byte
overlap with one or four frequent named kinds. The `point_range`, `within`, and
`point_within` prefixes provide point-coordinate and within selections.
Range selection runs before the kind filter. Setup validates each combination's
membership against scalar accessors and records per-file match counts.
