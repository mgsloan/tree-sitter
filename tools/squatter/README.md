# Squatter checks and benchmarks

Rust `xtask` owns corpus selection, staging, grammar builds, and execution:

```sh
cargo xtask squat test quick
cargo xtask squat test corpus --output build/squat-check
cargo xtask squat test sanitize --output build/squat-sanitize
cargo xtask squat bench --output build/squat-bench --repeat 5
```

`quick` runs hermetic Rust and native C tests, including binding tests against the
packaged JSON and C grammars. `corpus` stages one selection and builds grammars once,
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

Ten workloads are available:

- `query-matches`, `query-captures`
- `cursor-forward`, `iterator-forward`
- `scan-forward`, `scan-iterator` (full constant-time attributes)
- `seek-byte`, `seek-point`
- `cold-parse` (fresh parsers and grammar preparation)
- `warm-parse` (reused parsers, prepared grammars, and packing scratch)

Both parsing workloads compare mainline parsing, mainline parsing plus packing,
and tree-feller parsing directly into reverse preorder. Each warm parser gets
one untimed parse per source before measurement; the two mainline paths use
independent parsers. Parse workload and backend order rotate across repeats.
Returned tree destruction and correctness checks are outside timing; cold parser
construction, grammar preparation, and parser destruction are inside timing.

`feller` records each file's status, optional metrics, and paired ratios against
mainline (`ratios`) and mainline plus packing (`pack_ratios`). Ratios below one
favor tree-feller. Unsupported grammars and inputs whose mainline tree contains
errors have explicit skip reasons and null metrics. They contribute no direct
parser timings or ratios. Direct rejection or different compact slab bytes on
valid supported input fails the run. There is no recovery fallback.

Per-file statuses and language, aggregate, and run coverage counts distinguish
success, unsupported grammar, mainline syntax errors, and failure. Language and
aggregate `statistics` compare mainline with conversion across all cases;
`feller_successful` contains all three backends' statistics restricted to cases
where direct parsing and compact slab validation succeeded on every repeat.
Pressure summaries use the same split and reject changed eligibility.

```sh
cargo xtask squat bench --output build/parse-bench --repeat 7 \
  --benchmark cold-parse --benchmark warm-parse
```

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

Timing schema 4 adds the direct parser and warm parsing to schema 3's read
workloads. Without either parsing workload, prerequisite parsing reuses a grammar
pack context and is labeled `setup-parse`; it does not invoke tree-feller.

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

## Direct-parser checks

`cargo test -p tree-squatter` compares tree-feller output with mainline-packed C
trees, including aliases, fields, extras, empty input, deep trees, and packing
options. It also checks parser reuse, ownership, unsupported grammars, and the
separate mainline recovery path. `make -C lib/squat check` tests lexer fallback
and injects allocation failures into parser construction and parsing.

The standalone corpus runner compares complete reverse-preorder slabs with
`repack=true`. On mismatches it compares topology, symbols, fields, positions,
flags, and supertypes. It excludes mainline syntax errors and reports unsupported
grammars separately. It requires a populated code-corpora checkout and a C compiler:

```sh
make -C lib/squat ../../build/squat/feller-corpus
python3 tools/squatter/feller-corpus.py inventory --output build/feller-corpus
python3 tools/squatter/feller-corpus.py compare --output build/feller-corpus
python3 tools/squatter/feller-corpus.py retry --output build/feller-corpus
```

The inventory directory must be new. `--corpus` selects the checkout;
`--executable` overrides the compiled harness. Results are in `summary.json`,
per-grammar JSONL logs, `failures.jsonl`, and `retries.jsonl`. Process exit status
alone does not assert parity. Resource failures are retained in the report.

The port's targeted check compared 1,599 valid inputs across 46 grammars and found
byte-identical slabs throughout. It reused the `../postorder` corpus inventory,
taking the first 50 previously successful files under 512 KiB per grammar, plus
all 201 valid Csound inputs (including its 16 previous position mismatches).
This was a correctness sample, not a full corpus rerun or a timing measurement.
The small Csound regression is `lib/squat/tests/fixtures/csound-header.orc`.
