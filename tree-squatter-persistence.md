# tree-squatter-persistence MVP design

Status: implementation design, 2026-09-11.

## Scope

`tree-squatter-persistence` is a Rust library for loading a source file and its
Squatter tree together. Given a project root, a relative source path, and the
grammar to use, its simple API performs one operation:

```text
load source bytes
  → derive VER from the grammar and Squatter configuration
  → use .tree-squatter/PATH/VER.squat when it exactly matches
  → otherwise parse, pack, publish it, and retire stale source generations
  → return the captured source and matching tree
```

For a source at `./PATH`, the cache directory is:

```text
./.tree-squatter/PATH/
```

Examples:

```text
./src/main.rs       → ./.tree-squatter/src/main.rs/v1_<token>.squat
./templates/a.html  → ./.tree-squatter/templates/a.html/v1_<token>.squat
./Makefile          → ./.tree-squatter/Makefile/v1_<token>.squat
```

The MVP is project-local and Rust-only. It caches regular files already present
on disk. It does not cache stdin, editor buffers, injected-language subtrees,
query results, or incremental parse state. It has no database, content-addressed
object store, source-reference table, global cache, daemon, reader lease, or
background garbage collector. Cache updates perform bounded-scope cleanup in the
one source directory they update.

The source bytes returned by `load` are the exact bytes used to validate or build
the returned tree. Cache availability never affects matching correctness: any
cache error falls back to parsing the captured source.

An advanced split-phase API may expose a cache tree tentatively when source length
and mtime match, then validate it against a concurrently loaded and hashed source.
The tentative result is explicitly fallible and never weakens the guarantees of
the simple `load` operation.

## Decisions on language selection and grammar identity

### Language selection stays with the caller

The persistence library should not contain an extension-to-language registry.
The caller resolves a path to a grammar and passes that grammar to `load`.

Language selection is application policy rather than persistence policy:

- Extensions can be ambiguous or overridden by configuration.
- Some tools use filenames, compound extensions, shebangs, or file contents.
- ast-grep supports built-in and dynamically loaded grammars.
- Two tools can intentionally parse the same path differently.

The normal call site is therefore:

```rust
let grammar = language_registry.resolve(&relative_path, &configuration)?;
let loaded = persistence.load(&relative_path, &grammar, &mut parser)?;
```

This remains one persistence operation. The registry lookup is a small tool-layer
decision made before it. A higher-level application wrapper may combine resolution
and loading for convenience without moving the registry into this crate.

Grammar choice contributes to the variant filename. A tool can therefore look up
its exact expected variant without opening a file produced by another grammar.
Variants made from the same source bytes coexist: a cache built with grammar A is
still useful to a later invocation using grammar A even though grammar B cannot
consume it. A source-content change makes every existing variant stale, at which
point the next writer removes them without interpreting their grammar identities.

### Compare generated grammar fingerprints

Pointer equality, language name, Tree-sitter ABI version, node-kind lists, crate
version, and `TSLanguageMetadata` semantic version are insufficient proofs that
two grammars parse identically. Metadata is useful for diagnostics and quick
rejection, but two parser tables or external scanners can differ while those
values remain the same.

Use a 32-byte `GrammarFingerprint` derived from the actual grammar implementation:

```text
BLAKE3(
  "tree-squatter grammar fingerprint v1" ||
  length(parser.c) || parser.c ||
  each external scanner source in canonical name order ||
  generation/build inputs that can alter parser behavior
)
```

The exact canonical encoding must be length-delimited and covered by fixtures.
For a generated Rust grammar crate, compute the digest at build or release time
and expose it as a constant beside the language constructor. Comparing grammars
during `load` then costs one 32-byte comparison; grammar source or machine code is
not hashed per source file.

For bundled grammars that do not yet export fingerprints, the consuming build can
generate a manifest from their packaged `parser.c`, scanner sources, and relevant
compile definitions. A crate name/version is acceptable as an additional namespace
but not as the fingerprint itself. A grammar upgrade changes the fingerprint even
when its language name and ABI version do not.

For a dynamically loaded grammar, prefer a signed or packaged manifest containing
the same artifact digest. Otherwise hash the actual dynamic-library file once when
loading it and memoize the result for that loaded-library handle. Do not hash the
library for every source file.

The persistence library cannot derive this exact digest from an opaque
`tree_sitter::Language`; the caller supplies a language and its matching digest as
one `Grammar` value:

```rust
#[derive(Clone)]
pub struct Grammar {
    language: tree_sitter::Language,
    fingerprint: GrammarFingerprint,
}
```

Supplying a truthful pairing is part of the grammar-provider contract. Squatter's
checked loader can reject many incompatible grammars, but it cannot prove that a
caller associated a `Language` with the right artifact digest.

