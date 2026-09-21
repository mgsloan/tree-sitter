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

These follow-ups are not included in the current assembly. Conversion remains
entirely in Rust. [rust-core-results.md](rust-core-results.md) records cloud
timings and validation separately; assembly alone does not establish a speedup
or explain the whole cold-parse time.
