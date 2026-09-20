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

Eight workloads are available:

- `query-matches`, `query-captures`
- `cursor-forward`
- `scan-forward` (full constant-time attributes)
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
nonzero fields (or zero when none exists). If fewer distinct IDs are available,
arrays repeat the first selected ID; dynamic sets deduplicate the same selection.
Per-file arrays and match counts are recorded. Both paths include scan/predicate
preparation in timing; constructing reusable dynamic sets is setup work.

`range.fixed_N.{nodes,count}` and `range.dynamic_N.{nodes,count}` combine byte
overlap with one or four frequent named kinds. The `point_range`, `within`, and
`point_within` prefixes provide point-coordinate and within selections.
Range selection runs before the kind filter. Setup validates each combination's
membership against scalar accessors and records per-file match counts.

`--kind-selection rare` selects the least frequent named kinds instead;
`--kind-selection absent` selects valid grammar IDs absent from each input,
preferring named IDs. `--no-symbol-index` disables index construction during
packing. `field.{fixed,dynamic}_N.{nodes,count}` filters by field before 8/16
selected kind IDs; `range.{fixed,dynamic}_8.{nodes,count}` covers byte overlap
followed by eight IDs. These combinations exercise sparse candidate masks.

Composed workloads use 2/4/8/16 IDs, arrays and sets, and `.nodes`/`.count`:

| Prefix | Filter order |
| --- | --- |
| `field` / `kind_field` | Field then symbols / symbols then field |
| `flags` | Exclude extra and missing nodes, then symbols |
| `range_field` / `range_kind_field` | Byte overlap, then both field/symbol orders |
| `intersection` / `intersection_reverse` | Two symbol filters, in both orders |

The second symbol set selects alternate entries from the sixteen selected IDs;
its intersection with the first set varies with cardinality. Per-file IDs and
counts are recorded. Setup checks each composed enumeration against scalar node
accessors, and every timed count must match the scalar total. Use both narrow and
broad byte windows and `--no-symbol-index` to distinguish sparse-mask, SIMD, and
index effects.