In addition to the grammar fingerprint, store a `RepresentationFingerprint`
provided by `tree-sitter-squatter`. It changes when the slab format or required
interpretation changes, including native byte order, point-column mode, alignment,
Tree-sitter runtime compatibility, or other layout-affecting features. Grammar and
representation identity are separate so persistence-only changes do not invalidate
trees.

Per-tree Squatter packing flags are a separate canonical fixed-width value. A flag
change must produce a different variant even when the representation format can
decode both forms. Parse options that affect the tree, such as included ranges,
must likewise enter the identity before they are supported. All parse-option bits
are zero for the whole-file MVP.

### Variant token

`VER` is an opaque, filename-safe lookup token rather than a semantic version:

```text
variant_digest = BLAKE3(
  "tree-squatter cache variant v1" ||
  GrammarFingerprint ||
  RepresentationFingerprint ||
  canonical Squatter packing flags ||
  canonical parse options
)

VER = "v1_" || base32_lower_no_padding(first_128_bits(variant_digest))
```

The result has a three-character algorithm prefix and 26 Base32 characters, for
example `v1_k3j5...7m.squat`. The prefix versions token derivation; it is not the
grammar's human-readable version. Source contents are deliberately absent, so an
edit atomically replaces the same variant pathname.

The envelope stores the full grammar and representation fingerprints, all flags,
and parse options. The 128-bit pathname token is only a lookup hint. A truncated
hash collision therefore produces an envelope mismatch and rebuild, never an
incorrect hit. Neither lookup nor cleanup parses tokens belonging to other
versions of this library.

## Public API

The primary API is deliberately small and synchronous:

```rust
pub struct Persistence {
    root: PathBuf,
    cache_root: PathBuf,
    squatter_options: SquatterOptions,
}

pub struct Grammar {
    language: tree_sitter::Language,
    fingerprint: GrammarFingerprint,
}

pub struct LoadedFile {
    source: Arc<[u8]>,
    tree: LoadedTree,
    cache_outcome: CacheOutcome,
}

pub enum LoadedTree {
    Mapped(MappedTree),
    Owned(tree_sitter_squatter::Tree),
}

pub enum CacheOutcome {
    Hit,
    Miss(MissReason),
}

impl Persistence {
    pub fn open(
        root: impl Into<PathBuf>,
        squatter_options: SquatterOptions,
    ) -> Result<Self, Error>;

    pub fn load(
        &self,
        relative_path: &Path,
        grammar: &Grammar,
        parser: &mut tree_sitter::Parser,
    ) -> Result<LoadedFile, LoadError>;
}

impl LoadedFile {
    pub fn source(&self) -> &[u8];
    pub fn tree(&self) -> &tree_sitter_squatter::Tree;
    pub fn cache_outcome(&self) -> &CacheOutcome;
}
```

The caller supplies a parser so existing tools can retain their parser pools,
timeouts, cancellation setup, and thread-local reuse. `load` sets or verifies the
requested language before parsing. The initial cache contract supports a normal
whole-file parse with no included ranges. Add parse options to the cache identity
before supporting other parser configurations.

`SquatterOptions` is owned configuration with a canonical encoding supplied by
Squatter. `Persistence::open` derives its representation and flag identity once;
every `load` combines those values with the requested grammar to derive `VER`.

`LoadedTree::Mapped` owns an mmap-backed descriptor. `LoadedTree::Owned` is the
freshly packed result from a miss or from a cache-publication failure. Returning
the owned result on a miss avoids dropping it merely to reopen and revalidate the
file just written. Subsequent processes can map the published entry.

`LoadError` represents failure to obtain a source/tree pair: invalid path, source
open/read failure, parser cancellation, or packing failure. Cache open, decode,
write, rename, and cleanup failures are recorded in `CacheOutcome` or optional
diagnostics and do not make `load` fail after parsing succeeds.

Do not expose untyped lookup, publish, retire, maintain, or clear methods in the
MVP. The advanced API uses opaque tickets so only the library can validate or
publish a tentative load. This prevents callers from accidentally converting a
tentative tree into a verified source/tree pair.

## Tentative, executor-neutral API

The persistence crate should not depend on Tokio, GPUI, or another executor, and
it should not spawn threads. Use a split-phase synchronous API that a caller can
place on its own executor:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStamp {
    pub len: u64,
    pub mtime: PortableMtime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PortableMtime {
    pub seconds: i64,
    pub nanoseconds: u32,
}

pub trait SourceChunks: Send + Sync + 'static {
    fn len(&self) -> usize;

    /// Returns a nonempty slice beginning at `byte_offset`, or an empty slice at
    /// end of input. The returned bytes remain immutable for `self`'s lifetime.
    fn chunk_at(&self, byte_offset: usize) -> &[u8];
}

pub struct SourceIdentity {
    pub len: u64,
    pub digest: [u8; 32],
}

