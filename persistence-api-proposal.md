# Cache API proposal

Proposed API, not implemented. Signatures below omit method bodies and unchanged
maintenance and transfer APIs. Storage changes are specified below.

See [client examples](cache-api-examples.md) for usage patterns and remaining gaps.

Rename the exported `tree_squatter::PackContext` to `TreePacker`, retaining
its `new`, `pack`, `pack_with_options`, and `trim` methods.

One shared project cache, one mutable loader per worker. `load` never parses.
An asynchronous `Source` supplies metadata, preprocessed streams, and prepared
input. `ParserInput::read` supplies synchronous chunks from that prepared snapshot.
`preview` permits speculative tree access using raw file metadata; `verify`
confirms correspondence to the source once it is available.
Each loader owns an `Arc<Cache>` and can move independently between threads.
`Cache` is `Send + Sync` through its fields, but does not implement `Clone`.
Clone the `Arc` to share it; no manual unsafe trait implementations are needed.

The directory registry shares `Arc<Cache>` instead of `Arc<Store>`.
Pending work, maintenance, and snapshot admission owners
retain `Arc<Cache>`. The policy for repeated opens with conflicting
`CacheOptions` remains to be decided.
If a `Cache` starts without a store, it remains without one for its lifetime,
even if another process creates the cache. Registry reuse does not upgrade it.

