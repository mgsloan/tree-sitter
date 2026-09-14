# Cache validation boundary

Persistence uses Squatter's copied `from_bytes_safety_checked` loader. The existing
`from_bytes` and borrowed loader retain their stricter behavior. Both paths require
the exact matching grammar; persistence separately checks full implementation,
representation, path, and captured-source identities. Neither loader reparses the
source or proves tree correctness. There is no serialized-tree integrity checksum.

## Checks retained

- Header/version/layout compatibility, exact section extents, bounded allocation,
  and native alignment of owned storage.
- Node span/topology and traversal termination prerequisites; symbol, grammar,
  field, and supertype dictionary indexes.
- Coordinate overflow and range ordering. These conservative structural invariants
  remain shared with the strict loader rather than weakening native traversal's
  assumptions. Persistence additionally checks each node's end byte against the
  captured source length before exposing the source/tree pair.

## Semantic checks omitted

`lib/squat/index.c::validate_presence` reconstructs per-symbol occurrences and
checks exact sparse-list order, bitmap membership/cardinality, mode selection,
sentinels, and padding. Cache loading skips this reconstruction. Section extents
are still checked before any access. Its consumer, `sq_tree_group_has_symbol`,
checks the group and symbol indexes; bitmap accesses use those bounded indexes.
Sparse entries are read in a fixed-length loop and compared, never dereferenced
as node slots. Query planning calls that accessor rather than reading the section
directly. Incorrect membership can therefore change query results, intentionally
without causing a cache-integrity rejection.

Unused high bits in supertype dictionary words are also ignored. Node dictionary
indexes remain checked; `sq_node_has_supertype` reads only words/bits selected by
the grammar's bounded supertype enumeration. The bits outside that enumeration do
not control addressing. Actual membership is not reconstructed in either loader.

This is a scoped separation of known semantic checks, not a claim that every
remaining rejection is a mathematically minimal safety prerequisite. Do not
remove additional topology/coordinate checks without auditing every downstream
node, iterator, cursor, seek, and query consumer.

## Verification and remaining work

Tests exercise invalid/truncated headers and sections, deterministic slab bit
mutations followed by traversal, and corrupted presence sections followed by
group lookups and query execution. Native comparison tests also distinguish strict
rejection from safety acceptance for individual auxiliary mutations. Broader
coverage-guided fuzzing, multi-grammar dictionary fixtures, and platform/layout
qualification remain required. This validation is not a defense against another
process maliciously modifying a live LMDB mapping; cache directories are trusted.

The JSON native comparison and query suites, plus packed-column unit tests, pass
under AddressSanitizer and UBSan on Linux for this change. That is a regression
check, not exhaustive verification of the native safety boundary.
