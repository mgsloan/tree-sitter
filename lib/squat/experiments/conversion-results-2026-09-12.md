# Conversion speedups — 2026-09-12

Two rounds of format-preserving changes to `sq_tree_pack` reduce cloud-measured
conversion time by **14.6% with default packing and 14.2% with compact packing**
against `9d73c40ef`. Neither round changes the serialized representation.

| Round | Change | Default packing | Compact packing |
|---|---|---:|---:|
| 1 | [Frame and staging rework](conversion-frames-2026-09-12.json) | −10.9% | −10.8% |
| 2 | [Single subtree decode, hoisted columns](conversion-decode-2026-09-12.json) | −4.2% | −3.8% |
| | Compounded | −14.6% | −14.2% |

Each round has its own freshly measured paired baseline, so the compounded
figure is the product of two separate comparisons, not one measurement.

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

## Where the remaining time goes

Local sampling of a Python conversion after round 1, filtered to samples under
`sq_tree_pack`: `init_frame` 33.6%, `emit` 25.2%, the packer's own loop 17.5%,
`close_group` 12.4%, `sq_build_presence` 6.3%. IPC is about 3.3 and LLC load
misses are roughly 0.19 per visible node, so conversion is instruction-bound
rather than memory-bound; the productive direction is removing work, not
prefetching. The largest single remaining item is `init_frame`'s child-position
loop, which must compute every child's start because a multiline child's extent
cannot be subtracted.