```rust
pub struct Cache {
    root: PathBuf,
    store: Option<Box<Store>>, // absent when the cache could not be opened
    options: CacheOptions,
}

// Defaults: 256 MiB map ceiling, owned reads, create missing caches.
pub struct CacheOptions {
    pub map_size: usize,
    pub read: ReadPolicy,
    pub create_cache_if_absent: bool,
}

// Reuse tree_squatter::PackOptions:
// initial_group_capacity: 0, repack: false, symbol_presence: true, points: true.
// Only symbol_presence and points select the cached tree variant; capacity and
// repack control fresh packing. Persisted trees are always compact.

pub struct Loader {
    cache: Arc<Cache>,
    parser: tree_sitter::Parser,
    packer: Option<TreePacker>, // allocated on the first parse that needs packing
}

impl Cache {
    // With create_cache_if_absent: false, a missing cache leaves store absent; parsing still works.
    pub fn open(root: impl AsRef<Path>, options: CacheOptions) -> io::Result<Arc<Self>>;
    pub fn loader(self: &Arc<Self>) -> Loader;

    pub fn grammar(
        &self,
        language: &tree_sitter::Language,
        fingerprint: GrammarFingerprint,
    ) -> Result<Grammar, tree_squatter::Error>;
}

impl Loader {
    // Asynchronously hashes preprocessed source, then checks the cache. Never parses.
    pub async fn load<S: Source>(
        &self,
        source: &mut S,
        grammar: &Grammar,
        options: LoadOptions<'_>,
    ) -> Result<LoadResult, LoadError>;

    // Metadata-only source access; cached-tree decoding remains synchronous.
    pub async fn preview<S: Source>(
        &self,
        source: &mut S,
        grammar: &Grammar,
        options: LoadOptions<'_>,
    ) -> Result<Option<CachedCandidate>, LoadError>;

    // The next parse that needs packing recreates the packer lazily.
    pub fn drop_packer(&mut self) {
        self.packer = None;
    }
}

// Compare mtimes for equality only.
//
// See ["mtime comparison considered harmful" - apenwarr](https://apenwarr.ca/log/20181113)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileModificationTime(SystemTime);

impl FileModificationTime {
    pub fn new(time: SystemTime) -> Self {
        Self(time)
    }
}

#[must_use]
pub enum LoadResult {
    Loaded(LoadedTree),
    Miss(CacheMiss),
}

// Internal request state shared by lookup, verification, parsing, and publication.
struct LoadRequest {
    cache: Arc<Cache>,
    path: PathBuf, // client-supplied project-relative identity
    metadata: FileMetadata, // raw disk capture, before preprocessing
    grammar: Grammar,
    pack: PackOptions,
}

// Identity of transformed parser input, separate from raw disk metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceIdentity {
    pub hash: u128, // XXH3-128 of parser input after preprocessing
    pub byte_len: u64,
}

// A structurally validated tree whose correspondence to current source is unverified.
#[must_use]
pub struct CachedCandidate {
    miss: CacheMiss, // contains the candidate's recorded source identity
    tree: LoadedTree,
}

impl CachedCandidate {
    // Enables speculative structural work; no source text is retained.
    pub fn tree(&self) -> &tree_squatter::Tree;

    // Uses the source's current preprocessing, never the cached preprocessing record.
    pub async fn verify<S: Source>(
        self,
        source: &mut S,
        options: VerifyOptions<'_>,
    ) -> Result<Verification, LoadError>;
}

#[must_use]
pub enum Verification {
    Confirmed(LoadedTree),
    Changed(LoadResult),
}

// Owns source identity, grammar, originating cache, and packing settings.
// Retains no parser borrow, database transaction, or work lock.
#[must_use]
pub struct CacheMiss {
    request: LoadRequest,
    source_identity: SourceIdentity,
}

impl CacheMiss {
    // Parses the supplied snapshot without rechecking the cache or waiting for other work.
    pub fn parse<I: ParserInput>(
        self,
        loader: &mut Loader,
        input: &mut I,
        options: ParseOptions<'_>,
        handle_write: impl FnOnce(PendingWrite),
    ) -> Result<LoadedTree, LoadError>;
}

#[derive(Clone)]
pub enum LoadedTree {
    // Tree owns its slab allocation; no database transaction is retained.
    Owned(Arc<tree_squatter::Tree>),
    // Tree reads LMDB bytes held alive by an owning read transaction.
    Backed(Arc<tree_squatter::BackedTree>),
}

impl LoadedTree {
    pub fn tree(&self) -> &tree_squatter::Tree;
    pub fn transaction_backed(&self) -> bool;
    // Copies transaction-backed storage; existing aliases keep their snapshots.
    pub fn detach(&self) -> Result<Self, tree_squatter::Error>;
}

// None disables cancellation: is_cancelled returns false and cancel is a no-op.
#[derive(Clone, Copy, Default)]
pub struct Canceler<'a>(Option<&'a AtomicBool>);

impl<'a> Canceler<'a> {
    pub fn new(flag: &'a AtomicBool) -> Self {
        Self(Some(flag))
    }

    pub fn can_cancel(&self) -> bool {
        self.0.is_some()
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    pub fn cancel(&self) {
        if let Some(flag) = self.0 {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

// Opaque; private representation is intentionally unspecified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextPreprocessing {
    /* private fields */
}

impl Default for TextPreprocessing {
    fn default() -> Self {
        Self::zed()
    }
}

impl TextPreprocessing {
    pub fn none() -> Self;
    pub fn zed() -> Self;

    // Writes parser input to caller-owned storage; cancellation returns Interrupted.
    // Streaming where supported; Zed's detection fallback may buffer the input.
    pub fn apply(
        &self,
        input: impl io::Read,
        output: impl io::Write,
        options: PreprocessingOptions<'_>,
    ) -> io::Result<()>;
}

#[derive(Default)]
pub struct PreprocessingOptions<'a> {
    pub cancellation: Canceler<'a>,
}

#[derive(Default)]
pub struct LoadOptions<'a> {
    pub pack: PackOptions,
    pub cancellation: Canceler<'a>,
}

#[derive(Default)]
pub struct VerifyOptions<'a> {
    pub cancellation: Canceler<'a>,
}

#[derive(Default)]
pub struct ParseOptions<'a> {
    pub cancellation: Canceler<'a>,
}

#[derive(Default)]
pub struct PublishOptions<'a> {
    pub cancellation: Canceler<'a>,
}

#[must_use = "publish, transfer, queue, or explicitly discard this write"]
pub struct PendingWrite {
    request: LoadRequest,
    source_identity: SourceIdentity,
    preprocessing: PreprocessingInfo, // descriptive record from the parsed capture
    tree: LoadedTree,
    source: Arc<[u8]>, // retained only for publication under the current source-storing schema
}

impl PendingWrite {
    // Does not consume the work; busy or failed publication can be retried.
    pub fn publish(&self, options: PublishOptions<'_>) -> Result<WriteOutcome, CacheError>;
}

```

