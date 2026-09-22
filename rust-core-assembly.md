# C and Rust packing assembly

The per-node Rust loop reads Tree-sitter's private subtree representation
directly. It does not call the public node or cursor API. The remaining costs
are in packing code generation: helper boundaries, metadata staging, address
reloads, and bounds checks.

This comparison uses the linked portable x86-64 release executables, with
16-slot groups: GCC 15.3.0 for C and Rust 1.95.0 / LLVM 22.1.2 for Rust. These
are Cargo's ordinary release builds, without the separate `optimize` profile
or a native CPU target. C is unchanged from `fa2e389641c6`; the previous batched
Rust implementation is `d1712fee2918`. Current fused Rust is `288f1139e`.

The [assembly manifest](build/fused-packing/assembly/manifest.json) records
executable hashes, symbol sizes, and call sites. The files opened in Zed are
[C](build/fused-packing/assembly/c.asm.txt),
[current Rust](build/fused-packing/assembly/rust.asm.txt), and
[initial fused Rust](build/fused-packing/assembly/rust-initial.asm.txt).
[Batched Rust](build/fused-packing/assembly/rust-batched.asm.txt) is retained too.
Addresses below refer to these immutable files.

## Main differences

| Area | C | Current fused Rust | Implication |
| --- | --- | --- | --- |
| Tree access | Direct subtree loads; public API calls during setup | Direct subtree loads; public API calls during setup | Public Rust node/cursor wrappers are absent from the per-node path. |
| Node emission | `pack_tree → emit → emit_values` | Emission is inlined into `pack`; `mask_id` and `Builder::extend` remain calls | Fusion removes the extra Rust emission layer, but helper calls remain in both implementations. |
| Frame removal | Emits through a pointer into the frame stack | Emits through a borrow into the frame vector | The initial Rust version's 128-byte copy on each frame pop is gone. |
| Small supertype masks | Small-mask branch inside `emit_values`; dictionary call only for larger sets | Calls `mask_id` even for the direct-mask case | Rust enters a six-register save/restore prologue before testing the small-mask case. |
| Group closing | Caches slab and column addresses before the slot loop | Repeatedly reloads the slab pointer between column stores | A concrete opportunity to give Rust's encoder stable column addresses. |
| Bounds checks | No corresponding array checks | Checks remain for pending slots and position-arena indexing | Some can likely move outside loops through slice iteration; this does not justify unchecked indexing throughout the walker. |
| Frame initialization | Writes into its reserved stack slot | Writes frame fields into the vector after preparing them | Rust has more local stack storage and reloads across helpers; the current push has no full-frame `memcpy`. |

## Calls and copies

The initial fused Rust build outlined three helpers for each emitted node:

```text
pack → Walk::emit → Walk::emit_values → Builder::emit
```

The current build marks those emission helpers for inlining. Their separate
symbols disappear, but LLVM now outlines group fitting and mask lookup:

```asm
19db3d: call 19d310 <...Walk::mask_id>
...
19dbfc: call 19c330 <...Builder::extend>
```

These are two representative call sites; the parent and leaf paths each have
their own copies. Group closure, growth, and error handling add conditional
calls. Counting static call instructions is not a count of calls per input node.

The initial frame pop copied 128 bytes from the vector into a local frame using
eight pairs of vector loads and stores, at `19d88b–19d8ea`. Emitting while the
frame is borrowed, then discarding the pop result, removes this sequence. It
preserves the frame's arena marks until emission completes or fails.

Inlining also increases register pressure. The Rust traversal's fixed local
stack allocation grows from 280 to 344 bytes. This is a tradeoff to measure,
not evidence that more inlining is always better.

## Group-closing loop

Rust reloads `data.bytes` through `%r8` for consecutive column writes:

```asm
19c1d2: mov %cl,(%r11,%rbx,1)  # span delta
...
19c1db: mov 0x10(%r8),%rcx     # reload slab pointer
19c1df: add 0x50(%rsp),%rcx
19c1e4: mov %r14b,(%r11,%rcx,1)
...
19c1eb: mov 0x10(%r8),%rcx     # reload again
```

C computes column pointers before its loop and stores through those registers:

```asm
12d6fc: mov %cl,(%r11,%rdx,1)  # span delta
...
12d709: mov %cl,(%r10,%rdx,1)  # start-byte delta
...
12d717: mov %cx,(%r9,%rdx,2)   # end-byte delta
```

