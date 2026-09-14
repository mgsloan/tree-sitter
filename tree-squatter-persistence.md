# tree-squatter-persistence MVP design

Status: revised 2026-09-13. Storage decision: LMDB for all persistent metadata,
source contents, and packed trees. SQLite, external slab files, per-source cache
directories, generation marker files, and per-entry rename publication are removed.
Fingerprint policy remains open as noted below. Source input is raw disk bytes;
Zed participates only when loading does not change those bytes.

## Scope and guarantees

This synchronous Rust library captures a regular source file and returns its
matching Squatter tree. The caller supplies project root, relative path, and
grammar. Cache lookup and publication use one project-local LMDB environment.
Concurrent callers cooperate to avoid duplicate parsing where practical.

The current source mode is exact disk bytes: no decoding, BOM removal, newline
normalization, editor edits, stdin, injected-language trees, query results, or
incremental parse state. There is no normalized source mode in this crate.

Persist source bytes inside LMDB alongside trees, sharing one captured generation
among grammar variants for that path. The simple loader still reads/hashes disk:
stored contents and mtime do not prove the file is unchanged. Returned immutable
source is the exact input used to parse or confirm the tree. A concurrent rewrite
can produce a mixed capture; the API promises correspondence to the captured
bytes, not an atomic snapshot of concurrent source-file writes.

Only complete parses may be packed/published. Cache loading checks full identity
and memory safety, without an integrity checksum over source records, envelopes,
or slabs. BLAKE3 remains for source/implementation identities. Safety-valid semantic
corruption can go undetected; identity matching does not independently prove a
stored tree was produced by parsing the named source. No universal corruption
detection guarantee is implied.

Cache reads, cooperation, publication, and cleanup are optional. Writes and cleanup
can be deferred, cancelled, or omitted. A cache failure never invalidates an already
obtained pair. There is no async runtime, background thread, daemon, global object
store, external source snapshot file, or external slab file.

Use LMDB's normal durable transactions for process-crash and power-loss resilience.
Recovery must expose a complete old or new transaction, never half-published
source/tree records, assuming supported storage honors synchronization guarantees.

## Environment and filesystem boundary

Use one environment at:

```text
PROJECT/.tree-squatter/
  data.mdb          # all source, tree, and metadata records
  lock.mdb          # LMDB-managed coordination
  cooperation.lock  # application work/writer-admission locks; no cached contents
```

The application sidecar is only for OS-released coordination; LMDB owns all stored
content and metadata. Never manipulate LMDB's lock table/file directly. Do not
truncate, unlink, replace, compact in place, or reopen an environment behind active
handles. Ordinary maintenance changes records through transactions.

Share one environment handle per underlying environment per process through a
process-wide registry, including aliases and independently opened Persistence
instances. Select/audit a Rust binding that supports this ownership model; do not
open independent LMDB environments for the same files. Retain the environment as
long as any read owner exists. Configure named databases and reader-slot capacity
before opening, and coordinate first-time creation/schema initialization.

The implementation uses heed 0.22.1 with serialization features disabled, raw-byte
database codecs, and owning no-TLS read transactions. Its path canonicalization
means retained Linux directory handles do not anchor LMDB's own file opening;
the trusted/stable-directory limitation below remains essential. Keep the
application inode registry and separate work/writer-admission locks.

Require supported local filesystems. Do not use network filesystems or MDB_NOLOCK.
A cache open/permission/schema failure disables caching for the request; do not
automatically delete or overwrite an unknown/corrupt environment.

Accept nonempty project-relative source paths and reject parent traversal,
absolute/platform-prefixed paths, and the cache namespace itself. Normalize dot
components consistently; preserve non-UTF-8 Unix names and lossless Windows path
units. Record the platform path encoding. Case aliases may have separate entries
in the MVP; they must not cause identity confusion. Require regular source handles.
Permitted source symlinks can be read, but only cache targets within the root.