pub struct TentativeFile {
    tree: Arc<MappedTree>,
    ticket: ValidationTicket,
}

pub struct SourceSnapshot<S> {
    storage: S,
    identity: SourceIdentity,
    observed_file: Option<FileStamp>,
}

pub struct VerifiedFile<S> {
    source: SourceSnapshot<S>,
    tree: LoadedTree,
    cache_outcome: CacheOutcome,
}

pub struct Rebuild<S> {
    // Opaque rejected-load state, including SourceSnapshot<S>.
}

pub enum TentativeProbe {
    Hit(TentativeFile),
    Miss(TentativeMissReason),
}

pub enum Verification<S> {
    Confirmed(VerifiedFile<S>),
    Rejected(Rebuild<S>),
}

pub enum Validation<S> {
    Confirmed(VerifiedFile<S>),
    Corrected {
        file: VerifiedFile<S>,
        reason: TentativeMismatch,
    },
}

impl Persistence {
    pub fn load_tentative(
        &self,
        relative_path: &Path,
        observed: FileStamp,
        grammar: &Grammar,
    ) -> Result<TentativeProbe, PathError>;

    pub fn verify<S: SourceChunks>(
        &self,
        ticket: ValidationTicket,
        source: SourceSnapshot<S>,
    ) -> Result<Verification<S>, VerificationError>;

    pub fn rebuild<S: SourceChunks>(
        &self,
        rejected: Rebuild<S>,
        parser: &mut tree_sitter::Parser,
    ) -> Result<VerifiedFile<S>, LoadError>;

    pub fn validate<S: SourceChunks>(
        &self,
        ticket: ValidationTicket,
        source: SourceSnapshot<S>,
        parser: &mut tree_sitter::Parser,
    ) -> Result<Validation<S>, LoadError>;

    pub fn load_from_source<S: SourceChunks>(
        &self,
        relative_path: &Path,
        grammar: &Grammar,
        source: SourceSnapshot<S>,
        parser: &mut tree_sitter::Parser,
    ) -> Result<VerifiedFile<S>, LoadError>;
}
```

Names are provisional, but the separation is intentional:

- `load` remains the safe, simple operation that owns file I/O and returns only a
  verified source/tree pair.
- `load_tentative` performs no source read. It compares the supplied length and
  mtime with advisory metadata in the exact `VER.squat`, maps the tree, and returns
  an opaque validation ticket. `TentativeFile::tree()` is visibly tentative in the
  type system. Missing, incompatible, or malformed cache data produces
  `TentativeProbe::Miss`; only an invalid path/request is an error.
- The caller can split `TentativeFile` into an `Arc<MappedTree>` for immediate use
  and its `ValidationTicket` for a background task. The tree remains alive while
  either side owns it.
- `verify` compares the tentative entry with the exact source identity. A matching
  full digest returns `Confirmed`. A mismatch immediately returns `Rejected`, which
  both tells the caller to stop using the tentative tree and carries everything
  `rebuild` needs to parse and publish the same immutable chunks.
- `validate` is a convenience that performs `verify` followed by `rebuild` and
  reports `Confirmed` or `Corrected`. Interactive callers may prefer the two-step
  form so rejection can reach the UI before a potentially long parse finishes.
- `load_from_source` is the non-speculative chunked equivalent used when
  `load_tentative` misses. It hashes, validates or parses, and publishes while
  preserving the caller's source storage.

`PortableMtime` has checked conversions to and from `SystemTime` and lets adapters
such as Zed's `MTime` avoid exposing platform structs in the cache format. The
existing concrete `LoadedFile` remains the return type of `load`; it has the same
verified semantics as `VerifiedFile<Arc<[u8]>>` without forcing current callers to
adopt generics.

Failure to load or hash the source leaves the tree *unverified*, not *incorrect*.
`Rejected` and `TentativeMismatch` are reported only after a successful full hash
comparison. Once `verify` returns `Rejected`, a later rebuild failure cannot blur
that verdict: callers must stop using the tentative tree even when no replacement
is available.

`SourceSnapshot<S>` couples immutable chunk storage, its exact byte length and
digest, and an optional `FileStamp`. Its normal constructor computes the digest by
walking `chunk_at` from zero. A `SourceHashBuilder` also supports callers that build
a rope or other storage incrementally: feed it the exact parser-visible bytes as
they are produced, then use its result to construct the snapshot without a second
hash pass. A precomputed digest constructor is a caller correctness contract, but
not `unsafe`, because violating it cannot break Rust memory safety.

`SourceChunks::chunk_at` deliberately matches Tree-sitter's chunk callback. Add
implementations or small adapters for `Arc<[u8]>`, a memory map, and rope snapshots.
The parser can consume those chunks directly on a correction instead of flattening
a rope into a contiguous allocation. Hash and parse must observe the same immutable
logical byte stream; for an editor this is the decoded, normalized buffer text,
while `FileStamp::len` still describes the on-disk file.

This shape fits custom executors without embedding executor policy:

```rust
let TentativeProbe::Hit(tentative) =
    cache.load_tentative(path, file_stamp, &grammar)?
