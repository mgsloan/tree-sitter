# Conversion speedups — 2026-09-12

Five rounds of format-preserving per-node changes to `sq_tree_pack` reduce
cloud-measured conversion time on large inputs by **28.6% with default packing and
27.9% with compact packing** against `9d73c40ef`. None changes the serialized
representation.

| Round | Change | Default packing | Compact packing |
|---|---|---:|---:|
| 1 | [Frame and staging rework](conversion-frames-2026-09-12.json) | −10.9% | −10.8% |
| 2 | [Single subtree decode, hoisted columns](conversion-decode-2026-09-12.json) | −4.2% | −3.8% |
| 4 | [Presence index from whole symbol words](conversion-presence-2026-09-12.json) | −8.2% | −8.0% |
| 5 | [Frameless descent through hidden wrappers](conversion-descent-2026-09-12.json) | −5.1% | −5.0% |
| 6 | Direct ID and flag writes (`c6b8ae461`) | −4.0% | −3.8% |
| | Compounded | −28.6% | −27.9% |

Round 3 removes a cost proportional to the grammar rather than the tree, so it is
invisible on those inputs and worth **−10.3% on batches of small files**. Its
measurements are in [the setup record](conversion-setup-2026-09-12.json).

Each round has its own freshly measured paired baseline, so the compounded
figure is the product of five separate comparisons, not one measurement.

## Method