There is no longer a filesystem namespace collision between source names such as
.source and metadata: source paths are database values. Cache-root substitution
still matters. Retain/validate directory handles and use handle-relative no-follow
operations for application files. Audit the binding's pathname-based LMDB open:
checking a path and then passing it to LMDB does not itself provide race-free
resolution of data.mdb/lock.mdb. Use a proven platform opening strategy or disable
caching where its assumptions cannot be met. The supported environment assumes
cooperating processes, not an adversary able to rewrite mapped database files or
move open directories arbitrarily. Never claim directory checks create a sandbox.

## Language selection and identity

Language selection remains caller policy. A `Grammar` couples
`tree_sitter::Language` with a 32-byte `GrammarFingerprint`; truthful pairing is
a provider contract. An opaque language cannot supply an exact fingerprint.

Pointer equality, name, ABI, node-kind lists, crate version, and semantic version
are insufficient. The proposed grammar fingerprint is BLAKE3 over a domain-
separated, length-delimited canonical manifest of generated parser.c, scanner
sources and included dependencies, and behavior-affecting build/generation inputs.
Freeze canonical names/order and encoding with fixtures. Generate a constant at
build/release time; never hash grammar artifacts per source file.

Bundled grammars may use consumer-generated manifests. A dynamic grammar may use a
packaged manifest or a digest of the loaded artifact, computed once and bound to
its handle. Account for behavior supplied by dynamically linked dependencies; a
library-file digest alone need not cover those. Scanner behavior is assumed
deterministic for the declared inputs.

### Runtime and representation fingerprints: options

Runtime changes can affect parsing without changing grammar tables or slab ABI.
Identity must cover parsing behavior as well as representation compatibility.

1. Conservative artifact/build fingerprints for grammar, runtime, and Squatter:
   straightforward invalidation, but unrelated changes can destroy reuse.
2. Separate grammar fingerprint, parse-runtime compatibility epoch, and
   representation epoch/configuration: better reuse, but maintainers must bump
   epochs correctly for relevant fixes.
3. Hybrid: explicit compatibility epochs for releases, conservative source/build
   fingerprints for development builds without a declared epoch.

Recommendation for discussion: the hybrid, keeping grammar, parse-runtime, and
representation identities separate. Runtime ABI compatibility alone does not
establish identical parsing.

Squatter must export identity from its actual compiled C layout: slab version,
endianness, group size, alignment, points mode, and other interpretation switches.
Cargo feature names alone are insufficient. Output-preserving optimization kernels
need not change a compatibility epoch.

Persisted packing differences enter variant identity. Allocation hints such as
initial_group_capacity need not do so if mandatory compaction erases their effect.
Symbol-presence data and other persisted differences do. Specify this split
against the existing PackOptions before freezing the encoding.

Whole-file raw-byte parsing is the only mode. Canonical parse-option bits are zero;
included ranges and source transformations require a later design.

### Variant database key

Proposed canonical derivation, pending the identity choices above:

```text
variant_digest = BLAKE3(
  "tree-squatter cache variant v1" ||
  GrammarFingerprint || ParseRuntimeFingerprint ||
  RepresentationFingerprint || canonical persisted packing options ||
  canonical parse options
)
VariantId = full_256_bits(variant_digest)
```

Widths, byte order, and length delimiters are explicit. Store full identities and
variant digest in the envelope. Use the full digest in database keys; no filename token or Base32 encoding is
needed. Source identity is separate and participates in entry keys below.

## Named databases and record schema

Use a small fixed set of named databases, opened once. Encode keys and records
explicitly with versions, fixed-width integers, lengths, and canonical byte order.
Do not serialize Rust struct memory or depend on LMDB integer comparators'
native-endian layout. Query the actual LMDB maximum key size during initialization.

Proposed logical schema, to freeze with fixtures:

| Database | Key | Value |
| --- | --- | --- |
| meta | fixed schema/configuration keys | schema version and maintenance progress |
| paths | PathId | lossless canonical relative path and advisory current disk generation/stamp |
| sources | PathId + DiskGeneration | captured raw bytes and their length/digest |
| trees | PathId + DiskGeneration + VariantId | envelope plus complete compact Squatter slab |
| generations | PathId + DiskGeneration | advisory creation/use metadata and resumable retirement state |