The source already documents C's aliasing reason for caching these addresses.
The Rust assembly exhibits the same problem despite the owning `Tree` being
borrowed mutably: its raw byte stores do not give the optimizer the required
disjointness. This is an inference from the emitted reloads and source, not a
measurement of their individual cost.

Rust also checks the pending-array bound inside this loop (`19c180–19c187`).
Iterating an already bounded prefix could make that one check per group.

## Sizes and stack allocations

| Function | C code bytes | Rust code bytes | C fixed local stack bytes | Rust fixed local stack bytes |
| --- | ---: | ---: | ---: | ---: |
| Frame initialization: `init_frame` / `Walk::push` | 1,920 | 1,974 | 104 | 216 |
| Group close | 927 | 1,034 | 8 | 104 |
| Traversal: `pack_tree` / `pack` | 3,338 | 4,278 | 952 | 344 |

Stack figures are explicit fixed `sub ..., %rsp` allocations, excluding saved
registers, return addresses, and temporary stack arguments. The traversal row
has different scope: C also owns its builder, allocation, and finalization in
that function; Rust receives its builder by reference. It cannot establish
which implementation has lower total stack use. Code sizes are not timings.

## Follow-up candidates

1. Keep the small-mask fast path inline and outline only dictionary lookup.
2. Expose stable slab/column addresses for group closing, and iterate a bounded
   pending prefix to avoid checking each slot.
3. Revisit group-fit inlining and frame initialization after those narrower
   changes; monitor spills and duplicated code as well as call elimination.

These follow-ups are absent from the `288f1139e` assembly above. The next section
examines slab-pointer caching; the other candidates remain open. Conversion
remains entirely in Rust. [rust-core-results.md](rust-core-results.md) records
cloud timings and validation separately; assembly alone does not establish a
speedup or explain the whole cold-parse time.

## Slab-pointer follow-up

`TreeData::writer` now returns a `SlabWriter` that captures the slab pointer by
value. Its lifetime borrows the descriptor mutably, excluding resizing or
replacement until the last write. `Builder::close` uses one writer for its slot
loop. The existing byte/short write methods delegate to the same implementation,
preserving unaligned little-endian stores and the trusted-layout contract.

The writer introduces no allocation or out-of-line helper calls. It removes the
descriptor reloads from the loop without changing pending-array iteration,
group-fit decisions, traversal, or supertype lookup. The compiler retains some
cached addresses and values on the stack.

| Group-close loop | Before | After |
| --- | ---: | ---: |
| Slab-pointer loads per slot, with points | 6 | 0 |
| Instructions per slot, with points | 49 | 44 |
| Slab-pointer loads per slot, without points | 4 | 0 |
| Instructions per slot, without points | 28 | 23 |
| Instructions referencing stack memory, with / without points | 13 / 6 | 13 / 6 |
| Fixed local stack allocation | 104 bytes | 104 bytes |
| Entire group-close function | 1,034 bytes | 1,068 bytes |

Counts cover one loop body including its back edge on the normal path. They
exclude entry, exit, and error paths, and do not measure cycles or cache misses.
The extra setup increases total code size despite the smaller slot loops. All
other inspected packing helpers retain their previous code sizes.

The new points loop loads cached column addresses from stack slots, rather than
following the slab pointer in the descriptor again after each store:

```asm
19c20d: mov 0x30(%rsp),%rcx
19c212: mov %r11b,(%rcx,%rbx,1)  # span delta
...
19c21a: mov 0x38(%rsp),%r11
19c21f: mov %dl,(%r11,%rbx,1)    # start-byte delta
```

[Before](build/slab-writer/assembly/before.asm.txt),
[after](build/slab-writer/assembly/after.asm.txt), and
[C](build/slab-writer/assembly/c.asm.txt) retain the complete function bodies.
The [manifest](build/slab-writer/assembly/manifest.json) identifies executables;
[loop counts](build/slab-writer/assembly/loop-counts.json) and
[symbol sizes](build/slab-writer/assembly/symbol-sizes.json) retain the static
comparison. Benchmark results are recorded separately in
[rust-core-results.md](rust-core-results.md).

## Four packing follow-ups

Starting from `cc783db89`, all four remaining candidates were built separately,
then tested together with each change removed in turn. The retained code is
`d96b0793d`; each optimization has its own commit:

| Change | Commit | Assembly result |
| --- | --- | --- |
| Inline small-mask handling | `44b8fdd63` | Grammars with at most eight supertypes skip the lookup call and its six saved registers. Dictionary lookup stays outlined. |
| Iterate a bounded pending prefix | `0e906b68a` | One bounds check per group replaces the per-slot checks; the no-points loop also loses its stack-memory accesses. |
| Inline group fitting | `d12c23835` | Candidate values pass directly through the fit check; the separate `Builder::extend` call disappears. |
| Initialize retained frames earlier | `d96b0793d` | Frame metadata is stored before scratch allocation and mask lookup, reducing live locals across calls. |

The mask wrapper is forced inline, while `lookup_mask` retains the generic hash
and probing path. Its body falls from 408 to 369 bytes. The small-mask case no
longer enters that helper. Group-fit inlining increases traversal code size;
the table below includes that tradeoff rather than treating call removal as an
automatic improvement.

| Traversal build | Code bytes | Fixed local stack bytes |
| --- | ---: | ---: |
| Baseline | 4,278 | 344 |
| Small-mask change alone | 4,224 | 328 |
| Group-fit change alone | 5,138 | 280 |
| All four | 5,239 | 312 |

The frame rewrite pushes an initialized `Frame` before filling child metadata
and positions. It uses safe Rust and retains normal vector allocation. If a
later allocation or lookup fails, `Walk::drop` clears the frame along with the
other scratch state. `Walk::push` shrinks from 1,974 to 1,491 code bytes and from
216 to 120 fixed stack bytes. Its emitted code is unchanged by the other three
candidates.

| Group-close loop | Baseline | Bounded prefix |
| --- | ---: | ---: |
| Instructions per slot, with points | 44 | 40 |
| Instructions per slot, without points | 23 | 17 |
| Stack-memory instructions, with points | 13 | 10 |
| Stack-memory instructions, without points | 6 | 0 |
| Fixed local stack bytes | 104 | 104 |
| Entire function code bytes | 1,068 | 1,061 |

These counts include the normal loop back edge and exclude setup and error
paths. The bounded-prefix change produces the same loop counts independently
and in the combined build. It preserves the cached slab writer.

The [experiment report](build/packing-candidates/report.html) records cloud
results, including removal experiments that test whether each change still
helps in combination. [Baseline assembly](build/packing-candidates/baseline/assembly.txt),
[selected assembly](build/packing-candidates/selected/assembly.txt),
[symbol sizes](build/packing-candidates/selected/assembly.json), and
[loop counts](build/packing-candidates/loop-counts.json) retain the static evidence.
Conversion remains entirely in Rust.

## C emission and metadata follow-ups

Two C experiments start from `487670d5a`, whose C implementation is unchanged
from `fa2e389641c6`. `1e9c80d16` retains forced inlining of `emit` and
`emit_values`. The compiler-specific annotation is local to `pack.c`, with an
ordinary-inline fallback. The linked traversal no longer calls either helper
per node. Group closing, dictionary lookup, and frame initialization remain
conditional calls.

| Area | Baseline C | Inline emission | Capture metadata alone |
| --- | ---: | ---: | ---: |
| `pack_tree` code bytes | 3,338 | 7,448 | 3,338 |
| `pack_tree` fixed local stack bytes | 952 | 1,016 | 952 |
| Separate `emit` / `emit_values` code bytes | 260 / 1,694 | Inlined | 260 / 1,694 |
| Group-close code bytes | 927 | 927 | 1,008 |
| Group-close fixed local stack bytes | 8 | 8 | 56 |
| Points-loop instructions per slot | 31 | 31 | 36 |
| Points-loop stack-memory instructions per slot | 0 | 0 | 7 |
| No-points-loop instructions per slot | 17 | 17 | 19 |

Inlining duplicates emission at traversal call sites: total code for the three
functions grows from 5,292 to 7,448 bytes. The selected build reduces default
packing time by 8.9–10.2% in the cloud comparison.

The metadata trial copies `builder->count`, `builder->base`, and `builder->max`
before group closing's slot loop. This removes the count read and seven
base/extrema reads from the points loop, but introduces seven stack-memory
instructions and increases its instruction count. The no-points loop also
grows despite needing no stack-memory instructions. The combined cloud trials
show no consistent additional benefit after inlining, so metadata capture is
dropped. Its independent improvement does not carry over to the selected build.

Counts include the normal loop back edge and exclude setup and error paths.
Fixed stack figures exclude saved registers and return addresses. Other
packing helpers retain their previous sizes in these trials.

