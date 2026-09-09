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