PathId is a domain-separated 256-bit hash of platform tag plus canonical relative
path encoding. Long paths belong in values, not LMDB's size-limited keys. Compare
the stored full path before reuse or mutation. If a hash resolves to a different
path, bypass persistence for that request; never overwrite the other path record.
DiskGeneration contains full captured length and BLAKE3-256 digest. All tree
envelopes repeat the full requested identities for validation.

Trees reference the exact captured bytes in sources. Source records are scoped
to a path/generation, not globally deduplicated; this avoids reference counting
across unrelated paths. No second normalized source representation is stored.

Creating the source records, tree, generation metadata, and updating the
advisory current pointer is one write transaction. A generation can contain many
grammar/representation variants. No stale deletion is necessary to add one.
Keep all authoritative references self-consistent within each committed snapshot.
A late writer may update the advisory pointer to an older captured generation;
verified lookup uses the actual captured digest, so this affects usefulness only.

## Public API and ownership choices

Names and exact signatures remain provisional:

```rust
pub struct Persistence { /* shared environment + options */ }
pub struct LoadedFile { /* exact immutable source + tree owner */ }
pub enum LoadedTree {
    Database(DatabaseTree),
    Owned(tree_sitter_squatter::Tree),
}
pub struct PendingWrite { /* owned/shared source, complete slab, identities */ }
pub struct PendingLoad { /* opaque request and retry state */ }
pub struct Maintenance { /* logical continuation key; no parked transaction */ }
pub enum LoadStep {
    Ready { file: LoadedFile, write: Option<PendingWrite> },
    Deferred(PendingLoad),
}
pub enum ReadStorage { Owned, TransactionBacked }
```

LoadedFile exposes source() and tree(). The simple load wrapper defaults to owned
hits with short read transactions; TransactionBacked is an explicit option for
callers accepting snapshot retention. Misaligned slabs fall back to owned storage
even when transaction-backed reads are requested. This is one LMDB backend with
two in-memory ownership policies, not a fallback external-file backend.

Writing policy is disabled, inline, or deferred. A step-based API lets executors
schedule retries without blocking their workers; settle the wrapper's exact shape
for returning PendingWrite. PendingWrite owns/shares source and packed bytes and
never retains a parser borrow or LMDB write transaction. Dropping it performs no
unbounded I/O. Return the freshly packed owned tree on a miss, without reopening
the just-published record.

Publication status stays out of LoadedFile. An explicitly executed write task
may report completion/deferred/cancelled/error to its scheduler; observing that is
optional. Initial hit/miss metrics may remain available. LoadError means failure
to obtain the pair, including invalid path/grammar, source I/O, cancellation, or
parse/packing failure. Never expose publication of arbitrary caller-supplied slabs.

## Parser completion and cancellation

The current bindings use per-call ParseOptions::progress_callback. Returning
ControlFlow::Break(()) requests cancellation, for example when a caller observes a
cancellation token, deadline, superseded request, or shutdown. Passing a reusable
parser alone does not install that callback. Parser::parse passes no progress
callback; there is no implicit timer to inherit in this API.

Before every fresh parse, call Parser::reset(), set the requested language, and
set included ranges to the whole file. Document these mutations. Pass no old tree.
Tree-sitter otherwise retains resumable state after cancellation; a new load must
not resume another document's work.

Accept per-load cancellation/progress control separately from the reusable parser.
An interrupted parse yields no completed tree: reset state, release cooperation
ownership, and create neither a packed result nor a pending write. Never persist
a partial parse. A completed error-recovering tree containing ERROR/missing nodes
is cacheable; syntax errors are not cancellation.

An input callback must not return premature EOF to signal cancellation: that could
look like successfully parsing shorter input. Validate chunk-provider progress.
Check cancellation between bounded source reads, waits, writes, and cleanup steps.
Existing noninterruptible packing cannot promise immediate cancellation. Once a
complete pair exists, cancellation of cache work need not discard it. Cancellation
after commit cannot undo publication.

