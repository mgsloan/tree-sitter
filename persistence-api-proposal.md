# Cache API proposal

Proposed API, not implemented. Signatures below omit method bodies and unchanged
maintenance and transfer APIs. Storage changes are specified below.

See [client examples](cache-api-examples.md) for usage patterns and remaining gaps.

Rename the exported `tree_sitter_squatter::PackContext` to `TreePacker`, retaining
its `new`, `pack`, `pack_with_options`, and `trim` methods.

One shared project cache, one mutable loader per worker. `load` never parses.
The caller retains an immutable source snapshot and supplies chunks on demand.
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

// Reuse tree_sitter_squatter::PackOptions:
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
    ) -> Result<Grammar, tree_sitter_squatter::Error>;
}

impl Loader {
    // Hashes the supplied parser input, then checks the cache. Never parses.
    pub fn load<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        &self,
        path: &Path,
        file_byte_len: u64,
        mtime: FileModificationTime,
        grammar: &Grammar,
        read: &mut F,
        options: LoadOptions<'_>,
    ) -> Result<LoadResult, LoadError>;

    // Uses supplied metadata only; reads the cached tree without reading source bytes.
    pub fn preview(
        &self,
        path: &Path,
        file_byte_len: u64,
        mtime: FileModificationTime,
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
    path: PathBuf, // normalized project-relative path
    file_byte_len: u64, // raw disk capture, before preprocessing
    mtime: FileModificationTime,
    grammar: Grammar,
    pack: PackOptions,
    preprocessing: TextPreprocessing,
    persistable: bool,
}

// Identity of transformed parser input, separate from raw disk metadata.
struct SourceIdentity {
    hash: u128, // XXH3-128 of parser input after preprocessing
    byte_len: u64,
}

// A structurally validated tree whose correspondence to current source is unverified.
#[must_use]
pub struct CachedCandidate {
    miss: CacheMiss, // contains the candidate's recorded source identity
    tree: LoadedTree,
}

impl CachedCandidate {
    // Enables speculative structural work; no source text is retained.
    pub fn tree(&self) -> &tree_sitter_squatter::Tree;

    // Metadata here describes the actual disk capture, which may differ from preview.
    pub fn verify<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        self,
        file_byte_len: u64,
        mtime: FileModificationTime,
        read: &mut F,
        options: VerifyOptions<'_>,
    ) -> Result<Verification, LoadError>;
}

#[must_use]
pub enum Verification {
    Confirmed(LoadedTree),
    Changed(LoadResult),
}

// Owns source identity, grammar, originating cache, and packing/transform settings.
// Retains no parser borrow, database transaction, or work lock.
#[must_use]
pub struct CacheMiss {
    request: LoadRequest,
    source_identity: SourceIdentity,
}

impl CacheMiss {
    // Parses the supplied snapshot without rechecking the cache or waiting for other work.
    pub fn parse<T: AsRef<[u8]>, F: FnMut(usize, Point) -> T>(
        self,
        loader: &mut Loader,
        read: &mut F,
        options: ParseOptions<'_>,
        handle_write: impl FnOnce(PendingWrite),
    ) -> Result<LoadedTree, LoadError>;
}

#[derive(Clone)]
pub enum LoadedTree {
    // Tree owns its slab allocation; no database transaction is retained.
    Owned(Arc<tree_sitter_squatter::Tree>),
    // Tree reads LMDB bytes held alive by an owning read transaction.
    Backed(Arc<tree_sitter_squatter::BackedTree>),
}

impl LoadedTree {
    pub fn tree(&self) -> &tree_sitter_squatter::Tree;
    pub fn transaction_backed(&self) -> bool;
    // Copies transaction-backed storage; existing aliases keep their snapshots.
    pub fn detach(&self) -> Result<Self, tree_sitter_squatter::Error>;
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
    pub preprocessing: TextPreprocessing,
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
    tree: LoadedTree,
    source: Arc<[u8]>, // retained only for publication under the current source-storing schema
}

