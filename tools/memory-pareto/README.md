# Memory Pareto explorer

A storage experiment for the root [Squat design](../../design.md), using this
checkout's Tree-sitter runtime and the precompiled grammars from **code-corpora**.
Corpus selection, fetching, pins, and grammar builds belong to that repository.

The search is deliberately limited to **40 configurations**:

- All seven content fields at u8.
- Seven variants, each widening exactly one field to u16.
- Each variant at 4, 8, 16, 32, and 64 slots per group.

The fields are subtree size, start byte, end byte, start row, end row, start
column, and end column. Ends always use group-maximum-minus-end deltas; other
fields use group minima. There are no combinations, alternative coordinate
encodings, base choices, exception policies, or packing modes. `search.json`
can restrict the capacity sweep; the eight width variants are fixed in code.

## Run locally

Requires Python 3.11+, Git, and working rootless Podman. Initialize the selected
source repositories in code-corpora. Its
`containers/images.lock.toml` supplies immutable local IDs for the build and
grammars images; pull or load those images on the execution host first. Registry
digests can also be supplied with `--build-image` and `--grammars-image`.

```sh
python3 tools/memory-pareto/container.py \
  --code-corpora ~/cozy/code-corpora \
  --directory build/memory-pareto/run-1
```

The launcher builds the analyzer in the code-corpora build image, then copies
it into a derived grammars image. Dependency downloads occur during the build;
the analysis container runs offline. Only staged corpus inputs and the dedicated
result directory are mounted. Source staging copies the selected repositories,
including generated/untracked files, so allow disk space for another corpus copy.
Git metadata is omitted and directory symlinks are never followed during inventory.
The printed runtime image ID can be reused with `--image SHA256_ID`.

Use `--repo ripgrep` (repeatable) for a small run, `--jobs N` for parallel grammar
workers, and a fresh output directory for every run. The container is limited to
8 GiB of memory and N CPUs. The local host must have Podman's rootless prerequisites,
including `newuidmap`/`newgidmap` and subordinate UID/GID ranges.

## Run remotely

Put this checkout, code-corpora, and the images on an SSH host with rootless
Podman, then invoke the same launcher there:

```sh
python3 tools/memory-pareto/container.py \
  --host user@builder \
  --remote-checkout /srv/tree-sitter \
  --code-corpora /srv/code-corpora \
  --directory /srv/results/memory-pareto/run-1
```

Paths are on the remote host. The launcher does not synchronize checkouts or
start a VM. Results remain in the requested remote directory; retrieve them
with `scp -r user@builder:/srv/results/memory-pareto/run-1 ./`.
For an IAP-only machine, run the local command above through its existing cloud
SSH wrapper instead. Use the same tool revision on each execution host.

## Inputs and provenance

Every grammar report, configuration export, variant total, and summary records
`code_corpora_sha`. `results.md` prints it as well. `provenance.json` also records
code-corpora dirty state, selection-file hashes, exact image IDs, tool revision
and source digest, binary hash, grammar catalog hash, classification inputs,
repository pins, missing repositories, and the explicit repository filter.
Reused images retain their own tool revision and source digest.

The image's grammar artifact pins must match `selected-grammars.toml`, and every
loaded parser library must match its artifact checksum. Quarantined, failed,
and absent grammars remain unavailable in the inventory. Grammar compilation
and corpus downloading are not performed by this tool.

Classification uses the checked-in `extensions.json` map for the 17 corpus
languages and common support files. JavaScript/JSX use TSX and C/C++ headers use
C++. The mapping names code-corpora catalog entries (for example `csharp`, whose
parser exports `tree_sitter_c_sharp`). It requires no local Zed installation or
extension clones. `inventory.sqlite` retains unclassified files, unavailable
grammars, symlinks, and read errors.
Identical source bytes are analyzed once per grammar, with occurrence weights
preserved separately for training/test and tracked/generated files.

The default inclusive size cutoff is 4 MiB. The final 3,000 distinct TSX inputs,
ordered by size and content hash, are omitted to retain the latest exploration's
scope. Use `--skip-tsx-tail 0` to include them; a small TSX corpus can otherwise
be excluded entirely. Both exclusions and occurrence weights are saved. Missing
repositories and unclassified files prevent claims of complete corpus coverage;
`complete` in a grammar report refers only to its selected input manifest.

## Storage model and results

The model charges a 16-byte file header and, per group, one u8 count and seven
u32 bases (29 bytes). Counts, bases, and fields each occupy contiguous arrays.
Symbol/field IDs use grammar-derived widths of at least two bits, packed into
non-straddling 64-bit lanes across the entire array. Two built-in error IDs are
reserved after the real symbol range. Five flag bitmaps are charged, as in the
latest exploration. Content arrays are u8/u16 and align once per array.

When any proposed group's value range exceeds its field width, close the group,
charge abandoned slots, and retry the node in a new group. Reports distinguish
headers, occupied bits, overflow/final-group waste, and padding. Lane gaps and
last-word waste are subsets of padding. Slabs beyond the u32 address space are
invalid. The objectives are total bytes, overflow waste, and group-header bytes;
all layouts are exported, including dominated and invalid ones.

This is a storage model, not a codec or CPU benchmark. Extraction uses public
nodes and logical subtree counts; it does not implement the design's physical
slot subtree spans or last-child/hidden flag semantics. Schema-2 canonical length
columns are transformed to absolute ends before grouping. The extraction runtime
is the mainline baseline `072f68c829696687fb01cbc8764e655b3ee942ba`.

Outputs include `results.md`, `summary.json`, `variants.json`,
`configurations.jsonl`, grammar reports, source coverage ledgers, and inventories.
Failed/changed inputs and cancelled runs produce partial results. Each parse has
a 30-second deadline. SIGINT/SIGTERM to the local launcher or coordinator finish
the current input, flush partial aggregates, and leave queued grammars unstarted.
For remote cancellation, signal the coordinator on the execution host.

Summaries can be regenerated without the container or corpus:

```sh
python3 tools/memory-pareto/run_corpus.py summarize --directory PATH_TO_RESULTS
```

## Development checks

```sh
cargo test --manifest-path tools/memory-pareto/Cargo.toml
python3 -m unittest discover -s tools/memory-pareto -p 'test_*.py'
```

Integration tests additionally use `PARETO_BINARY` and `PARETO_JSON_LIBRARY` to
exercise extraction, weighted exports, the catalog-to-summary pipeline, and real
SIGINT/SIGTERM cancellation. Rust tests compare grouping and allocation with a
literal reference and the Pareto sweep with pairwise dominance.

The binary also exposes `extract LIBRARY SYMBOL GRAMMAR OUTPUT SOURCE...`,
`analyze RECORDS SEARCH OUTPUT`, and `run LIBRARY SYMBOL GRAMMAR SOURCES SEARCH OUTPUT`.
Set `CODE_CORPORA_SHA` to the full corpus Git SHA when writing standalone analysis
reports. Output reports use exclusive creation. Generated results are ignored;
no historical measurements are retained here.