## Safety-only validation and ownership

Do not add an envelope/slab integrity checksum. BLAKE3 remains for source and
implementation identity, not verification of serialized-tree integrity.

Add a safety-only loader to Squatter. Before unchecked native operations, establish
all their safety prerequisites: arithmetic overflow checks, allocation/slice bounds,
column extents/alignment, bit widths/shifts, node/group/dictionary indexes,
language-table indexes, and source ranges used for slicing. Include traversal
invariants necessary to prevent invalid accesses or malformed unbounded walks.
A slab-within-file bounds check alone is insufficient.

Do not reparse or prove semantic equivalence. Do not reconstruct indexes merely
to verify their semantic accuracy. Audit current checked loading to distinguish
safety prerequisites from semantic checks; do not blindly remove node validation.
Fuzz loading followed by tree/cursor/traversal/query operations on malformed slabs.

Safety-only does not imply constant-time opening. Eager validation may still scan
nodes/columns. Lazy opening needs checked-on-access operations or lazily validated
columns with safe failure propagation before exposing unchecked accessors. Measure
pages touched and opening cost honestly.

### LMDB value alignment and transaction ownership

LMDB owns the mapping; do not mmap data.mdb independently or interpret database
page internals. Fetch source and tree records from the same read transaction.
A borrowed value is valid only while that transaction and environment remain
alive. No transaction reset, renewal, abort, environment resize, or close may
invalidate an exposed slice.

For owned hits, copy the slab into properly aligned owned memory, retain the
captured source, and end the transaction promptly. Use Squatter's safety-only
loader on that storage. Stored source bytes are still available to tentative or
snapshot-oriented APIs; they never replace disk validation in the simple loader.

For transaction-backed hits, an opaque read owner retains the read-only transaction
and environment for the full tree/source lifetime. Squatter's owning-byte API must
destroy its descriptor before releasing that owner. Nodes/cursors borrow the tree
normally. No fabricated static lifetime or exposed raw transaction is permitted.
If current disk bytes have already been captured, they can remain the returned
source; cached source views are explicitly tied to the same transaction owner.

A value's slab offset divisible by 64 does not prove its actual pointer is aligned.
Validate the address and extent on every open. Do not assume LMDB values satisfy
8- or 64-byte slab alignment, including after page moves or compact copies.
The initial correct path copies misaligned values. Any later zero-copy encoding
using reserved values/padding must prove alignment across reopen/relocation and
initialize all padding; it cannot rely on accidental allocator behavior.

Use MDB_NOTLS if read owners can move between threads. Audit the binding's Send,
Sync, transaction-use, and drop rules; the flag is not blanket permission for
concurrent LMDB API calls on one transaction. Synchronize transaction operations
and final destruction. Immutable tree access must have a separately justified
safety contract. Never reset a transaction while any tree/source borrow exists.

### Long-lived snapshots and capacity

A borrowed tree pins an LMDB snapshot, which may delay reuse of pages retired by
unrelated writes across the environment. This is an accepted opt-in tradeoff, not
per-entry file retention. Default owned hits avoid that cost. An explicit detach
operation can produce an owned copy; it cannot revoke existing aliases or free
their snapshot until the final owner drops.

Bound configured map capacity, reader slots, and newly granted borrowed owners.
Track oldest local read age and map usage for optional diagnostics. On pressure,
prefer owned reads for new requests and skip/defer writes that cannot fit.
Never invalidate live readers to reclaim space. Run mdb_reader_check during
bounded maintenance to clear crashed-reader slots; it cannot clear live readers
merely because they are old.

Start with an agreed configurable map ceiling suitable for the platform. Do not
resize behind active local transactions or exported references. On MDB_MAP_FULL,
abort the write and defer/skip; retain the valid pair. A controlled later growth
operation requires local quiescence and cross-process resize coordination.
On MDB_MAP_RESIZED, adopt the size only after local owners have drained; otherwise
bypass caching. Fixed-capacity operation is an acceptable first implementation.
Avoid giant virtual reservations on unsupported address-space/platform targets.