impl PendingWrite {
    // Does not consume the work; busy or failed publication can be retried.
    pub fn publish(&self, options: PublishOptions<'_>) -> Result<WriteOutcome, CacheError>;
}

```

## Source and transformation contract

Callbacks use Tree-sitter's exact signature: `&mut F`, where
`F: FnMut(usize, Point) -> T` and `T: AsRef<[u8]>`. They return a chunk starting
at the requested byte offset and row/column position; an empty chunk signals EOF.
Borrowed slices and owned buffers are supported. The callback may cache rope
traversal state, but must expose an immutable snapshot and support arbitrary
reads. Hashing traverses chunks, tracks points, and derives parser-input length.
There is no separate parser-input-length argument.

All callbacks supply **already transformed parser input**. `load`, `verify`, and
`parse` never transform it again. `LoadOptions::preprocessing` identifies the profile
used to produce that input. Every tree byte offset and point uses its coordinates.

`TextPreprocessing::none()` preserves bytes. `zed()` reproduces Zed's initial
file-loading behavior: binary admission checks, encoding detection/decoding,
BOM removal, malformed-input replacement, and CRLF/lone-CR normalization.
`TextPreprocessing::default()` and `LoadOptions::default().preprocessing` use
`zed()`. Callers wanting unchanged bytes explicitly select `none()`.
An existing preprocessed rope needs no conversion or flattening. Output errors
or cancellation invalidate any partial output. Automatic detection can abandon
its streaming attempt and restart with buffered decoding. Helpers with resettable
sinks discard partial output and hashes before restarting. `apply` accepts an
arbitrary writer, so it must buffer uncertain output when it cannot retract it;
it must never append fallback output after an abandoned decoded prefix.

The supplied `file_byte_len` and `mtime` describe the **raw disk capture**, before
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
Do not label explicitly decoded text as `zed()` if automatic detection would
produce different text. Version the preprocessing policy in persisted identities,
including changes to detection and decoder behavior.

`TextPreprocessing` is the working name. Alternatives: `TextProcessing` (shorter,
less specific), `TextPreparation` (describes preparing parser input), or
`TextDecoding` (familiar, but understates newline normalization). `Reencoding`
suggests writing another encoding and does not describe byte-preserving mode well.

## File metadata and hashing utilities

Provide shared utilities for opening a file, reading its raw length and
`FileModificationTime`, preprocessing it, and hashing the resulting parser input.
Use streaming **XXH3-128**, with a fixed seed of zero and canonical persisted byte
order, for `SourceIdentity`; include the processed byte length. Flat, rope, and
streamed input must produce identical identities regardless of chunk boundaries.
The content-hash format change requires a cache schema/version change. Grammar
and runtime fingerprints are separate from this source hashing choice.
[XXH3 reference](https://xxhash.com/).

Hash after all decoding, BOM handling, and newline normalization. Do not add a
raw-byte hash. A hashing sink optionally forwards processed bytes into caller
storage, allowing one preprocessing pass for capture and hashing. Internal
verification consumes this computed identity without hashing it again; public
utility/result names remain to be settled.

Read metadata before and after processing from the same open file handle;
detect changed length/mtime and return a retryable failure. Check bytes read
against the observed raw length. These checks detect ordinary changes, not an
atomic filesystem snapshot. Errors/cancellation discard partial hashes and output.
Keep these synchronous utilities independent of an async runtime; applications
can run them on blocking workers. Filesystem/cache adapters need not live in the
standalone decoder crate.

When `preview` finds matching raw mtime and length:

1. Begin speculative structural work with the candidate tree.
2. If preprocessing can stream for this input, feed processed chunks directly
   into XXH3, retaining only bounded scratch, not the full source or a rope.
3. Confirm using the processed hash/length and matching preprocessing policy.
   A mismatch performs the same exact lookup as `verify`, reusing that identity.
4. If parsing is needed, capture source and validate it against the miss before
   publication. A discarded streaming input requires rereading for this step.

Streaming eligibility can depend on the input: `none()` always streams; Zed's
ordinary BOM-less UTF-8 path streams but may fall back after invalid UTF-8 or an
escape character. Reset its hasher on fallback and use the copied buffered path.
Initially preserve Zed's buffered BOM/UTF-16/legacy-encoding behavior. Extending
those paths to stream is separate from the verbatim extraction. Metadata equality
alone never confirms the candidate. This helper is an alternative to the chunk
callback `verify`, whose caller already has a replayable source snapshot.

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
loader only for scratch and parses the same caller-owned snapshot. It validates
the digest before publication; a mismatch is an error, not publication under the
old identity. It never rechecks the cache or waits for another worker. Avoiding
a second hash may later use a separate verified snapshot handle.

`preview` compares the supplied raw mtime and length with a stored metadata hint
for the same path, grammar, preprocessing, and packing variant. It returns `None`
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
| Exact tree key | Path identity, parser-input hash and length, grammar/runtime/representation identity, versioned transform profile, and semantic packing flags |
| Metadata hint | Path and variant, raw mtime and length, reference to an exact tree generation |

Raw mtime/length are hints, not part of exact content identity. Allocation capacity
and repacking preferences do not distinguish cached variants. Source records
store parser-input bytes; raw and transformed sources must not share the old
raw-byte record identity. `LoadRequest` retains the path, variant settings, and
raw metadata; `SourceIdentity` retains the parser-input hash and length. Tree/source data
and the corresponding hint are published atomically. Publishing an already cached
tree may refresh its hint. Late publication can make a hint stale; verification
still protects correctness. No timestamp ordering is assumed.

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

Apply preprocessing while reading disk, into caller-owned bytes (a rope writer can
be used instead). Obtain raw metadata from the file capture, not the output:

```rust
let preprocessing = TextPreprocessing::zed();
let mut bytes = Vec::new();
preprocessing.apply(&mut file, &mut bytes, PreprocessingOptions::default())?;
let mut read = |offset: usize, _: Point| bytes.get(offset..).unwrap_or_default();
let options = LoadOptions { preprocessing, ..Default::default() };
let tree = match loader.load(path, file_byte_len, mtime, &grammar, &mut read, options)? {
    LoadResult::Loaded(tree) => tree,
    LoadResult::Miss(miss) => miss.parse(
        &mut loader,
        &mut read,
        ParseOptions::default(),
        |write| write_queue.push(write),
    )?,
};
```

Start speculative work before the editor loads its rope snapshot:

```rust
let candidate = loader.preview(
    path,
    observed_file_byte_len,
    observed_mtime,
    &grammar,
    LoadOptions { preprocessing, ..Default::default() },
)?;
// If present, begin provisional structural work using candidate.tree().
// The caller schedules source loading concurrently and retains its snapshot.
```

Once that snapshot is available, `read` is its Tree-sitter-compatible callback:

```rust
let loaded = match candidate {
    Some(candidate) => match candidate.verify(
        captured_file_byte_len,
        captured_mtime,
        &mut read,
        VerifyOptions::default(),
    )? {
        Verification::Confirmed(tree) => {
            // Keep work derived from this candidate.
            LoadResult::Loaded(tree)
        }
        Verification::Changed(loaded) => {
            // Discard work derived from the candidate.
            loaded
        }
    },
    None => loader.load(
        path,
        captured_file_byte_len,
        captured_mtime,
        &grammar,
        &mut read,
        LoadOptions { preprocessing, ..Default::default() },
    )?,
};
// A Miss can be parsed as in the first example, using the same snapshot.
```

A write worker calls `write.publish(PublishOptions { cancellation })` and may
retry busy work. Passing `drop` as the parse handler explicitly discards writes.
The required callback makes the decision explicit; `#[must_use]` is advisory
and cannot enforce eventual publication.
