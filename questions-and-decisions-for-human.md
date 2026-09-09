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