## Cooperation and writer admission

A parse miss first checks for work already in progress for PathId, generation,
and variant. OS-released work ownership is separate from LMDB
transactions: never hold the sole write transaction while parsing.

The Linux prototype uses 256 one-byte OFD work locks in cooperation.lock at offsets
1 through 256, selected by the first BLAKE3 byte of the full tree key. Writer
admission uses a separate whole-file flock, independent of OFD locks on supported
local Linux filesystems. Hash collisions cause only extra bounded waiting. Use
nonblocking OS lock operations and an in-process ownership registry. Other
platforms currently bypass parse-work coordination; their interoperable lock
protocol and close semantics still need auditing. Keep the sidecar handle
stable and never unlink/recreate it during operation. This avoids unbounded files
or stale persistent claim rows. Never lock byte ranges inside LMDB-managed files.

State machine:

1. Capture/hash source and try a short read transaction for an exact entry.
2. On miss, try work ownership nonblocking and recheck with a fresh read transaction.
3. The owner parses. Contenders return Deferred or perform bounded cancellable
   retry/backoff, opening a fresh snapshot on each cache recheck.
4. Owner death/cancellation releases the OS lock. Live-stalled owners are handled
   by wait budgets; callers can continue deferring or parse independently.
5. Inline writing retains work ownership until publication finishes. Deferred
   writing releases it when returning the pending item, accepting possible
   duplicate parses until publication. Every writer rechecks before inserting.

No PID or lock-file existence proves ownership. No lease/heartbeat/fencing protocol
is necessary for this advisory optimization. A late independent writer still
publishes only its exact captured identity.

Every application write transaction, including initialization and maintenance,
first obtains the nonblocking writer-admission lock. This avoids entering LMDB's
blocking single-writer acquisition behind another cooperating process. LMDB retains
its own native locking and atomicity. Never acquire work ownership while holding
writer admission. Foreign clients ignoring this protocol can still block native
LMDB acquisition: support only participating writers, and document that cancellation
cannot interrupt arbitrary blocked native calls. Unsupported coordination disables
publication/cooperation, not source parsing.

## Envelope and source encoding

Each tree value has magic/schema version, header lengths, disk source
identity, grammar/runtime/representation identities, canonical
persisted options, full variant digest, slab offset/length, and required alignment.
Optional diagnostics do not establish matching. There is no integrity checksum,
source marker file, abbreviated filename token, or per-entry metadata sidecar.

Source values have explicit versions and byte lengths followed by contents.
Check all conversions, offsets, extents, flags, source bounds, and total lengths.
Reject ambiguous trailing bytes and unsupported required versions/options.
Do not trust lengths from metadata when constructing slices. Always use actual
MDB_val lengths; then perform Squatter safety validation.

## Load and deferred atomic publication

The verified path:

1. Open/read the regular source into immutable storage while hashing. Sample the
   same handle before/after; retain advisory length/mtime only if stable.
2. Derive full request keys. Open a read transaction; validate path, source
   relationships, full tree envelope identities, and memory safety.
3. Return an owned or transaction-backed hit according to policy, or close the
   miss snapshot and enter cooperation. Never wait for another writer's result
   while retaining an old read snapshot.
4. Reset/configure the parser, parse completely, and pack compactly. Return the
   exact source and tree; disabled/deferred writes do no contents publication.
5. Inline/deferred PendingWrite executes the transaction below. Failure is optional
   cache work failure; the returned pair remains usable.

Publication:

1. Finish hashing/packing/envelope preparation outside any write transaction.
   Pending work must not pin an unrelated read snapshot while queued.
2. Obtain writer admission; begin a write transaction and recheck full identities.
   If a valid matching tree exists, discard the duplicate candidate.
3. Insert/reuse source bytes, insert the complete
   slab, and update generation/path records atomically. All these are LMDB values.
   A source record removed by earlier cleanup is reinserted in this transaction.