else {
    return schedule_normal_chunked_load();
};
let (tree_for_ui, ticket) = tentative.split();

let verification = cx.background_spawn(async move {
    let source = load_rope_and_stream_hash(file).await?;
    cache.verify(ticket, source)
});
```

The application owns cancellation and decides whether a rejection or completed
correction is still relevant. Zed, for example, should associate the verification
and rebuild tasks with its buffer version, stop publishing results from a rejected
tree, and install a corrected tree only if the path, language, and buffer version
still match. The persistence crate should not know about GPUI entities, tasks,
ropes, or version vectors.

Work derived from a tentative tree should carry the same application-level token.
It is suitable for provisional highlighting or navigation that can be replaced;
persisted diagnostics, edits, or refactors should wait for `Confirmed` or use the
corrected tree.

Returning `(TentativeFile, impl Future<...>)` from the library would look compact,
but the future cannot begin concurrently until something spawns or polls it and it
couples the library to the caller's source-loading future. A callback or event
stream has the same executor and cancellation problem. The split-phase ticket is
the recommended API.

## Path mapping

`Persistence::open(root, squatter_options)` captures an absolute project root and
defines `root/.tree-squatter` as the cache root. `load` accepts a relative path
with at least one normal component. Reject absolute paths, `..`, platform prefixes,
an empty path, and paths whose first component is `.tree-squatter`.

Preserve every source-path component, including the final filename, and treat the
result as a per-source cache directory. Place the expected variant directly inside
it as `VER.squat`: `a.rs` becomes `a.rs/v1_<token>.squat`. Thus `a`, `a.rs`, and
`a.squat` remain distinct, and finding the expected variant requires no directory
scan.

Prefer this layout to `PATH.VER_squat`. The suffix layout mixes variants for every
source in the same parent directory and makes cleanup identify which arbitrary
names belong to one source. The per-source directory gives direct O(1) lookup of
the expected pathname and an O(k) scan only on a source-generation change, where
`k` is the number of variants and temporary files for that source.

The source-directory namespace has this permanent compatibility contract:

- Every direct regular file whose name ends in `.squat` is a disposable cache
  variant, regardless of the preceding filename syntax.
- `.source` defines the source generation shared by every named variant.
- A variant whose envelope names that same source generation may coexist with any
  other grammar or Squatter configuration for that generation.
- When `.source` differs from the captured source, every `*.squat` is stale and
  cleanup need not parse its `VER`, envelope, flags, or format.
- Cleanup does not recurse, follow links, or delete unknown non-`.squat` entries.

Different grammar and Squatter identities are incompatible variants, not stale
ones. They remain useful to the tools that created them. Only a source-generation
change retires the complete set. The envelope's own source digest remains the
final correctness check even though `.source` maintains the directory-level
cleanup invariant.

The MVP does not guess that a same-source variant is abandoned merely because a
different one exists. That would require an age, size-budget, or explicit usage
policy and belongs in a later optional cleanup mechanism.

Future library versions must preserve the `.source` format and this contract. They
may use arbitrary new `VER` syntax; an older library can still determine from
`.source` that all variants are stale and collect them by suffix. Any future
non-disposable metadata must use a name without the `.squat` suffix.

A typical directory during a version transition is:

```text
.tree-squatter/src/main.rs/
  .update.lock
  .source
  v1_k3j5...7m.squat
  future-format-that-v1-does-not-understand.squat
  .tmp.<128-bit-random>
