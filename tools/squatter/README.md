# Squatter checks and benchmarks

## Publishing `organized`

Development happens on `dev`. The `organized` branch contains only squatter,
tree-feller, and their supporting files. Publication merges preserve the previous
`organized` commit as first parent and the selected `dev` commit as second parent.
Use `git log --first-parent organized` to browse publications.

The publisher requires Python 3.11 or newer. Commit the development changes,
then run:

```sh
python3 tools/publish.py check
python3 tools/publish.py prepare
git diff organized..publish/organized
git worktree add ../organized-review publish/organized
cargo test --locked --workspace --manifest-path ../organized-review/Cargo.toml
python3 tools/publish.py publish
```

`prepare` creates the review branch; `publish` fast-forwards `organized` locally.
Neither command pushes. The review worktree can be reused on subsequent runs;
it must be clean when the script advances its branch. An unchanged exported tree
does not create a commit. Source commits containing only excluded changes join
the ancestry with the next changed publication.

`--source`, `--target`, and `--candidate` override `dev`, `organized`, and
`publish/<target>`. For a new output branch, first create it at the desired shared
ancestor with `git branch organized <base>`. An unpublished review branch is never
replaced: choose another `--candidate` or explicitly delete the abandoned branch.
Preparing the same source and target again reuses the pending candidate.

`tools/publish.toml` maps source files and directories to published paths. Directory
mappings include new files automatically. Each export starts from an empty Git
index, so deleted files and old destinations disappear. Moving a source outside
its mapping requires updating the mapping. Missing sources and overlapping
destinations are errors. File modes and symlinks are preserved.

The public root files live in `tools/organized` on `dev`, including a separate
manifest and lockfile. Tree-sitter comes from the revision pinned there; `dev`
continues to use the local fork. To update public dependencies, regenerate the
lockfile in a candidate worktree, copy it to `tools/organized/Cargo.lock`, and
commit it on `dev` before preparing a new candidate.

Exports read committed files, including the mapping and templates. Uncommitted
changes are excluded. Run the exporter version committed at the selected source
revision. Independent changes on `organized` are rejected unless their history
has been incorporated into `dev`; normally make fixes on `dev` and publish again.
Do not merge publication cleanup back into `dev`.

The publisher checks Git state and exported contents; run the Cargo checks on
the candidate before publishing. Its own regression tests use disposable repos:

```sh
python3 tools/publish_test.py
```

## Checks and benchmarks

Rust `xtask` owns corpus selection, staging, grammar builds, and execution:

```sh
cargo xtask squat test quick
cargo xtask squat test corpus --output build/squat-check
cargo xtask squat bench --output build/squat-bench --repeat 5
```

`quick` runs hermetic Rust tests, including native grammar fixtures, slab layout,
parser, query, and persistence checks. `corpus` stages one selection, builds its
grammars, and runs public-API comparisons against Tree-sitter. The inherited-field,
hidden-seek, and Csound regression fixtures are also staged.

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

## Core benchmarks

Squatter uses the Rust core. Build the query, traversal, scanning, and lifecycle
benchmarks with `cargo build --release -p squatter-bench`. Manifests record the
backend.

Query root searches for one to four kinds use specialized SIMD scan kernels;
larger unions use the packed-word search. Presence indexes, scan budgets,
caches, and cancellation remain active.

Byte lookup uses coordinate masks, and point-column scans stop at the first
match. Binary search, immediate returns, empty-boundary descent, and the distant
point-search fallback are preserved.

`core-lifecycle-bench` measures packing alone (cold/reused/dropped scratch), full and safety-only loads,
borrowed/retained loads, compact copying, `to-compacted`, grammar preparation/cache
loading, and query construction, destruction, and disabling. `load-full` includes
an explicit `Forest::validate()` call after safe loading; `load-safety` only runs
the loader's memory-safety checks. Each operation
includes destruction unless named `query-drop` or `query-disable-*`; those exclude
compilation. Compact copying reuses its destination. Mutation/destruction batches
retain at most 16 programs and bound untimed compilation work per sample.
`--workload` restricts operations; `--query` selects a registry query, otherwise
the first supported query is used. Use `--iterations` for equal operation counts
under allocation instrumentation such as heaptrack. Pin these processes externally
and alternate their order. Reports retain raw times, iteration counts, input and
binary hashes, sizes, and the objects kept resident.

`point-access` measures preorder traversal reading both endpoints of every node.
Point data is compressed during packing; accessor timing includes no source lookup.
`--no-points` switches accessor measurements to synthetic points. Use the existing
`seek-point` workload in `squatter-bench` to measure indexed navigation.

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

`cargo test -p tree-squatter` compares Tree-feller output with Tree-sitter packing,
including aliases, fields, extras, empty input, deep trees, and packing options.
It also checks parser reuse, ownership, lexer fallback, concurrent preparation,
unsupported grammars, and the separate Tree-sitter recovery path. Corpus checks
and parsing benchmarks validate direct-parser slabs against Tree-sitter packing.

## Endian compatibility

The Rust probe exchanges all 8 packing variants between the host and a
big-endian PowerPC64 process under QEMU. It checks exact bytes, copied and borrowed
loads, attributes, navigation, and compact copying. It requires Zig, `qemu-ppc64`,
a little-endian host, and the Rust target:

```sh
rustup target add powerpc64-unknown-linux-musl
python3 tools/squatter/endian.py --output build/squat-endian
```

Use `--target-dir` to reuse Cargo artifacts. `--bits 32` uses a 32-bit
little-endian peer and requires `rustup target add i686-unknown-linux-musl`.
Storage version 0 uses 32-slot groups, 16-bit span deltas, and 8-byte column alignment.
Grammars whose symbol and grammar IDs fit in eight bits use separate byte columns;
other grammars use 16-bit symbol codes and an optional 16-bit grammar column.
See [coverage](../../crates/tree-squatter/tests/README.md) for the migrated C checks.

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
reusable sets for N = 1, 2, 4, 8, 16 frequent named kinds. Use
`dynamic_1.{nodes,count,fold}` for single-kind scans; these replace the duplicate
`kind.{nodes,count,fold}` workloads. Field variants are
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
`--kind-selection sparse` selects the most frequent named kinds with at most
`ceil(nodes / 2048)` occurrences. These fit sparse posting lists at every tested
group size when the index is present, and the selection stays identical across
16/32/64 slots. Per-ID frequencies and the threshold are recorded. The
`fixed_N.reverse_nodes` and `dynamic_N.reverse_nodes` workloads traverse the
selected nodes in reverse preorder, with scalar order validation before timing.
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