4. Check cancellation before commit; abort the whole transaction if requested or
   if any put fails. Limit each normal publication to one complete source/tree
   request. Enforce size budgets; do not split a visible tree across transactions.
5. Commit using normal synchronous durability, then release writer/work ownership.
   Once commit begins it is a noninterruptible native operation; cancellation
   cannot undo a completed commit.

MDB_RESERVE may reduce intermediate buffers, but reserved memory must be completely
initialized before the next update/commit and cannot escape the write transaction.
It is not a place to perform a long parse. Bound bytes written per task and measure
the unavoidable single-record copy/commit latency honestly.

## Crash and power-loss resilience

Use normal synchronous LMDB commits with a read-only mapping: do not enable
MDB_NOSYNC, MDB_NOMETASYNC, MDB_MAPASYNC, MDB_WRITEMAP, or MDB_NOLOCK. Audit the
selected LMDB build's platform synchronization behavior, including macOS storage
flushing, rather than assuming all compile-time configurations provide the same
guarantee. Environment bootstrap must also persist necessary directory entries.

An interrupted uncommitted transaction cannot publish only the source or only the
tree. After recovery readers see a complete committed snapshot. A commit error
may leave its durability outcome uncertain; treat it as optional cache failure,
and validate normally next time. No external rename/marker ordering is required.

LMDB manages free/retired pages, metadata pages, and its lock file. Never repair
a failed cache by deleting lock.mdb or replacing data.mdb under readers. Unknown
schema or detected database errors disable cache use pending separate maintenance.
Safety validation applies to retrieved slabs; it does not turn LMDB into a safe
parser for arbitrary maliciously rewritten database files.

Process-kill tests are insufficient for power-loss claims: test filesystem/VM
failure at commit boundaries with volatile caches discarded. The guarantee assumes
working storage barriers. No checksum is added to detect arbitrary safety-valid
media corruption.

## Optional, bounded maintenance

Cleanup is a separate resumable operation using short read transactions to discover
candidates and bounded write transactions to revalidate/delete them. Persist or
return logical continuation keys, never a cursor/transaction across executor waits.
The writer-admission gate applies. Cancellation aborts the current batch without
undoing previously committed batches. No publication depends on a complete sweep.

Maintenance can retire obsolete generations, abandoned variants by explicit
age/space policy, and paths whose source no longer exists. Filesystem checks occur
outside write transactions; errors such as permission denial are not deletion.
Recheck database identities before deleting. Source recreation can still race;
at worst a disposable entry is lost and subsequently rebuilt. Never modify source.

Delete tree records in bounded prefix batches; remove source/generation
records only once their referencing trees are gone in the current write snapshot.
A concurrent late publisher atomically restores all records it needs. Check any
advisory current pointer before removing its target and clear/update it atomically.
Metadata deletion is separate from physical page reuse: live readers can retain
old pages even after records disappear from current snapshots.

Free pages are reused by LMDB; record deletion does not promise the database file
shrinks. Automatic online compaction or replacement of the environment is out of
scope. Disk quotas, map ceilings, and optional later offline compact-copy
maintenance must respect live readers. Run crashed-reader checks without treating
live-stalled transactions as dead.

Deleted/renamed paths never loaded again are discovered by optional whole-cache
sweeps. Never running maintenance consumes space, not correctness. Avoid writes
on every cache hit just to record recency; sample/defer accounting if needed.

### Temporary files

Normal capture, publication, and record cleanup create no application temp files:
source contents and slabs are prepared in memory and written through LMDB
transactions. Aborted transactions leave LMDB-managed pages, not orphan slab files.
data.mdb, lock.mdb, and cooperation.lock are persistent environment files and must
never be swept as temps.