```

`.update.lock` is the permanent advisory-lock pathname for this protocol and is
never treated as a variant. `.source` is a small, stable generation marker
containing a magic value, source byte length, and full BLAKE3-256 source digest.
It is independent of `VER` and the Squatter envelope format. If a cache path cannot
be represented safely because an unexpected file, directory, symlink, or reparse
point occupies an internal pathname, parse without persistence rather than
modifying an ambiguous entry.

Require the source handle to refer to a regular file. A symlinked source may be
read if the application permits it, but only cache it when the resolved target is
inside the project root; otherwise parse without persistence. Never follow a
symlink or reparse point for a cache entry or temporary file. If the cache root or
one of its parent components is unexpectedly redirected, disable caching for that
load rather than risk writing outside the project.

Tools that walk the project must exclude `.tree-squatter` explicitly. Repositories
using the cache should add `/.tree-squatter/` to their ignore rules, but the library
does not edit `.gitignore`.

If `load` is called for a path whose source no longer exists, it may best-effort
remove direct variant files in the corresponding source cache directory before
returning the source error. Cache directories for deleted or renamed paths that
are never loaded again can remain; deleting the entire `.tree-squatter` directory
while no participating tools are using it is the simple project-level cleanup
mechanism outside the library API.

## Source generation marker

`.source` has a deliberately permanent, minimal format so libraries that disagree
about every `VER` can still agree on whether the directory is stale. Version 1 is
a fixed 56-byte little-endian record:

```text
8 bytes   magic "TSQSRC01"
2 bytes   marker format version = 1
2 bytes   record length = 56
4 bytes   reserved zero
8 bytes   source byte length
32 bytes  BLAKE3-256 source digest
```

Unknown version, length, or nonzero required fields make the marker mismatched;
they never make a cache hit. Write a new marker completely under a random temp
name and atomically replace `.source`. `VER` formats may evolve independently, but
a future library must continue writing this marker format if it wants old versions
to preserve same-source variants. Changing the source-generation protocol requires
a separately designed migration.

## Cache file format

Each `.squat` file contains a small persistence envelope followed by one compact
Squatter slab. The fixed envelope contains:

- Magic and persistence format version.
- Fixed-header length and total envelope length.
- Source byte length and BLAKE3-256 digest of the captured source.
- Optional advisory on-disk `FileStamp` containing byte length and mtime.
- `GrammarFingerprint`.
- `RepresentationFingerprint`.
- Canonical Squatter packing flags and parse options.
- The full 256-bit variant digest used to derive the abbreviated pathname token.
- Optional language name, ABI version, and semantic version for diagnostics.
- Slab offset, slab length, required alignment, and payload checksum.

`FileStamp` is only permission to return a tentative tree. It is never proof of a
verified hit. The simple loader records a stamp only when metadata sampled before
and after its source read has identical length and mtime. The chunked API accepts a
stamp when the caller asserts that it describes the supplied logical source
snapshot. Cache publication still stores and later checks the full source digest.

Define integer byte order and exact offsets explicitly. Do not serialize Rust
struct memory or use a general serialization format. Check every conversion,
addition, alignment, and range before constructing a slice. Unknown versions,
unknown required flags, duplicate fields, a length mismatch, or trailing ambiguous
data are cache misses.

Align the slab to 64 bytes within the file and map the file from offset zero. This
satisfies ordinary mmap offset constraints while preserving Squatter's internal
alignment. Write the compact representation with unused growth capacity removed.

The checksum catches corruption that still happens to form a structurally valid
slab. On an MVP hit, verify it and then call Squatter's checked borrowed loader.
Both operations touch most or all slab pages, so the MVP must not claim demand-
paged selective I/O during opening. A future lazy-safe loader can change this
without changing the path-based cache model.

## Tentative fast path

`load_tentative` follows this path:

1. Accept a caller-supplied `FileStamp`, normally obtained from a `stat` already
   performed by a worktree or file-watcher layer.
2. Derive the exact `VER.squat`, open it, and map it once.
3. Read the envelope from that mapping and reject unless grammar, representation,
   flags, parse options, on-disk length, and mtime all match.
4. Return the mapped tree and `ValidationTicket` without reading or hashing source
   contents.

Do not create a metadata sidecar for each `VER` and do not make a separate mapping
just for the envelope. On the expected-hit path the tree mapping is needed anyway;
reading its first page supplies the metadata. A small `pread` before mapping saves
a mapping on tentative misses but adds another operation to expected hits. Measure
both approaches, with map-once as the initial implementation.

The hoped-for application-visible latency is therefore an existing or fresh source
`stat`, opening the exact cache path, one `mmap`, and faults for the envelope and
actually accessed tree pages. `mmap` itself does not read the entire file. Path
lookup, opening the cache file, and minimal envelope checks still exist, so describe
benchmarks as “no source read or hash before tentative access” rather than literally
only two system calls.

This latency target conflicts with the current checked Squatter loader and whole-
payload checksum, both of which scan the slab before returning. A tentative API may
return early only after Squatter has a memory-safe constant-time open followed by
checked-on-access or lazily validated columns. Skipping structural checks around
unsafe native access merely because `.tree-squatter` is local is not acceptable.
Until that prerequisite exists, the API shape can be implemented and measured,
but tentative opening will still touch most slab pages.

### Source mmap experiment

An mmap-backed source can implement `SourceChunks` by returning a suffix beginning
at Tree-sitter's requested byte offset. This is worth benchmarking for command-line
tools: hashing walks the mapping sequentially, and a correction parse can use the
same bytes through `Parser::parse_with` without another contiguous copy. Because a
full hash touches every source page and Tree-sitter normally lexes the entire file,
this primarily tests copy, allocation, and syscall reductions rather than less
physical I/O.

It is a weaker fit for Zed. Zed must still decode the file, normalize it into its
rope representation, and display all text. More seriously, a private file mapping
is not an immutable snapshot of a file another process may rewrite or truncate;
on Unix, accessing pages beyond a new end of file can raise `SIGBUS`. A before/after
metadata check detects some races but does not make accesses safe or ensure that
hashing and parsing observed identical bytes.

Keep source mmap as an opt-in experiment for files assumed stable. The correctness
path reads or decodes into owned immutable storage, then lets hashing and
Tree-sitter consume that same `SourceChunks` value. For Zed, a rope adapter plus a
streaming hash during rope construction is the preferred experiment.

## Load algorithm

`load` performs these steps:

1. Validate the relative path; derive the source cache directory, complete variant
   identity, and exact `VER.squat` path.
2. Open the source as a regular file, sample handle metadata, and read it once into
   the buffer that will be returned while computing BLAKE3. Sample the same handle
   again; retain an advisory `FileStamp` only if length and mtime were stable.
3. Try to open the exact variant read-only. Do not read `.source` or enumerate the
   directory on the hit path. On Windows request sharing modes that permit
   replacement/deletion while handles are live.
4. Read and validate the variant envelope. Compare source length/digest, grammar
   fingerprint, representation fingerprint, packing flags, and parse options with
   the current request.
5. If they match, mmap the cache file, verify the payload checksum, and construct
   a checked owning Squatter tree with the supplied `Language`. Return the captured
   source and mapped tree.
6. On any cache miss, configure the supplied parser with the requested language,
   parse the captured source, and pack a compact Squatter tree.
7. Write an envelope and the packed bytes to a uniquely named temporary file in
   the destination cache directory. Flush and close the writable handle.
8. Acquire the source directory's cross-process update lock and re-read `.source`.
   If it now matches, recheck the expected entry: another writer may have published
   it while this process parsed. If that entry validates, discard the temp and use
   it. Other valid variants for this same source remain untouched.
9. If `.source` does not match, stream the directory entries once and delete every
   direct regular `*.squat` file. Ignore the names and envelopes, do not sort or
   accumulate the listing, and never recurse or follow links. If any variant cannot
   be deleted, leave `.source` unchanged, skip publication, release the lock, and
   return the owned tree. This preserves an unambiguous generation for retry.
10. After successfully clearing an old generation, atomically replace `.source`
    with a complete marker for the captured source. A crash before this point
    leaves the old marker and causes cleanup to retry; a crash after it merely
    leaves a current generation with no variant for this identity.
11. Atomically replace the expected variant path with the completed temp. If this
    fails, remove the temp best-effort, release the lock, and return the owned tree.
    Do not delete other variants when `.source` already matched.
12. Release the lock and return the same captured source and freshly owned tree.

Never use size or mtime as proof that the source is unchanged. `load` must read the
source to return it anyway, so hashing that same stream is both exact and cheap
relative to a parse. If another process modifies the source during the read, the
tree still corresponds to the exact buffer returned. A later load hashes its own
captured bytes and will reject a cache entry for a different snapshot.

Identity mismatch, malformed envelope, checksum failure, or Squatter validation
failure all follow the same miss path. A successful replacement repairs the
expected entry. A source-generation transition also cleans every prior variant.
Preserve a detailed internal miss reason for benchmarks and debug logging, but
callers should not need recovery logic.

## Publication and concurrent readers

Write every candidate completely under a random sibling name such as:

```text
.tree-squatter/src/main.rs/.tmp.<128-bit-random>
```

The temp and destination must be on the same filesystem. Close writable handles
before publication. Use the platform's atomic replacement operation; never write,
truncate, or punch holes in the discoverable `.squat` file.

On Unix, replacing the pathname detaches the old inode while existing file handles
and mappings continue to reference it. Readers can fault untouched old pages after
replacement. The old storage becomes reclaimable after the final handle/mapping is
released. This directly solves automatic deletion of the prior cached generation:
replacement of the same `VER` removes its old name. Deleting an obsolete variant
after a source change has the same lifetime behavior. Variants with another `VER`
and the same source generation remain named.

On Windows, open cache readers with read and delete sharing and attempt the native
replacement operation. If an existing mapping, foreign handle, filesystem, or
scanner prevents replacement, leave the old cache file intact, delete the temp if
possible, and return the fresh owned tree. No reader is disrupted. A later load
can retry after the blocking handle disappears. Old-generation deletion can also
fail while a mapped Windows reader is active. Leave `.source` unchanged and skip
publication of the new generation; a later load retries after the reader exits.

Readers never take the update lock. Writers parse and create their temp files
before acquiring it, so the serialized section contains only revalidation,
publication, and one small directory scan. Use an advisory lock whose ownership
the OS releases on process exit, stored as a fixed non-`.squat` entry in the source
cache directory. If locking is unsupported or fails, skip publication and cleanup
and return the valid owned result.

The interoperable lock name and operation are part of the path protocol. On Unix,
open `.update.lock` without following links and take an exclusive `flock`. On
Windows, open the same file with cooperative sharing and take an exclusive
`LockFileEx` lock over its first byte; initialize the file to at least one byte.
Future implementations must use this same lock before publishing or deleting a
variant, even when they use a different `VER` algorithm.

The lock serializes `.source` transitions with variant publication. Writers that
captured the same source generation add or replace their own variants without
deleting one another. Writers that captured different generations cannot
interleave deletion, marker replacement, and publication. The final writer can
still have captured a source snapshot that is no longer on disk, but its envelope
names those exact bytes and a later load detects and repairs that state.

The hit path opens `VER.squat` directly and does not need `.source`. A new grammar
or flag variant reads `.source` under the update lock but publishes without
enumeration when the source generation is unchanged. Directory enumeration happens
only when `.source` proves that every old variant is stale, and examines the one
source directory. Its cost is small beside the parse, pack, and write already
required by that source change. Streaming the listing makes memory use constant
even if the directory is unexpectedly large.

A crash before replacement leaves a temp file, and a crash after replacement
leaves a complete cache entry. Source-generation cleanup may also remove sibling
temps matching the library's exact naming pattern. Temp cleanup is best-effort and
never follows links. The whole cache remains disposable.

## Mmap and tree ownership

The current `tree-sitter-squatter::BorrowedTree<'a>` safely borrows slab bytes but
cannot be stored beside the mmap it borrows using ordinary safe Rust. Add an
owning-byte abstraction to Squatter rather than fabricating a `'static` lifetime
in persistence or ast-grep:

