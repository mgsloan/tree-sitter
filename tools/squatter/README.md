# Corpus comparisons and experiments

Run the full offline workflow with the corpus checkout at `../../code-corpora`:

```sh
python3 tools/squatter/run.py --output build/squat-corpus --per-bucket 4 --repeat 3
```

This snapshots the tool sources, stages deterministic source files, compiles local
grammars in the corpus's pinned build image, builds the Rust tools, creates sample
lists, compares both backends on original and mutated inputs, and measures layout
and equality variants. Each operation has its own log and status. Input hashes,
tool snapshot hash, grammar pins and binary hashes, and image identity are kept
in `container-run.json`. Output directories must be new. No checkout is modified.

The convenience runner selects eleven available grammars and a bounded selection
of training and holdout repositories. `--repo` and `--grammar` are repeatable; `--per-bucket` applies
per split, grammar, and size bucket. Files over `--max-file-bytes` (default 4 MiB) are excluded.
Use repeatable `--benchmark` selectors to run a subset.
Coverage counts and missing repositories are recorded. The underlying tools can
operate on the full corpus, with a configurable default limit of 16 MiB per file.
The runner currently requires x86-64 Linux, Podman, Cargo, and the cached corpus
build image; it invokes the container's ELF loader for host-built Rust binaries.

## Individual tools

Inside a compatible grammar container, use its artifact directory directly:

```sh
corpus-analysis sample --code-corpora /corpus --output /out/samplings
squatter-bench --code-corpora /corpus --samplings /out/samplings \
  walk-forward cursor-forward cold-parse train-small --repeat 5 --output example
corpus-analysis memory-pareto --help
```

Alternatively, `--registry registry.json` supplies grammar metadata:

```json
{
  "grammars": {
    "json": {"library": "json.so", "symbol": "tree_sitter_json", "sha": "grammar revision",
      "queries": [{"name": "highlights", "path": "highlights.scm", "sha256": "query hash"}]}
  }
}
```

Library and query paths are relative to the registry file. Optional `library_sha256` is
verified before loading. The built-in suffix map can be replaced by `suffixes`.
Artifact catalogs, when present, restrict loading to entries marked `built`.

Sampling emits `train-*` and `test-*` newline-separated relative-path lists for
tiny, small, normal, large, and unusual. The 100 KiB–1 MiB gap is intentional.
Unusual files add a new parent/field/child combination, an arity threshold
crossing, or a missing-node symbol. Grammar-specific novelty is compared against
tiny/small/normal coverage. Already-selected large files are not duplicated.
Directory symlinks, including the `training -> train` alias, are skipped.

`squatter-bench` accepts benchmark names, sampling names, and file paths as
positionals. It also supports `--all`, `--count`, `--repeat`, `--mutate`, `--seed`,
`--short-circuit`, `--batch-size`, and `--repack`. Named hash domains isolate file
selection, mutations, and seek positions. All repeats use identical bytes.

## Measurement contract

Cold parse always includes a fresh parser; squat additionally converts its parsed
tree. Other benchmarks run each backend over a whole batch before switching;
the first backend alternates by batch and repeat. Visible preorder ordinals
identify nodes across representations. Comparison and identity-map setup are
outside timed regions. Walk timings include recording the supported attributes.

Seek differences are counted but ignored by default at the human's request;
`--strict-seeks` makes them fail again.

Field-lookup API differences are expected only when squat agrees with mainline's
visible-child cursor (with no fields on ERROR parents). Any other field mismatch
fails. `expected_field_differences` records the count in run metadata and each
file's `cold-parse` record, separately from ignored seek differences. Counts are
per checked node/field pair across repeats, not unique nodes; they are collected
by the untimed relationship checks when `cold-parse` is selected. Large trees use
the existing relationship-check sampling stride. Query results remain strict.

Outputs are `NAME-files.jsonl`, `NAME-languages.jsonl`, `NAME-aggregate.jsonl`, and
`NAME-run.json`. File metrics are medians of repeats; ratios are medians of paired
repeat ratios (squat divided by mainline). Summaries report the requested six
percentiles over per-file values. Short-circuiting flushes collected data and
marks summaries partial. Failed files and the first failure are recorded.

