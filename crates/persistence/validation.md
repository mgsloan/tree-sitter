# Cache validation boundary

Copied and retained forest loaders use the same checks. The
`from_bytes_safety_checked` entry point delegates to `from_bytes`; borrowed loading
uses the same validator. Persistence separately checks implementation,
representation, path, and captured-source identities. Loading does not reparse
source or prove that a slab belongs to the supplied grammar bindings.

## Core checks

Release loading checks header/version/layout compatibility, section sizes,
region extents, and grammar bindings. Retained storage must be eight-byte aligned.
Tree metadata is reconstructed backward from root spans, checking root accesses,
span arithmetic, and strict progress within each region. Root byte bounds also
establish the region's ordering classification.

Full descendant topology, symbols, fields, supertypes, coordinates, and boundary
alignment are checked in debug builds. Release readers rely on those content
invariants. Persistence additionally walks nodes to check their end bytes against
the captured source length before returning the pair.

## Separate side data

Presence loading validates the concatenated region records and bitmap extents.
Attachment checks region counts and grammar dimensions. `PresenceCache::validate_for`
also checks bitmap contents against the core. Absent records select ordinary symbol
scanning. Incorrect bitmap contents can change query results in any build profile.

Point loading validates its header and payload size. Attachment checks forest
dimensions and occupied-slot delta overflow. `PointsData::validate_for` also checks
content ordering and unused slots. Neither sidecar reconstructs core groups or
verifies coordinates against source text.
Failed attachment preserves the previous side data.

Both `validate_for` methods are explicit and identical in debug and release builds.
Packing and copies made by `detach` or `to_compacted` trust their matching sidecars
without validation.

## Verification and remaining work

Tests cover truncated headers and sections, deterministic slab mutations,
sidecar attachment, retained-owner lifetimes, and forest round trips.
Broader property tests, coverage-guided fuzzing, and platform/layout qualification
remain planned. Cache directories and their immutable slabs are trusted.

## Retained transaction ownership

`snapshot.rs` uses heed's `env.clone().static_read_txn()`, which returns an owning
`RoTxn<'static, WithoutTls>` retaining the environment. The static lifetime is
provided by heed's ownership API, not fabricated from a borrowed transaction.
Admission retains an `Arc<Store>`; construction uses
exclusive transaction access to check path, contents, and envelope from one read
snapshot. The slab pointer and length are sealed in a `StableSlab` owner. There
are no subsequent get/cursor/reset/renew calls; heed's final drop aborts the transaction
before releasing the environment/admission permit.

Squatter's `Forest` retains the slab owner and caches its byte address and length.
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