```rust
pub struct OwnedTree<B> {
    tree: Tree,        // destroyed first
    owner: Pin<Box<B>>,
}

impl<B: ImmutableBytes> OwnedTree<B> {
    pub fn from_owner(language: &Language, owner: B) -> Result<Self, Error>;
    pub fn tree(&self) -> &Tree;
    pub fn owner(&self) -> &B;
}
```

`ImmutableBytes` must be sealed or have an explicitly audited unsafe contract: the
byte address and length remain stable and contents remain immutable for the tree's
lifetime. Pin the mapping owner before calling `sq_tree_from_bytes_borrowed`.
Declare fields or implement `Drop` so the runtime tree descriptor is destroyed
before unmapping and closing its file.

Persistence then defines `MappedTree` around `OwnedTree<MappedFile>`. Nodes and
cursors borrow the tree normally, so they cannot outlive the mapping in safe Rust.
Dropping `Persistence` does not affect already returned `LoadedFile` values.

## Crate layout and dependencies

Keep the implementation in one new workspace crate:

```text
crates/persistence/
  Cargo.toml
  src/
    lib.rs          # public load API and result types
    grammar.rs      # fingerprints and Grammar
    variant.rs      # canonical identity and concise VER derivation
    source.rs       # stable .source generation marker
    format.rs       # canonical envelope codec
    mapping.rs      # mmap owner and MappedTree construction
    path.rs         # validated source/cache path mapping
    load.rs         # single load state machine
    publish.rs      # writer lock, generation changes, and publication
    platform.rs     # small Unix/Windows differences
  tests/
    format.rs
    load.rs
    lifecycle.rs
```