Any future spill/export/compact-copy operation outside an OS-managed temporary
facility must include orphan cleanup from its first implementation. Use exclusively
created, never-reused session directories, with an OS-held owner lock registered
under a permanent short registry lock. Create and acquire owner locks while holding
that registry lock; cleanup opens/tries them nonblocking under the same lock and
skips live owners. Failed attempts close handles immediately. Reclaim only recognized
regular files with handle-relative no-follow operations, in cancellable batches.
Retire an empty session and its owner-lock inode under the registry lock so no
participant can adopt a deleted lock. Missing-owner sessions are interrupted
registration and are checked under the same registry lock. Do not use PID or age
alone to decide abandonment.

Include bounded startup/scheduled orphan sweeps when that optional facility exists,
even if original source paths are never revisited. Maintenance can be deferred or
disabled, but later execution must reclaim crash leftovers. /tmp placement alone
does not prove automatic cleanup. There is no need to implement a temp-session
registry in the initial no-temp LMDB workflow.

## Tentative and chunked API

Retain TentativeFile, opaque ValidationTicket, SourceSnapshot, verify, Rebuild, and
validate phases. Apply the same cooperation/deferred-write policies to rebuilds.

Tentative lookup uses paths' advisory generation/stamp and fetches contents plus
the requested tree in one read snapshot. A matching stamp authorizes tentative
access only; source bytes loaded from LMDB are also tentative relative to disk.
The simple loader never treats cached contents as proof of the current disk file.
Apply ownership/alignment policy and safety checks before exposing tentative data.

Full comparison with a freshly captured disk source confirms or rejects the
ticket. Source-read/hash failure leaves it unverified; rejection occurs before
rebuild and cannot be reversed by a later rebuild failure. Tickets bind environment,
path, grammar/configuration, and the immutable tree snapshot. A later
database deletion/replacement does not invalidate a live transaction-backed ticket.

SourceChunks returns consistent immutable bytes at arbitrary requested offsets,
with empty output only at/beyond EOF. Validate hash-walk progress, lengths, and
native offset limits. Constructors normally compute the hash; streaming hashing
can avoid a second pass. Precomputed identities are caller correctness contracts,
never authority for unchecked memory access.

Provisional UI may use tentative results. Edits/refactors and persisted diagnostics
wait for confirmation. Applications manage superseded request/buffer versions.

### Zed compatibility: unchanged-load-only participation

The persistence crate stores raw disk bytes and trees parsed from those bytes.
Zed reuses or publishes an entry only when its parser-visible input is exactly the
captured disk bytes, and grammar/representation identities also match. Track actual
byte transformations during Zed's existing load: encoding conversion, BOM removal,
and CRLF/lone-CR normalization make the buffer ineligible. Transformation detection
does not replace the full source identity comparison or application buffer-version
checks. A loader taking a normalization path that changes no bytes remains eligible.

BOM-free UTF-8/LF files can participate without transformation. Zed retains
line-ending/encoding preferences and can write CRLF again on save; saving does not
guarantee a future load is byte-preserving. For transformed inputs, Zed parses its
buffer normally and neither confirms a raw cache tree for that buffer nor publishes
the buffer tree under the raw-source key. No coordinate translation or normalized
variants are implemented in persistence. A cache tree shown tentatively before
loading must be discarded for buffer use if a transformation is then detected.

