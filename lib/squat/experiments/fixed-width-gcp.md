# Fixed width versus Squatter main and Tree-sitter on GCP

Results pending completion of the cloud run.

## Setup

GCP `e2-standard-4`, `us-central1-a`: four vCPUs arranged as two physical cores
with two SMT threads each; reported shared L3 is 55 MiB. Benchmark affinity is
CPU 0, and the concurrent pressure worker uses CPU 1, a different physical core.
The existing stopped `squatter-benchmark` instance is reused for this run.

Three representations:

- Tree-sitter runtime from `06c1764ce`.
- Squatter from the clean `../main` checkout at `06c1764ce`.
- This worktree's optimized fixed-width implementation, with the Cargo
  `tree-sitter-squatter/fixed-width` feature enabled.

Both executables use the same Ubuntu 24.04 build image, rustc 1.98.1, GCC 13.3,
and Cargo release settings. Source and executable hashes are recorded under
`build/gcp-fixed-width/`. Neither native build enables CPU-specific compiler flags.

The corpus contains 48 real source files totaling 5,066,350 bytes: three files
per grammar, chosen closest to 5 KB, 35 KB, and 350 KB from the staged/local
corpus. Languages are Bash, C, C++, C#, CSS, Go, HTML, Java, JavaScript/JSX
(using the TSX grammar), JSON, PHP, Python, Ruby, Rust, TypeScript, and YAML.
The 32 smaller files also run each grammar's highlighting and available tags
queries. Rust uses the cached official `tree-sitter-rust` 0.24.2 crate because
the corpus's selected Git pin was unavailable; parser and query hashes identify
that source. Other grammar sources come from the selected local checkouts.

## Conditions and measurement

- **Isolated:** no deliberate cache disturbance.
- **Concurrent pressure:** continuous randomized pointer chasing through
  110 MiB on CPU 1, at 100% duty, while CPU 0 measures the workload.
- **Cache eviction:** one complete randomized traversal of a 110 MiB buffer on
  the benchmark thread before each measurement. Eviction time is excluded.

The pressure working set is twice the reported LLC. Scans perform one full
attribute traversal per timed sample, so repeated warm passes cannot dilute
cache-eviction effects. Both ordinary cursor scans and cached iterator scans
are allocation-free and avoid identity-map bookkeeping. Queries include text
predicates and result materialization. Query compilation and grammar preparation
are excluded. Setup parsing includes parse plus packing for Squatter.

Every configuration has five repeats. The harness rotates Tree-sitter/Squatter
execution order, and the driver alternates the two executable builds between
conditions/workloads. Both layouts retain spare capacity, point information,
and the symbol-presence index. Preflight comparisons cover all 48 traversal
inputs and all 32 query inputs for both Squatter builds.

Timing aggregates use the geometric mean of per-file median ratios. There are
equal file counts per language. Memory is measured separately using requested
retained allocations, excluding shared grammar metadata and parser scratch.

## Results

Pending.

## Artifacts

`build/gcp-fixed-width/` contains both source snapshots, the fixed-width patch,
input and grammar manifests, build logs, binaries, cloud commands, raw results,
and summaries. `prepare.py`, `build.sh`, `run.py`, and `summarize.py` record the
local staging/build process and cloud workload matrix.
