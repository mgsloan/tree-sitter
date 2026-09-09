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
of repositories. `--repo` and `--grammar` are repeatable; `--per-bucket` applies
per grammar and size bucket. Files over 4 MiB are excluded from this quick run.
Coverage counts and missing repositories are recorded. The underlying tools can
operate on the full corpus, with a configurable default limit of 16 MiB per file.
The runner currently requires x86-64 Linux, Podman, Cargo, and the cached corpus
build image; it invokes the container's ELF loader for host-built Rust binaries.

## Individual tools

Inside a compatible grammar container, use its artifact directory directly:

```sh
corpus-analysis sample --code-corpora /corpus --output /out/samplings
squatter-bench --code-corpora /corpus --samplings /out/samplings \
  walk-forward walk-backward cold-parse train-small --repeat 5 --output example
corpus-analysis memory-pareto --help
```

Alternatively, `--registry registry.json` supplies grammar metadata:

```json
{
  "grammars": {
    "json": {"library": "json.so", "symbol": "tree_sitter_json", "sha": "grammar revision"}
  }
}
```

Library paths are relative to the registry file. Optional `library_sha256` is
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

Reverse walks enumerate siblings forward and consume them in reverse for both
backends. This avoids inconsistent fields in mainline's reverse cursor while
charging the same adapter cost to both. Native packed reverse cursors are tested
separately. Seek differences are counted but ignored by default at the human's
request; `--strict-seeks` makes them fail again.

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