Directly depend on the workspace's exact `tree-sitter` and
`tree-sitter-squatter` crates. Cargo's `links = "tree-sitter"` constraint keeps one
Tree-sitter native provider in the dependency graph. Likely supporting crates are
`blake3`, an mmap crate, and small audited platform bindings for Windows sharing
and replacement. Avoid pulling in a database, async runtime, generic serializer,
file watcher, or global cache-directory abstraction.

Before publishing independently, package Squatter's C sources and headers inside
`tree-sitter-squatter`; its current build reaches outside its crate and the package
is marked `publish = false`.

## Tests

Format tests use fixed binary fixtures for the `.source` marker, source digest,
grammar fingerprint, representation fingerprint, packing flags, variant
digest/token, envelope bytes, alignment padding, and malformed lengths. Fuzz
envelope decoding and every checked arithmetic boundary.

Path tests cover nested files, extensionless files, existing `.squat` suffixes,
per-source directories, arbitrary unfamiliar variant names, non-UTF-8 Unix paths,
Windows separators/prefixes, `.` and `..`, symlinks, cache-root redirection,
case-sensitive/case-insensitive filesystems, and source paths inside
`.tree-squatter`.

Differential tests load the same corpus through native Tree-sitter, freshly packed
Squatter, cache miss, and mmap hit. Compare node kinds, grammar symbols, fields,
flags, byte/point ranges, traversal order, errors, and ast-grep match/output results.

Use deterministic subprocess barriers for lifecycle tests:

1. Reader A maps a cache entry and pauses before touching coordinate columns.
   Writer B loads changed source and atomically replaces the entry. Reader A then
   reads cold pages and completes with its original source/tree pair.