Run on `squatter-benchmark`, GCP `e2-standard-2` in `us-central1-a`: Intel Xeon
2.20 GHz Broadwell, pinned to CPU 0 through the guest dynamic loader. Binaries
were built with GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`, no LTO, points
enabled, 16-slot groups. The nine inputs are 1.1–3.6 MB files in JSON, TSX,
TypeScript, C++, Python, and YAML, totalling 4,665,634 visible nodes; the
registry selects TSX for the JavaScript inputs.

Each file and packing setting gets five alternating pairs; each measurement is
the median of seven conversions of one already-parsed tree, and the reported
value is the median of those five pair medians. Parsing and tree destruction are
outside the timers. Every pair asserts that both variants produced identical
serialized bytes, sizes, group counts, capacities, node counts, and supertype
counts. This measures warm repeated conversion of one parsed tree, not
end-to-end parse-and-convert or cold-cache behavior, and the JSON files dominate
the node-weighted totals.

Baselines drift between rounds: the same binary measured 72.66 ms on one input
in round 1 and 62.57 ms in round 2. Compare only within a round.

## Round 1: frames and staging

Sampling on the local development machine attributed 9.7% of `init_frame` and
8.5% of the packer's own samples to two `rep stos` sequences. Both came from
compound-literal initializers that zeroed a 112-byte `Frame`: one per raw
subtree, and one built for every childless subtree purely to reach `emit`.

- `Frame` now splits into the `EmitNode` subset `emit` reads and the rest of the
  traversal state, with fields assigned individually. A leaf fills eight fields.
- `distance()` chased `tree->data` and the header for every node; the builder
  mirrors the closed-group slot base instead.
- `set_pending_column` divided by a runtime lane count twice per node. It now
  walks the lane index.
- `emit` assembled a local `Pending` and copied it into the staging array. The
  narrow field stores were immediately reloaded as a 16-byte vector, which the
  local profile showed as the single hottest instruction in `emit` (22.6% of its
  samples). Candidates are now written directly into their staging slot, so a
  full group closes before staging rather than being rejected by `group_fits`.

Every file improved, between 8.9% and 13.0%.

## Round 2: one decode per subtree, hoisted columns

- The `subtree.h` accessors each repeat the inline/heap test. The traversal read
  four fields of each child and `emit` read five, so each raw subtree paid that
  test up to nine times. `child_facts` and the equivalent block in `emit` and
  `init_frame` decode the needed fields in one branch.
- `close_group`'s per-slot loop recomputed `tree->data` and six `tree->layout`
  offsets for every store. Writes through the slab's `uint8_t *` may alias the
  tree, so the compiler could not hoist them. The loop now takes column pointers
  computed once.
- The group flag transpose is branchless, and the symbol-space size that
  `sq_encode_symbol` recomputes per call is cached in the builder.

Every file improved, between 2.4% and 6.3%.

## Validation

Both rounds produce byte-identical slabs. Serialized bytes, sizes, group counts,
capacities, node counts, and supertype counts match the previous implementation
on 212 cases: 53 bounded corpus files across eleven grammars, with both packing
settings and both presence settings. The mainline differential comparison passes
on all eleven grammars, and unit checks pass. Only the seek differences that the
harness ignores by default were reported.

Not yet checked for these changes: sanitizers, allocation-failure injection,
big-endian, and byte-only builds.

## Round 3: the grammar-sized scan per tree

Rounds 1 and 2 targeted per-node work, which the large-input benchmark measures.
A separate cost is proportional to the grammar rather than the tree: for every
`sq_tree_pack`, `allocate_tree` walks all `symbol_count + alias_count` symbols to
collect the supertype list. Sampling a one-line TypeScript file put 25.1% of
conversion in `ts_language_symbol_metadata` and another 24.2% in `allocate_tree`
itself — about half of a small-file conversion in that one scan.

`ts_language_symbol_metadata` is an out-of-line call whose only special cases are
the two builtin error symbols, 65535 and 65534, which this range never reaches.
Reading `language->symbol_metadata[symbol].supertype` directly is equivalent, and
ascending order is preserved because it fixes each supertype's bit position in the
serialized column. `load_bytes` runs the same scan when a cached slab is opened,
so it gets the same treatment; that is the read path the persistence design uses.

Measured on the cloud VM against 2,400 small files (300 per grammar, under 2 KiB)
from the 10,000-file corpus, with the largest files per grammar as a control.
Five alternating pairs per batch; each measurement packs the whole batch of
already-parsed trees, so parsing stays outside the timers.

| Grammar | Symbols | Nodes/file | Small batch | Large batch |
|---|---:|---:|---:|---:|
| typescript | 383 | 21 | −33.4% | −0.4% |
| yaml | 296 | 29 | −24.5% | — |
| python | 274 | 44 | −19.5% | −0.4% |
| cpp | 558 | 90 | −16.5% | +0.3% |
| tsx | 400 | 65 | −14.7% | +0.1% |
| css | 144 | 71 | −5.4% | −0.8% |
| go | 219 | 250 | −2.6% | −1.2% |
| json | 25 | 66 | −1.5% | −0.3% |
| **Total** | | | **−10.3%** | **−0.3%** |

The saving tracks grammar symbols divided by nodes per file, as a fixed per-tree
cost should. Go's "small" files average 250 nodes and already amortize it. The
nine large inputs from rounds 1 and 2 were rerun as a second control and moved by
at most 1.0%, within this VM's run-to-run drift.

Serialized bytes are identical on the same 212 cases. This measures warm repeated
conversion of already-parsed trees; a real small-file workload also pays parsing
and file I/O, which this does not measure.

### The load path gains more than packing

`sq_tree_from_bytes` pays the same scan and nothing else of comparable size, so
opening a cached slab improves more than converting a fresh one. Local medians
over the same 300-file batches, best of nine repeats, `-O3`:

`load_bytes` also ran that scan a second time, purely to size-check the slab: it
counted supertypes itself when `allocate_tree` derives the identical value while
allocating. Allocating first and reading `tree->supertype_count` back removes the
duplicate. The allocation moves ahead of the final size check, so every later
rejection releases the tree; `length` is bounded and `layout.end <= length` is
already verified by then.

Paired alternating medians over the same 300-file batches:

| Grammar | Session start | Direct read | Also deduplicated | |
|---|---:|---:|---:|---|
| typescript | 4.53 µs/file | 1.95 µs/file | 1.67 µs/file | −63% |
| cpp | 10.10 µs/file | 6.45 µs/file | 5.87 µs/file | −42% |
| python | 4.90 µs/file | 3.14 µs/file | 2.86 µs/file | −42% |
| json | 3.67 µs/file | 3.68 µs/file | 3.45 µs/file | −6% |
| **Total** | **23.21** | **15.23** | **13.84** | **−41%** |

This is the operation the persistence design exists to perform. For the same
TypeScript batch, parsing costs 15.7 µs/file, so a cache hit went from 3.5× to
9.4× faster than parsing. Packing itself is only 9-17% of parse-plus-pack on
these files, so the fresh-parse path gains far less: about 1.5-4% end to end.

The deduplication reorders allocation relative to validation, so it was also
checked under ASan, UBSan, and leak detection across four grammars, covering the
malformed, truncated, and unaligned-borrow rejection paths in `compare.c`.

Reproduce the load path with `make -C lib/squat $BUILD/load-bench` and
[load.c](load.c), which serializes each parsed file once and then times repeated
`sq_tree_from_bytes` over the batch. Reproduce a packing batch with
`make -C lib/squat $BUILD/setup-bench` and
[setup.c](setup.c): it parses every listed file, then times repeated conversion of
the whole batch. Pair two builds and alternate their order; one unalternated
`-O2`-versus-`-O3` pair produced a convincing 5-12% phantom regression here.

## Round 4: presence index from whole symbol words

`sq_build_presence` is a full second pass over the packed tree. It decoded every
symbol through the generic per-slot reader, which divides by the lane count and
dispatches on width, then called `ts_language_public_symbol` out of line and
reread the entry's mode bit for each slot. It now decodes each group's lanes from
whole native words, maps stored symbols to public entries through a table built
once per tree, and marks promotion with an occurrence count above the entry's
sparse capacity. Every packed column stores lane i at shift i × bits under a
native load, so one decoder covers all widths on either endianness.

Cloud: −8.2% default (706.9 to 649.2 ms) and −8.0% compact
(715.4 to 658.4 ms); per file between −5.9% and −10.1%. The laptop showed −5.9%.

## Round 5: frameless descent through hidden single-child subtrees

A hidden subtree with one child keeps nothing past that child: the same start
position, no scratch, and `later` unchanged. Each still paid a whole frame —
`init_frame`, a pop for the child, and a final pop. Counting them in the raw
trees: 53-57% of frames on the large TSX inputs, 47% TypeScript, 36% Python, 32%
C++, 22% JSON, 8% YAML. `descend_hidden` walks such chains directly, deriving the
production's alias and direct field for structural child 0 and the wrapper's
supertype bit. Grammars with more than 64 supertypes keep masks in scratch and
still use frames.

Cloud: −5.1% default (650.4 to 617.4 ms) and −5.0% compact
(662.4 to 629.3 ms); per file between +0.8% and −13.2%, with YAML at
+0.8% and −0.2%. The same code inline in the traversal loop measured −4.7% and
−4.5% overall but slowed YAML by 2.8% and 2.5%, so the helper stays out of line.
The laptop disagreed with both — mixed, with YAML +7% — which is why the VM decides.

## Measured and rejected

- **Right-to-left child positions.** `init_frame`'s first touch of each child's
  heap block looked memory-bound, and a redesign could have touched blocks in
  allocation order instead. But per-node conversion is flat across tree sizes
  that fit in cache and those that do not: Python 51.6, 50.1 and 49.8 ns per
  visible node for ~6K-, ~33K- and 456K-node trees. The misses are not headroom.
- **Branch mispredicts.** 0.25-0.35% of branches, a few percent of time; the hot
  loads sit after a data-dependent branch, which is where skid lands.
- **Fixed 16-slot loops in `close_group`,** zeroing abandoned slots so loop counts
  are constant: slower on every one of 18 rows locally, +2.6% overall.
- **A vectorized fit check** over an 8-lane `PackValues`. Byte-identical, and
  GCC vectorizes it at x86-64-v3 into 20 instructions, but at the baseline SSE2
  these builds target it emulates unsigned min/max and executes more: 651.3M to
  666.9M instructions for a Python conversion under callgrind.

## Round 6: direct ID and flag writes

`c6b8ae461` moves packed symbol, grammar, and field writes to node acceptance,
using advancing lane cursors; flags accumulate in the builder. `close_group`
only needs staged coordinates, spans, and supertypes. It also caches language
facts used by frame initialization. Against `cf4c63ed4`, the nine-file cloud
comparison measured 615.88 to 591.32 ms default and 627.59 to 603.50 ms compact
(five alternating pairs of seven-repeat medians). Python conversion instructions
fell from 651.3M to 614.2M. Serialized output matched on 212 cases, forced growth
matched on 53 files, and unit checks passed with points, without points, and with
32- and 64-slot groups.

## Current-head cloud sampling, 2026-09-13

Software `cpu-clock:u` sampling at 997 Hz, pinned to CPU 0, default packing,
points enabled. Hardware cycle events are unavailable on this VM. The existing
`sharedlane-bench` matches current `pack.c` and `index.c`; its SHA-256 is
`11620a05a93de7065b7bc527c44dd4b66fa01c7f6550e39f0de39c979061afbc`.
Python `dict_huge.py` ran 99 timed conversions and TSX `worker-xquery.js` ran 60.
No samples were lost. These are self percentages of all recorded user samples,
not normalized pack-only percentages; startup/unattributed samples remain.

| Function | Python (~4K samples) | TSX (~5K samples) |
|---|---:|---:|
| `init_frame` | 39.11% | 33.22% |
| `emit` | 21.03% | 16.74% |
| `sq_tree_pack` self | 15.29% | 17.38% |
| `descend_hidden` | 4.85% | 8.62% |
| `sq_build_presence` | 4.68% | 4.41% |
| `close_group` | 5.39% | 4.18% |

The latest group-writing change has reduced that target substantially. Frame
initialization remains the largest function. The next experiment is reusable
context on small-file batches, where fixed setup costs are more important than
this large-file profile shows, followed by scalar fit checks and capacity data.

## Historical instruction profile before round 6

Exact callgrind counts inside `sq_tree_pack`, which do not vary with the
laptop's core type or power state: about 714 instructions per visible node for
Python and 802 for TSX. `emit` is the largest function at roughly 230
instructions per call, a third of which is the seven scalar range checks; the
rest is spread thin over prologue, symbol encoding, flag assembly and staging.
`init_frame` runs about 210 per call, with repeated reloads through
`builder->tree->language`. `close_group` spends about 15 instructions per slot per
packed column on lane arithmetic.

## Historical sampling after round 1

Local sampling of a Python conversion after round 1, filtered to samples under
`sq_tree_pack`: `init_frame` 33.6%, `emit` 25.2%, the packer's own loop 17.5%,
`close_group` 12.4%, `sq_build_presence` 6.3%. IPC is about 3.3 and LLC load
misses are roughly 0.19 per visible node, so conversion is instruction-bound
rather than memory-bound; the productive direction is removing work, not
prefetching. The largest single remaining item is `init_frame`'s child-position
loop, which must compute every child's start because a multiline child's extent
cannot be subtracted.