Linux hardware counters cover this thread's userspace instructions, cache
references, and cache misses, with multiplexing correction. Restricted kernels
leave those values null and record the reason; zeros are not substituted. Wall
time and process CPU time remain available. Cache event definitions depend on
the CPU; no synthetic cache-hit count is inferred from unlike events.

Layout builds measure actual packing and bytes for 16/32/64 slots and 8/64-byte
column alignment. Sparse grammar IDs, variable-width supertypes, and interleaved
symbol/field storage are explicitly byte estimates, not implemented access paths.
The scan microbenchmark compares scalar, portable SWAR, popcount SWAR, compiler
AVX2, explicit SSE2, and explicit AVX2 with correctness checks and tail handling.

The CLI-control regression uses an existing completed run with a CSS grammar:

```sh
python3 tools/squatter/test-controls.py build/squat-corpus
```

It checks absolute paths, default/strict seek behavior, partial-result flushing,
and repeatable sampling. It needs fresh `controls` and `control-samplings`
subdirectories and the pinned CSS grammar's known seek discrepancy.

## Query comparisons

The runner stages query files from each grammar and matching language definitions
in the corpus's Zed and extension checkouts. The registry records their original
paths and SHA-256 hashes. Query compilation and regex compilation are outside the
timed region. Both compilers must agree on acceptance; jointly rejected queries
are recorded with both errors, and a grammar with no accepted queries fails.
A custom registry must provide query sources when running query benchmarks.

Both engines evaluate built-in equality, regex, and membership predicates against
identical source bytes. Other host predicates are metadata, as in mainline's Rust
bindings. `query-captures` compares full partial-match snapshots, capture indexes,
pattern IDs, and visible node identities in emission order. Collection is timed
for both backends. No query-result differences are ignored. `--unoptimized-query` disables squat
scan/plan shortcuts while retaining the capture coordinator, for ablation runs.

Each query has a 30-second execution timeout. Each file/operation allows four
million captured-node entries in recorded snapshots. Exceeding either budget is
a failed comparison, never a successful truncated result. Generated large files
can require quadratic snapshot storage; start broad query checks with:

```sh
python3 tools/squatter/run.py --output build/squat-queries \
  --max-file-bytes 102400 --per-bucket 2 --repeat 3 --skip-layouts \
  --benchmark query-matches --benchmark query-captures
```

The query layout/ablation matrix reuses a completed source snapshot and its exact
staged bytes. It builds 16/32/64-slot variants and compares each against mainline,
including mutations, the unoptimized 16-slot executor, and repacked slabs:

```sh
python3 tools/squatter/query-variants.py build/squat-queries --repeat 3
python3 tools/squatter/summarize-queries.py build/squat-queries --output query-results.json
```

`query-variants.json` records compiler flags, executable hashes, commands, and
individual pass/fail status. Existing matrix manifests are never overwritten.
The summary checks query and input identities and requires complete passing runs.
Its cross-variant ratios compare separate per-file medians; the mainline/squat
ratios within each run retain the paired-repeat contract.

## Cursor comparisons

The forward workloads separate navigation from attribute decoding:

| Selector | Work timed |
|---|---|
| `cursor-forward` | Native traversal and node identities |
| `walk-forward` | Traversal and all supported attributes |

Both use the ordinary `Cursor` and compare against mainline on identical bytes.
Cursor creation, destruction, and result collection are timed. The attribute
walk uses one bulk FFI call per node; it should not be compared directly with
older measurements using separate node accessor calls. Cached cursors and
reverse traversal workloads have been removed.

```sh
python3 tools/squatter/run.py --output build/squat-cursors \
  --max-file-bytes 102400 --per-bucket 2 --repeat 5 --skip-layouts --skip-sampling \
  --benchmark cursor-forward --benchmark walk-forward
```

Use `--image IMAGE_ID` if the corpus's pinned image is not cached locally.
To include larger files, use `--max-file-bytes 4194304 --per-bucket 1` and a fresh
output directory.