See [Zed's load/save code](https://github.com/zed-industries/zed/blob/main/crates/worktree/src/worktree.rs).

## Implementation and tests

Implementation has begun in crates/persistence. The first owned-read milestone
implements atomic sources/trees/path records, deferred writes, parser reset and
cancellation, actual compiled representation identity, and process/thread writer
admission. Cache loading now uses a structural safety entry point that skips
auxiliary-index semantic reconstruction and unused auxiliary padding checks while
retaining conservative node/topology/coordinate checks. See
[the scoped validator audit](crates/persistence/validation.md); broader fuzzing
remains pending. The crate also implements Linux parse-work cooperation with
bounded waits/resumable deferral, optional bounded obsolete-generation and
deleted-path cleanup, and explicit stale-reader checks. The prototype stores the
advisory generation pointer in a separate `current` named database; schema version
2 is intentionally incompatible with prototype 1 and does not migrate it.
Opt-in transaction-backed slab hits now retain a sealed MDB_NOTLS read owner with
actual-pointer alignment checks and owned fallback. Their source stays an owned
disk capture. A fixed limit of 32 local owners bounds admission; explicit detach
copies a slab without revoking existing aliases. Configurable admission, reader
age diagnostics, cached source views, capacity/age eviction, other-platform work locks, and
power-loss qualification remain pending. See the crate README for scope.


Use one crates/persistence workspace crate with modules for identity, schema/codec,
environment ownership, source capture, transaction-backed trees, load/cooperation,
publication, maintenance, and platform integration. Depend on the exact workspace
Tree-sitter/Squatter, BLAKE3, an audited LMDB binding, and minimal platform locking.
Do not add SQLite or an independent slab-mmap/file backend. Independent packaging
also requires Squatter's C sources/headers and publish=false to be addressed.

Sequence:

1. Freeze named DB schema/key encodings and cooperation ranges; choose/audit LMDB
   binding, per-process environment registry, durability, capacity, and reader policy.
   Fingerprint details remain explicitly open.
2. Add Squatter safety-only loading and owning-byte API; implement owned LMDB hits
   first, then transaction-backed hits with verified alignment and lifetime rules.
3. Implement exact-byte capture, parser reset/cancellation, and atomic LMDB
   source/tree publication with disabled/inline/deferred policies.
4. Add bounded cooperation, maintenance, capacity/reader-pressure handling, and
   deterministic multiprocess lifecycle tests.
5. Add tentative/chunked integration; validate power-loss behavior on supported
   platforms and benchmark before default enablement.

Required tests:

- Fixed schema/key/envelope/identity fixtures; malformed lengths/indexes/flags and
  downstream tree/cursor/query fuzzing under safety-only validation.
- Native versus fresh-packed versus owned/transaction-backed LMDB hit semantics.
- Source contents stored once per path/generation across variants; no partial
  source/tree publication; full path collision checks and long/non-UTF-8 path values.
- Cancellation then parser reuse on another document; no partial parse publication,
  but complete syntax-error trees remain cacheable.
- Misaligned values copy safely; aligned borrowed trees retain transaction/environment;
  final descriptor/source drop order; no invalidation by resize/reset/close.
- Old reader touches cold pages after updates/deletions of its records; unrelated
  write churn demonstrates snapshot-retention cost and map-pressure fallback.
- Multiple Persistence instances/aliases share one environment; reader-slot exhaustion,
  crashed-reader cleanup, live-stalled readers, MAP_FULL/MAP_RESIZED, and no forced
  detachment of exposed references.
- Contender deferral, owner death, stripe collisions, bounded waiting, duplicate late
  publication, writer-admission ordering, and no write transaction during parsing.
- Disabled/deferred publication returns usable pairs; dropped queued work holds no
  write transaction and creates no application temp files.
- Process and power-loss injection before/during commit, including multi-record
  publication and maintenance; normal recovery never exposes a partial pair.
- Cleanup cancellation/resumption, source deletion/recreation, late writers restoring
  removed source records, pointer consistency, and physical retention under readers.
- Tentative cached contents remain unverified until disk capture; restored mtime/size
  does not authorize confirmed reuse; rejection before failed rebuild.
- Raw CRLF/BOM/invalid-UTF-8 bytes preserved; unchanged Zed loads eligible and
  transformed loads excluded from both cache reuse and publication.
- Normal operations never create slab/marker temps; any future disk-temp operation
  includes owner-registration/cleanup crash tests and never deletes LMDB files.

Measure capture/hash, LMDB begin/get/copy/commit, safety validation/pages touched,
cooperation waits, parse/pack, cleanup, source/slab storage, reader ages, map growth,
and retained pages under long-lived readers. Compare owned and transaction-backed
hits. No checksum pass, filesystem slab rename, or SQL checkpoint is involved.

LMDB implementation reference:
[official API and caveats](https://github.com/LMDB/lmdb/blob/mdb.master/libraries/liblmdb/lmdb.h).
