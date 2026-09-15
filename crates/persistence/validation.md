# Cache validation boundary

Persistence defaults to Squatter's copied `from_bytes_safety_checked` loader.
Opt-in transaction-backed hits use `from_owned_slab` with the same validation.
The existing `from_bytes` and `from_bytes_borrowed` retain their stricter behavior. Both policies require
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

## Transaction-backed ownership

`snapshot.rs` uses heed's `env.clone().static_read_txn()`, which returns an owning
`RoTxn<'static, WithoutTls>` retaining the environment. The static lifetime is
provided by heed's ownership API, not fabricated from a borrowed transaction.
Admission retains an `Arc<Store>`; construction uses
exclusive transaction access to check path, contents, and envelope from one read
snapshot. The slab pointer and length are sealed in a `StableSlab` owner. There
are no subsequent get/cursor/reset/renew calls; heed's final drop aborts the transaction
before releasing the environment/admission permit.

Squatter's `BackedTree` drops the native descriptor before its storage owner.
`StableSlab` is an unsafe implementation contract: its slice must remain at the
same address and be immutable until drop, including across owner moves. The LMDB
implementation relies on normal copy-on-write operation, retained environment
ownership, and no environment resizing. The native loader verifies the actual
address alignment on every hit; misalignment releases the owner and falls back
to copied storage. No special LMDB page layout or reserved-value encoding is used.

The environment uses `read_txn_without_tls` (MDB_NOTLS). Heed makes this owning
transaction Send but not Sync. Only our sealed storage owner implements Sync,
never the transaction API: construction has exclusive access;
after sealing, concurrent readers only access immutable mapped bytes, not the
transaction API. Final abort requires exclusive owner destruction and may run
on a different thread. No transaction handle is exposed. Tests cover owner
release on validation/alignment failure, cross-thread final drop, concurrent
publication/cleanup, bounded admission, aliases after detach, and reclaiming a
crashed process's reader slot.

## Heed environment opening

Heed is pinned to 0.22.1 with serialization features disabled. Its named database
handles and schema initialization are committed in one writer-admitted transaction.
Normal locking and synchronous durability flags remain enabled. Our inode-based
process registry remains necessary for aliases and shared application lock state;
heed's canonical-path registry is not a replacement for that registry.

Heed canonicalizes the environment path before calling LMDB. In particular, it
resolves Linux `/proc/self/fd/<directory>` to a pathname. Retaining that directory
still helps application-side sidecar access, but does not make LMDB's own opening
handle-relative or race-free. The existing trusted, cooperating-process contract
requires stable cache paths during opening; hostile component/leaf substitution
and arbitrary directory moves are not supported.