2. A reader races replacement between open and mmap and obtains either a complete
   old entry, a complete new entry, or a miss—never partial bytes.
3. Two writers for the same variant publish different captured snapshots. The
   final cache contains one complete snapshot; a subsequent load validates against
   current source and repairs a stale winner.
4. Two grammars with the same name/ABI/semantic version but different parser tables
   have different fingerprints and variant paths and never reuse one another's
   entry. Both variants remain reusable while the source digest is unchanged.
5. Changing only external scanner code invalidates the entry. Reusing the same
   grammar constant across many files performs no per-file grammar hashing.
6. Crash during temp write leaves the discoverable entry unchanged. A crash after
   old-generation deletion but before `.source` replacement retries cleanup. A
   crash after marker replacement but before variant publication leaves a valid
   empty generation. OS lock ownership is released in every case.
7. Windows replacement succeeds with cooperative sharing where supported; when a
   mapped or foreign handle blocks it, the writer returns a valid owned tree and a
   later load retries without disrupting the reader.
8. Source changes with unchanged size and restored mtime, corrupt checksum,
   checksum-valid malformed slab, incompatible representation, full disk, and
   unwritable cache all return the correct source/tree or a source-level error.
9. Changing only Squatter packing flags changes `VER`. The new and old variants
   both remain and each produces a hit for its own configuration while source bytes
   remain unchanged.
10. A source change makes `.source` mismatch. Under the writer lock, the update
    streams the directory and deletes every direct regular `*.squat`, including an
    arbitrary filename that the current version cannot parse. It ignores
    subdirectories and non-`.squat` files.
11. Concurrent same-generation writers retain both different variants. Writers
    for different source generations cannot interleave cleanup and publication. A
    Windows deletion blocked by an old mapping leaves `.source` unchanged, skips
    publication, and is retried on a later update.
12. A valid hit opens the exact `VER.squat` without reading `.source` or enumerating
    its directory. Adding a variant for unchanged source reads the marker but still
    avoids enumeration.
13. A file rewritten with different bytes but identical length and restored mtime
    produces a tentative hit followed by `Verification::Rejected`; rebuilding from
    the supplied chunks returns the correct tree. A genuine match returns
    `Confirmed` without parsing.
14. Source-load failure leaves the tentative state unverified. A rebuild failure
    after rejection never reports the tentative tree as confirmed.
15. `Arc<[u8]>` and rope adapters produce identical hashes and trees across empty,
    one-byte, Unicode, CRLF-normalized, and adversarial chunk boundaries. A digest
    accumulated during source construction matches a later chunk walk.
16. Tentative loading uses the caller's supplied `FileStamp`, performs no source
    read, and issues no directory enumeration. Executor integration tests run the
    same ticket on GPUI or a minimal test executor without a persistence dependency
    on either runtime.

Measure source read/hash, `.source` check, envelope check, mmap, checksum, Squatter
validation, parse, pack, temp write, generation cleanup, and replacement separately.
Compare hits and misses to ast-grep's current parser reuse. Because checked opening
scans the slab, report it honestly; selective page-in is a later milestone.

## Implementation sequence

1. Add `GrammarFingerprint` and a generated fixture for one grammar. Add a stable
   `RepresentationFingerprint` and owning-byte tree API to Squatter.
2. Freeze the `.source` marker, envelope, variant-token algorithm, per-source
   directory contract, and path mapping with fixtures and fuzzed decoding.
3. Implement `Persistence::load` using ordinary owned reads/trees, then add mmap
   hits without changing the public operation.
4. Add `SourceChunks`, `SourceSnapshot`, streaming hashing, and the split-phase
   tentative/verify/rebuild API without taking a dependency on an async executor.
5. Implement the per-source update lock, source-generation transition, atomic
   replacement, opaque stale-variant cleanup, and old-reader lifecycle tests on
   Linux and macOS.
6. Implement Windows locking, sharing, replacement/deletion behavior, and fallback
   tests.
7. Integrate ast-grep's language registry and thread-local parsers at the CLI file-
   loading boundary. Exclude `.tree-squatter` from traversal.
8. Prototype Zed integration with GPUI-owned tasks and a rope `SourceChunks`
   adapter. Benchmark map-once versus header-`pread` tentative probing and owned
   source reads versus source mmap.
9. Run semantic differential tests and end-to-end performance measurements before
   enabling the cache by default.

The MVP is complete when one `load` call always returns matching source/tree data,
repeat loads directly hit `./.tree-squatter/PATH/VER.squat`, grammar or Squatter
flag changes select another reusable `VER`, source changes retire every old-source
variant without interpreting its name, Unix readers survive replacement/deletion
while accessing cold pages, Windows readers are never disrupted, and all cache
failures fall back to parsing. The advanced API additionally reports tentative
trees distinctly, confirms or rejects them using the exact chunk-stream digest,
and leaves task scheduling to the caller's executor.
