# Questions and decisions for human review

## Scope

- Implement the non-query portions of `design.md`. Query compilation and execution are explicitly out of scope for this work.
- Keep the mainline runtime and its bindings unchanged. The C implementation lives in `lib/squat/`; a separate Rust crate builds and wraps it.
- The existing untracked `todo.md` belongs to the human and is left untouched.

## Decisions

- Start with the representation and a differential testable API, then build corpus and benchmark tooling on that API.
- Version 1 uses native endianness and the exact matching grammar. Cross-endian persistence and a durable grammar fingerprint remain open design questions. The slab header is explicitly padded to 32 bytes, with all padding initialized to zero.
- Dictionary overflow is a reported conversion error, rather than the crash provisionally described in the design. Allocation and 32-bit layout overflow are reported as errors too.
- Navigation may scan siblings or ancestors when the format has no direct index. Cursors maintain an ancestor stack for efficient repeated tree walks.

## Questions

- What grammar identity should a persistent envelope record: generated parser hash, grammar source revision, or both? Version 1 callers must provide the exact matching grammar.
- Should the supertype dictionary grow beyond 256 entries by widening its node column in a later representation version?
- Should packed trees preserve included ranges as a separate optional section? The current design preserves node coordinates, but has no included-range section.

## Validation and remaining work

Updated alongside implementation commits below.

### C runtime baseline

- Implemented aligned slab columns, checked 32-bit layout arithmetic, lane-aware growth/compaction, reverse raw-subtree traversal, navigation/cursors, symbol presence entries, supertype dictionaries, and validated deserialization.
- Reverse traversal stages each open parent's child positions. Subtracting a multiline child's point extent cannot recover its preceding column. This avoids repeated forward rescanning and does not stage a full array of visible node attributes.
- Supertype bit order is ascending raw grammar ID, including metadata on ABI-14 grammars whose public supertype enumeration is unavailable. A visible node receives its incoming mask, as specified in the design; its own bit applies to children.
- Mainline's next-sibling node accessor skips empty nodes ending at the original node's end byte; cursors and child enumeration retain them. The public API reproduces this distinction. A separate structural sibling helper supports complete iteration.
- Mainline field lookup stops at ERROR productions even though child field enumeration may expose fields from hidden children. Squat preserves this distinction.
- `child_with_descendant` requires a strict descendant in squat. Mainline can return surprising children when passed the parent itself; that out-of-contract case is not reproduced.
- The cached code-corpora build image works. Its grammar runtime image was absent and the registry pull failed authentication. Tests compile local grammar sources inside the existing offline build image instead.
- Initial JSON/Python differential checks and standalone lane relocation checks pass. Broader grammar/error-recovery and sanitizer coverage is in progress.

### Scope and compatibility updates from the human

- Query implementation is now authorized, after the non-query implementation is finished. Reference `../main` and reuse/adapt its optimized query code where useful. Do not start that phase before the preceding work is complete.
- The human already established that padded, non-straddling u64 SWAR wins for symbol equality. Retain it as the baseline; experiments should investigate useful remaining tradeoffs rather than rediscover the format choice.
- The human asked for readable control flow, meaningful names, and concise explanations of non-obvious invariants. Added local formatting rules and expanded the initial compact C implementation accordingly.
- The human explicitly requested programmatically ignoring seek differences for now, suspecting an upstream bug. They are counted and reported, but do not fail default comparison runs. `--strict-seeks` restores strict behavior. No hidden-node section or seek-barrier metadata was added.
- Minimal valid CSS repro is `a b {}` in `lib/squat/tests/fixtures/hidden-seek.css`: an empty hidden node can make mainline return the visible ancestor where the packed tree finds the child at the same position. This also occurs in malformed inputs in other grammars. Treating this as an upstream bug remains a hypothesis, not a confirmed diagnosis.

### Equality and layout experiments

- Added exact lane-wise SWAR equality, with a physical-slot group mask that removes leading waste. Tests compare every encoded column against scalar reads and exercise widths 2–32, including adjacent-lane carry/borrow cases.
- Added scalar, portable SWAR, hardware-popcount SWAR, compiler-targeted AVX2, explicit SSE2, and explicit AVX2 microbenchmarks. Hardware-popcount and compiler-targeted baselines prevent attributing compiler flag differences to SIMD alone.
- Experimental builds can use 32 or 64 slots per group; leading-waste width and magic flags change with the group size. The default public format remains 16 slots. These builds are for measurement, not an unversioned change to persisted data.
- Rust bindings retain the grammar, borrow nodes/cursors from the owning slab, and expose shared statically dispatched tree/node/cursor traits. A container integration executable verifies FFI layout, independence from the mainline tree, traversal, and serialization.