## Source and transformation contract

`Source` owns the file path, preprocessing choices, raw metadata, and byte access. Its
asynchronous methods permit I/O without imposing an executor. `futures_io::AsyncRead`
is the stream interface; disk adapters supply their own I/O backend or blocking
pool. In-memory sources complete immediately. Async syntax alone does not make
blocking filesystem calls or CPU-heavy decoding nonblocking.

```rust
pub trait Source {
    type Reader<'a>: futures_io::AsyncRead + Unpin where Self: 'a;
    type Input<'a>: ParserInput where Self: 'a;

    fn path(&self) -> &Path; // client-supplied project-relative identity
    fn preprocessing(&self) -> TextPreprocessing;
    async fn metadata(&mut self) -> io::Result<FileMetadata>;

    // Starts at processed byte zero. Never returns raw bytes needing decoding.
    async fn reader(&mut self, options: ReadOptions<'_>) -> io::Result<Self::Reader<'_>>;

    // Prepares an immutable snapshot; subsequent read calls require no I/O.
    async fn prepare(&mut self, options: ReadOptions<'_>) -> io::Result<Self::Input<'_>>;

    // Hashes a checked capture, without retaining text where preprocessing streams.
    async fn hash(&mut self, options: ReadOptions<'_>) -> io::Result<FileFingerprint>;
}

pub trait ParserInput {
    type Chunk: AsRef<[u8]>;

    fn path(&self) -> &Path; // same logical identity as the source capture
    fn metadata(&self) -> FileMetadata;
    fn preprocessing_info(&self) -> &PreprocessingInfo;
    fn read(&mut self, byte_offset: usize, position: Point) -> Self::Chunk;
}

// Describes the capture; never selects decoding during lookup or verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreprocessingInfo {
    pub mode: TextPreprocessing,
    pub requested_encoding: Option<String>, // canonical name if explicitly selected
    pub encoding: Option<String>, // effective encoding; None for unchanged bytes
    pub had_bom: bool,
    pub line_ending: Option<LineEnding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding { Lf, CrLf }
```

`Source::hash` is a source-level operation so file adapters can restart speculative
UTF-8 decoding and its hasher without publishing incorrect stream bytes. A generic
`reader` cannot retract bytes: automatic detection may require buffering before
returning a committed stream. The two methods must produce identical processed
identities. Source implementations perform before/after metadata checks for both
hashing and preparation. A failed or cancelled capture returns no usable result.

`ParserInput::read` uses Tree-sitter's offset/point arguments and `AsRef<[u8]>`
output contract. It may revisit offsets; empty chunks signal EOF. Mutable cursor
state is allowed, but the exposed text must not change. A flat buffer can return
its entire remaining suffix. An adapter passes `|offset, point| input.read(offset,
point)` to Tree-sitter. `Chunk` has a fixed type, so borrowed chunks borrow the
underlying source snapshot, not a scratch buffer overwritten by the next call.
The associated lifetimes on `Source` permit such borrowing without `dyn` or boxing.

Parsing is synchronous and should run on a CPU/blocking worker. Async preparation
finishes before parsing starts. Read errors occur during preparation, not as fake
EOF in the Tree-sitter callback. The traits use static dispatch; cross-thread
futures require explicit `Send` guarantees from an adapter. The eventual public
trait must specify those guarantees before stabilizing; bare `async fn` does not
promise `Send`. Owned snapshots can move to workers; borrowed inputs need scoped
work or a source owner moved alongside the job.

All streams and parser inputs contain **already preprocessed bytes**. The cache
never decodes them again. Every tree byte offset and point uses these coordinates.