The [C experiment report](build/c-packing-candidates/report.html) records
independent and combined timings, including a comparison with the latest Rust
implementation. [Baseline assembly](build/c-packing-candidates/baseline/assembly.txt),
[selected assembly](build/c-packing-candidates/selected/assembly.txt),
[metadata trial](build/c-packing-candidates/metadata/assembly.txt), and
[loop counts](build/c-packing-candidates/loop-counts.json) retain the evidence.

## Rust follow-up after C emission inlining

The baseline is `8162ad00e`: its Rust packing executable is byte-identical to
the previous selected `d96b0793d` build; C includes `1e9c80d16` emission inlining.
Four opportunities remain visible in this assembly:

| Area | Rust baseline | Trial | C |
| --- | --- | --- | --- |
| Child masks with at most 64 supertypes | Calls an 810-byte helper with six saved registers and 72 local stack bytes | Inline the zero/one-word paths; outline allocation for larger masks | Already part of frame initialization |
| One-word dictionary lookup | Generic hash loop and `bcmp` | Specialize the comparison to `cmp`, optionally inline the lookup | Generic hash loop and `memcmp` |
| Per-node symbol/field/grammar stores | Two slab-address loads, or three with a separate grammar column | Capture one address through the existing borrowed writer | Retained column cursors |
| Group-close points loop | 40 instructions and ten stack-memory instructions per slot | Encode point columns in a separate pass | 31 instructions and no stack-memory instructions |

The child-mask wrapper becomes `inline(always)` while `child_mask_words`
remains outlined. Ordinary frame initialization avoids the allocation helper's
prologue, return-value staging, and error check. Its own fixed stack allocation
is unchanged. Larger masks retain the existing allocation and inheritance logic.

Dictionary specialization exposes a constant one-element slice to the existing
hash/probing code. The outlined variant removes `bcmp` for that case; the inline
variant additionally removes the lookup call. Neither is selected: inspection
of the exact benchmark grammar libraries finds zero to seven supertypes in
every language. Those builds never execute dictionary lookup. Their timing
differences can reflect code placement but cannot demonstrate a lookup speedup.
This candidate needs a corpus with 9–64 supertypes.

The per-node writer trial captures column offsets and the separate-grammar flag
before writing IDs. Its borrow excludes slab relocation while the addresses are
used. Unlike C's retained cursors, it still computes the destination from the
slot and column offset for every node.

| Rust function | Baseline code / local stack bytes | Child-mask trial | Writer trial | Separate-points trial |
| --- | ---: | ---: | ---: | ---: |
| Traversal | 5,239 / 312 | 5,239 / 312 | 5,231 / 312 | 5,239 / 312 |
| Frame initialization | 1,491 / 120 | 1,648 / 120 | 1,491 / 120 | 1,491 / 120 |
| Group close | 1,061 / 104 | 1,061 / 104 | 1,061 / 104 | 1,383 / 104 |

The separate-points trial emits 17 instructions per slot for non-point columns,
then either 20 scalar instructions per point pair or 52 vector instructions per
four pairs. Those loop bodies have no stack-memory accesses. The extra pass,
vectorization guards, setup, and scalar tail still cost time; the smaller loop
bodies alone cannot establish a speedup. Counts include normal back edges and
exclude setup and error paths. Fixed stack allocations exclude saved registers
and return addresses.

The cloud comparison retains child-mask inlining (`dd20ec429`), ID writer
caching (`c622d51eb`), and separate point encoding (`e07cb3011`). Together they
reduce default packing time by 4.6–5.2%, bringing Rust within 0.6% of optimized
C. Removing either writer caching or the point pass slows every measured
profile in both process orders. Writer caching adds little after child-mask
inlining alone; its benefit is clearer after splitting point encoding.

[Original Rust assembly](build/rust-asm-followup/baseline/assembly.txt),
[selected Rust assembly](build/rust-asm-followup/selected/assembly.txt),
[C assembly](build/rust-asm-followup/c/assembly.txt),
[C dictionary assembly](build/rust-asm-followup/c/dictionary.asm.txt),
[loop counts](build/rust-asm-followup/loop-counts.json), and
[grammar facts](build/rust-asm-followup/grammar-facts.json) retain the evidence.
The [cloud report](build/rust-asm-followup/report.html) records selection,
individual and combined trials, and final comparisons.

Two further candidates remain unmeasured: retain ID-column cursors for the open
group, as C does, and change point staging to reduce the gathers in the vector
loop. Retained cursors must be refreshed after slab growth; point staging must
preserve group-fit extrema and exact output bytes.
