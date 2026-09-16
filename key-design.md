# Persistence keys and validation

Status: design discussion; not implemented.

Keys select reusable cache slots. Stored identities determine whether an entry
is valid. Rebuilding a grammar should replace its slot, not accumulate entries
for every implementation fingerprint.

## Direction

- Use XXH64 for file contents. Store captured content length and observed file
  mtime; mtime is a hint, not proof that the contents are unchanged.
- Remove the runtime source/build fingerprint. Trust explicit Squatter version
  bumps for changes that invalidate cached results, including parsing changes.
- Share slots across writer programs. Different declared grammar versions should
  coexist; writer program names and versions need not enter the key.
- Keep exact grammar fingerprints for validation, outside the key. A mismatch
  causes a miss; publication replaces that slot.

## Proposed identities

| Component | Key | Stored validation |
|---|---|---|
| Grammar | Name + declared version | Exact implementation fingerprint |
| Squatter | Compatibility version + layout | Matching slab header |
| Packing | Persisted options | Matching options |
| Source | Project-relative path; content identity undecided | Length + XXH64; observed mtime |

For example, `rust@0.23` and `rust@0.24` occupy separate slots across programs.
Development builds declaring `rust@0.24` replace one another, with fingerprint
checks preventing reuse of an incompatible entry. Prepared grammar-table caches
should follow the same separation between slot identity and validation.

Tree-sitter's embedded grammar semantic version could provide a default where
available. Its ABI version is a runtime compatibility constraint, not a grammar
release identifier. Callers must be able to supply missing identity information.

Different implementations declaring the same name/version can still evict each
other. An optional stable qualifier, such as a fork or development workspace
name, could distinguish these without creating a slot per build.

## Representation identity

The existing `format_flags` header word combines `SQ_VERSION` with per-tree
flags. `SQ_VERSION` already includes the slab version, group size, and column
alignment. Exposing that word directly, with per-tree flags cleared, could replace
the hashed representation identity without changing the slab layout.

Persisted options such as point storage and symbol-presence indexing remain
separate key inputs. Allocation hints do not belong in identity. With no runtime
fingerprint, maintainers must bump the compatibility version whenever existing
cached results should no longer be accepted, even if the byte layout is unchanged.

## Open choices

- **Source generations:** a stable path slot bounds growth during editing but
  loses reuse when reverting contents. Including length/hash preserves that reuse
  and requires eviction of old generations. Keep mtime outside the key either way.
- **Grammar version policy:** choose defaults, fallback behavior for missing
  versions, and whether to support a stable qualifier.
- **Exact grammar fingerprint:** specify how providers cover generated parser
  tables, scanners, and behavior-affecting inputs. Structural slab validation
  alone cannot establish grammar identity.
- **Header/version policy:** confirm reuse of the existing format word versus
  separating result compatibility from byte-layout compatibility.

Coarse keys change replacement policy, not validation requirements. Deferred
publication must still preserve a matching source/tree pair, and existing readers
must remain valid when a slot is replaced.