`TextPreprocessing::none()` preserves bytes. `zed()` reproduces Zed's initial
file-loading behavior: binary admission checks, encoding detection/decoding,
BOM removal, malformed-input replacement, and CRLF/lone-CR normalization.
`TextPreprocessing::default()` uses `zed()`; disk sources default to that policy. Callers wanting unchanged bytes explicitly select `none()`.
An existing preprocessed rope needs no conversion or flattening. Output errors
or cancellation invalidate any partial output. Automatic detection can abandon
its streaming attempt and restart with buffered decoding. Helpers with resettable
sinks discard partial output and hashes before restarting. `apply` accepts an
arbitrary writer, so it must buffer uncertain output when it cannot retract it;
it must never append fallback output after an abandoned decoded prefix.

`FileMetadata::byte_len` and `mtime` describe the **raw disk capture**, before
preprocessing. Raw length need not equal parser-input length. A rope supplied through
this API must represent that disk capture under the selected preprocessing; arbitrary
edited buffers must not establish a disk-metadata association.

`FileModificationTime` follows [Zed's newtype](https://github.com/zed-industries/zed/blob/main/crates/fs/src/fs.rs):
private `SystemTime`, equality and hashing, no ordering or arithmetic traits.
Newline conversion matches [Zed's CRLF and lone-CR normalization](https://github.com/zed-industries/zed/blob/main/crates/worktree/src/worktree.rs).

## Standalone preprocessing crate

Create a crate with no Zed-crate dependencies, structured so Zed could adopt it
directly. Copy the decoding/detection functions and tests verbatim from Zed
revision `74646bf29c1a3d1cdb178930d810ecc5a8a6bece`, retaining attribution and
license notices. Keep copied code identifiable; isolate the minimal adapters
needed to replace Zed filesystem, rope, scheduling, and text-type dependencies.
Do not rewrite the algorithms while extracting them.

Sources: [file_content.rs](https://github.com/zed-industries/zed/blob/74646bf29c1a3d1cdb178930d810ecc5a8a6bece/crates/language/src/file_content.rs),
[streaming loading](https://github.com/zed-industries/zed/blob/74646bf29c1a3d1cdb178930d810ecc5a8a6bece/crates/worktree/src/worktree.rs#L7273),
[reload decoding](https://github.com/zed-industries/zed/blob/74646bf29c1a3d1cdb178930d810ecc5a8a6bece/crates/language/src/buffer.rs#L1651),
and [newline normalization](https://github.com/zed-industries/zed/blob/74646bf29c1a3d1cdb178930d810ecc5a8a6bece/crates/text/src/text.rs#L3628).

The crate supports automatic detection and explicitly selected encodings,
including reload using the current encoding, BOM overrides, forced non-Unicode
decoding without BOM handling, and replacement of malformed input. Report the
effective encoding, BOM presence, and original line-ending style to the caller.
Keep editor state and cache storage outside the crate. Use `encoding_rs` and
`chardetng` as Zed does; caller adapters own output storage and scheduling.

Initially the cache-facing `TextPreprocessing` exposes only `none()` and `zed()`
constructors. Explicit encoding selection is supported by the standalone crate;
exposing that selection through the opaque cache profile is a later API addition.
A custom `Source` may select an encoding independently of these constructors.
Record that choice and its observed outcome in `PreprocessingInfo`. Version the
stored description format for compatibility, but do not include it in tree identity.

`TextPreprocessing` is the working name. Alternatives: `TextProcessing` (shorter,
less specific), `TextPreparation` (describes preparing parser input), or
`TextDecoding` (familiar, but understates newline normalization). `Reencoding`
suggests writing another encoding and does not describe byte-preserving mode well.

## Paths and cache-root selection

Use plain `Path`/`PathBuf`; do not copy Zed's `RelPath` or introduce a path wrapper.
`Source::path()` is the client-supplied logical identity relative to the selected
project/cache root. Store it without lexical normalization, canonicalization, or
path validation. Use native path bytes, without a Unicode-only restriction.

Document these client responsibilities on source constructors and cache entry
points: choose a consistent root and relative spelling, supply the same identity
for a capture and its prepared input, and ensure the source represents the file
intended by that identity. The library does not check absolute paths, `..`, empty
paths, reserved directories, root containment, or source/input path agreement.
Clients wanting canonical root identity can canonicalize the root themselves.
Alternate spellings or aliases are not guaranteed to share a cache entry.

Preserve symlink components in the logical identity. `project/link.rs` belongs to
that project's cache even when opening it follows a target outside the project.
`SourceFile` follows ordinary filesystem opening semantics; it does not resolve a
target to choose another cache. Relative paths are identities, not containment
proofs. Source-opening restrictions, if needed, belong to the client.
Clients may choose another identity policy, such as following symlinks while
retaining the last path under the project root. The cache does not implement or
require any particular symlink-resolution policy.

Read metadata and contents from the same open handle. Retargeting a symlink can
make a metadata-only preview stale; processed-content verification determines
whether its tree is reusable. If preparation reopens or rereads the source, the
processed hash must still match before publication. This content validation is
independent of the intentionally absent path validation.

There is no `persistable` flag or path-based eligibility gate. Lack of a local
store still allows transferable publication work. Cache storage continues to use
encoded/hashed database keys; a source path is not a cache output-file location.
The same client-owned path convention applies to transfer and maintenance APIs.

## File metadata and hashing utilities

Metadata conversion remains synchronous and pure; file access and contents/hash
utilities follow the asynchronous `Source` interface. `SourceFile` is the supplied
disk adapter (backend selection remains an integration choice); `FileContents`
is an immutable in-memory source.

`SourceFile::open(root, path)` stores `path` as its logical identity and opens
`root.join(path)`, without validation or normalization. Both arguments are supplied
by the client. Snapshot constructors take that same project-relative identity;
the loader copies it directly into `LoadRequest` rather than deriving it from an
absolute filesystem path. Prepared input retains the capture's logical identity.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    pub byte_len: u64, // raw disk bytes
    pub mtime: FileModificationTime,
}

impl FileMetadata {
    pub fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self>;
}

#[derive(Default)]
pub struct ReadOptions<'a> {
    pub cancellation: Canceler<'a>,
}

pub struct SourceFile { /* private backend, handle, policy, and prepared storage */ }
pub struct FileContents { /* immutable path, bytes, metadata, and preprocessing record */ }
pub struct ChunkSource<'a> { /* owned path, borrowed chunk index, metadata, preprocessing record */ }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFingerprint {
    pub metadata: FileMetadata,
    pub preprocessing: PreprocessingInfo,
    pub source: SourceIdentity,
}

impl SourceFile {
    pub async fn open(
        root: impl AsRef<Path>,
        path: impl AsRef<Path>,
    ) -> io::Result<Self>;
    pub fn with_preprocessing(self, preprocessing: TextPreprocessing) -> Self;
    pub async fn read(&mut self, options: ReadOptions<'_>) -> io::Result<FileContents>;
}

impl FileContents {
    // Bytes must already have the described preprocessing applied.
    pub fn from_preprocessed(
        path: PathBuf,
        bytes: Arc<[u8]>,
        metadata: FileMetadata,
        preprocessing: PreprocessingInfo,
    ) -> Self;
    pub fn bytes(&self) -> &[u8];
    pub fn fingerprint(&self) -> &FileFingerprint;
}

impl<'a> ChunkSource<'a> {
    pub fn from_preprocessed(
        path: PathBuf,
        chunks: impl IntoIterator<Item = &'a [u8]>,
        metadata: FileMetadata,
        preprocessing: PreprocessingInfo,
    ) -> Self;
}

// SourceFile, FileContents, and ChunkSource implement Source.
// reader/prepare/hash are trait methods.
// FileContents::prepare borrows its bytes, without another copy or transformation.

// Already-preprocessed input; hashing performs no decoding or normalization.
pub async fn hash_source(
    input: impl futures_io::AsyncRead + Unpin,
    cancellation: Canceler<'_>,
) -> io::Result<SourceIdentity>;
```

`SourceFile::read` preprocesses, hashes, and collects a checked capture.
`prepare` retains a checked capture and returns a parser view; an unchanged
prepared capture can be reused by the adapter. An explicit new read/hash may
observe a newer capture. `FileContents` always represents the same snapshot.
`metadata` on a file source observes its open handle; on an in-memory source it
returns the recorded disk metadata. Repeated operations start at byte zero.

Use streaming **XXH3-128**, seed zero and a fixed persisted byte order, plus
processed byte length. Flat, rope, and streamed input must hash identically,
regardless of chunk boundaries. The content-hash format change requires a cache
schema/version change. Grammar/runtime fingerprints are separate.
[XXH3 reference](https://xxhash.com/).

Hash after decoding, BOM handling, and newline normalization; do not add a raw
hash. `load` and `verify` use the completed source fingerprint without rehashing
its stream. Source implementations must report the actual processed bytes and
capture metadata; fingerprints are not proof against a dishonest implementation.
`parse` independently validates prepared input against the miss's identity before
publication, and takes metadata/preprocessing information from that prepared capture.

File adapters check raw length/mtime before and after capture on the same handle,
including raw byte count. Changes return `WouldBlock`, cancellation returns
`Interrupted`; other I/O/decoding failures propagate. These checks detect ordinary
changes, not atomic filesystem snapshots. Async adapters must not perform hidden
blocking file operations on executor threads; backend details stay outside the
standalone decoder crate. The synchronous `TextPreprocessing::apply` remains a
low-level utility for blocking workers, not an asynchronous I/O implementation.

When metadata matches a candidate, `verify(&mut source, ...)` calls `source.hash`.
If preprocessing streams, retain only bounded scratch and the hash, not a rope.
Zed's UTF-8 attempt may fall back: reset the hash and restart the copied buffered
path on the same handle. Buffered BOM/UTF-16/legacy paths remain initially.
Cached encoding information is never used to select or accelerate preprocessing.

On a mismatch, exact lookup reuses the computed identity. A miss may require
`source.prepare().await` to reread the input; parsing checks its identity before
publication. Metadata equality alone never confirms a candidate. `verify_file`
is removed: disk and memory verification both use `Source`.

Check the extraction against Zed's fixtures plus chunk-boundary cases for BOMs,
UTF-8 characters, CRLF, encoding fallback, malformed input, and explicit reload
encodings. Check that streaming and materialized hashing agree, including fallback
after a prefix has already been hashed.

## Lookup and preview

`LoadRequest` replaces the old internal `Request`; it is not caller-constructed
or exported. It owns the common context and moves through the stages without an
extra `Arc`. Encoded keys and headers are derived from it and `SourceIdentity`,
including the grammar fingerprint and current runtime/representation identity.
Cancellation remains per operation and is not retained in the request.

`load` hashes parser input and performs an exact lookup. `CacheMiss` retains its
identity and settings, not source bytes or a callback. `parse` uses the supplied
loader only for scratch and parses the prepared snapshot corresponding to the hashed capture. It validates
the digest before publication; a mismatch is an error, not publication under the
old identity. It never rechecks the cache or waits for another worker. Avoiding
a second hash may later use a separate verified snapshot handle.

`preview` compares the supplied raw mtime and length with a stored metadata hint
for the same path, grammar, and packing variant; preprocessing is not a filter. It returns `None`
when no usable candidate exists. It reads and structurally validates only the
cached tree, using its recorded parser-input length for bounds checks. It does
not read the source file or cached source bytes, and it does not hash source.
The candidate retains its slab ownership independently of later cache changes.
Its private `miss` holds the request and recorded identity for verification;
it does not expose a parse path before verification. On a mismatch, verification
replaces that identity with the computed one before the exact lookup.

Matching metadata is speculative: files can change without changing length or
mtime. Structural analysis may start through `CachedCandidate::tree`, but its
results must remain provisional. Source-dependent predicates require the matching
source snapshot. `verify` hashes the actual input once:

- `Confirmed(tree)` preserves the candidate tree; speculative work can be kept.
- `Changed(load)` rejects the candidate and performs an exact lookup using the
  same computed identity. The result is another cached tree or a `CacheMiss`.
  Neither branch rereads or rehashes the input during verification.

Verification uses the actual capture's raw metadata, which can differ from the
preview metadata. Confirmation depends on parser-input identity, not metadata
equality, and establishes correspondence to that snapshot rather than promising
the disk has not changed since capture. `verify` never parses or publishes.
Any resulting miss retains the actual capture's metadata in its `LoadRequest`.

## Storage and publication

Keep two identities:

| Record | Contents |
|---|---|
| Exact tree key | Path identity, parser-input hash and length, grammar/runtime/representation identity, and semantic packing flags |
| Metadata hint | Path and variant, raw mtime and length, reference to an exact tree generation |
| Capture description | Preprocessing choice and observed outcome for the capture that was published |

Raw mtime/length are hints, not part of exact content identity. Allocation capacity
and repacking preferences do not distinguish cached variants. Source records
store parser-input bytes. Different preprocessing choices may share an exact
tree when they produce identical bytes and other key fields match. `LoadRequest` retains the path, variant settings, and
raw metadata; `SourceIdentity` retains the parser-input hash and length. Tree/source data
and the corresponding hint are published atomically. Publishing an already cached
tree may refresh its hint. Late publication can make a hint stale; verification
still protects correctness. No timestamp ordering is assumed.

Stored preprocessing is descriptive only. The source chooses how to read; cache
lookup, hashing, verification, and parsing never consult cached encoding/BOM/line
ending information. Different choices that produce different bytes mismatch;
identical bytes can reuse the tree. An existing record describes its original
capture, not necessarily the current source's choice or outcome. Do not expose
that record as the current file's encoding merely because verification succeeded.

Every successful, persistence-eligible parse invokes `handle_write` once before
returning the tree. The client publishes, queues, transfers, or discards the owned
work. It retains no transaction or parser borrow. Work is provided even without a
local store so it can be transferred; local `publish` then returns an error.
Dropping work does not publish. Publication errors are handled independently of
the successful parse. All options structs implement `Default` as documented.

The current database also stores source bytes, so `PendingWrite` still needs an
owned parser-input copy. Removing it requires deciding to store only trees and
identities, or supplying source again at publication. `CacheMiss` and `LoadedTree`
retain no source bytes and use no dynamic source dispatch. Text-predicate APIs
also need chunk-aware access to avoid flattening ropes outside parsing.

## Examples

```rust
let mut source = SourceFile::open(root, path).await?;
let loaded = loader.load(&mut source, &grammar, LoadOptions::default()).await?;
let tree = match loaded {
    LoadResult::Loaded(tree) => tree,
    LoadResult::Miss(miss) => {
        let mut input = source.prepare(ReadOptions::default()).await?;
        // Run this synchronous parse on an application worker.
        miss.parse(&mut loader, &mut input, ParseOptions::default(), |write| {
            write_queue.push(write);
        })?
    }
};
```

```rust
let candidate = loader.preview(&mut source, &grammar, LoadOptions::default(),
).await?;
if let Some(candidate) = candidate {
    // Begin provisional structural work using candidate.tree().
    match candidate.verify(&mut source, VerifyOptions::default()).await? {
        Verification::Confirmed(tree) => { /* keep speculative work */ }
        Verification::Changed(loaded) => { /* discard it; handle the new hit/miss */ }
    }
}
```

A write worker calls `write.publish(PublishOptions { cancellation, ..Default::default() })`
and may retry busy work. Passing `drop` as the parse handler explicitly discards
writes. The required callback makes the decision explicit; `#[must_use]` is
advisory and cannot enforce eventual publication. Publication, maintenance, cached
tree decoding, and parsing remain synchronous; source I/O is asynchronous.
