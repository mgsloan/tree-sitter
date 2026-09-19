# Persistence API design

The public API has three namespaces: `tree_squatter_persistence` for caching,
`source` for capturing input, and `text` for preprocessing. The root contains the
normal load/parse/apply workflow; applications implementing their own input
adapter mainly work in `source`. Storage, keys, transactions, and lookup state
remain private.

This organizes the [proposal](persistence-api-proposal.md) and its
[client examples](cache-api-examples.md) for a rewrite. Signatures omit bodies;
private fields show the planned ownership and representation. `Store` and
`FileBackend` mark backend boundaries: the former owns the database environment
and handles, the latter an open file and its I/O scheduling. Their internals,
grammar-fingerprint derivation, and complete maintenance and transfer contracts
remain open. Maintenance and transfer can extend `Cache` and `CacheWrite` without
new public modules. This design supersedes the proposal's workflow signatures and
request ownership; the proposal and examples retain their original API.

Detailed storage and decoder-extraction requirements remain in the proposal.
The accompanying packing change is renaming `tree_sitter_squatter::PackContext`
to `TreePacker`, retaining `new`, `pack`, `pack_with_options`, and `trim`.
`PackOptions` stays in that crate with its existing defaults: zero initial
capacity, no repacking, symbol presence enabled, and points enabled.

## `tree_squatter_persistence` — cache workflow

Share one `Arc<Cache>` per project and keep one movable `Loader` per worker. The
cache is `Send + Sync` through its fields; the loader owns a parser and lazy
packing scratch. Misses, unchecked trees, and writes can move between workers
without a parser borrow or work lock. Unchecked trees and misses retain the
grammar, semantic packing flags, and source identity. They store no path, raw
metadata, or destination cache. `Loader::check_hash` uses its cache for
fallback lookup; `parse` uses its loader for scratch and the write destination.
Only `CacheWrite` binds a destination cache, retaining it for deferred application.

`load` hashes the source and looks up that exact input; it never parses. On a
miss, prepare an immutable input asynchronously, then parse synchronously on a
worker. Parsing independently checks the prepared bytes against the miss's hash
and length. A mismatch fails without a write callback. It neither repeats lookup
nor waits for another worker. Every successful parse returns an owned tree and
calls the required handler once with publication work, even without local storage.
That work owns the prepared input's path, metadata, preprocessing description,
and processed source bytes, plus the tree and its cache identity. Dropping it
discards the write. Synchronous `CacheWrite::apply` borrows the work, allowing
retry after `Busy` or failure without invalidating the parsed tree. Cancellation
after commit cannot undo publication; cancellation settings are never retained
in owned work.

`unchecked_load` matches raw metadata and structurally validates a cached tree
without reading source bytes. `check_hash` uses the source's current preprocessing
and hashes once: matching content returns the tree; changed content performs an
exact lookup with that hash, the retained grammar and features, and `source.path()`.
It never parses or publishes. The result applies to the captured snapshot;
a resulting miss retains only its source identity, grammar, and features.
`LoadResult::Loaded` may contain the original tree or another exact hit; it does
not indicate whether speculative work can be reused. Text-dependent queries need
the matching source snapshot.

An `UncheckedTree` owns a `BackedTree`, retaining its read transaction during
speculation and hash checking. A matching hash moves that backing into
`LoadedTree::Backed`; callers can detach it afterward. A matching tree retains its
original backing even if a different loader calls `check_hash`. That loader's
`ReadPolicy` controls fallback exact lookup. Unchecked loading
always uses backed storage and returns `None` when no usable backed tree is
available, even if an exact load could copy the entry.

Exact identity comprises path, processed hash and length, grammar/runtime/
representation identity, and the `points` and `symbol_presence` packing flags.
`LoadOptions::features` selects those flags through `TreeFeatures`, which defaults
to both enabled. They remain fixed through hash checking and parsing.
`ParseOptions` supplies initial group capacity
and repacking preferences, defaulting to zero and false. Parsing combines these
with the retained features to construct `PackOptions`; stored trees are compact.
Raw metadata is a hint, and preprocessing records are descriptive. Publication
atomically stores tree bytes, source bytes, and the hint. Refreshing a hint for an
existing tree is allowed; delayed writes can leave stale hints.

`CacheOptions` defaults to a 256 MiB map ceiling, owned reads, and creating missing
caches. With `create_cache_if_absent: false`, a missing cache yields a storeless
handle. A cache opened without storage stays that way: lookup misses,
`unchecked_load` returns `None`, parsing works, and local publication errors.
Repeated opens reuse the live `Arc<Cache>` for the same root. This design resolves
conflicting options by rejecting them with `io::ErrorKind::InvalidInput`; reuse
never upgrades a storeless handle. Roots are not canonicalized by the library.

Owned trees retain their slab and grammar; backed trees also retain an owning read
transaction. Preferred backed reads may fall back to owned storage. `detach`
copies backed storage without changing existing aliases. Error design must let
callers distinguish cancellation, changed prepared input, unavailable storage,
and malformed or incompatible cache data.

```rust
use std::{io, path::{Path, PathBuf}, sync::Arc};
use tree_sitter_squatter::{BackedTree, Tree, TreePacker};

use source::{FileMetadata, ParserInput, Source, SourceIdentity};
use store::Store;
use text::PreprocessingInfo;

mod store;
pub mod source;
pub use tree_squatter_text as text;
pub use text::Canceler;

pub struct Cache {
    root: PathBuf,
    store: Option<Box<Store>>,
    options: CacheOptions,
}

pub struct Loader {
    cache: Arc<Cache>,
    parser: tree_sitter::Parser,
    packer: Option<TreePacker>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TreeFeatures {
    pub symbol_presence: bool,
    pub points: bool,
}
impl Default for TreeFeatures { /* both flags enabled */ }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheOptions {
    pub map_size: usize,
    pub read: ReadPolicy,
    pub create_cache_if_absent: bool,
}
impl Default for CacheOptions { /* defaults described above */ }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadPolicy {
    #[default]
    Owned,
    PreferTransactionBacked,
}

// Provider-supplied identity must cover all behavior-affecting grammar inputs.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GrammarFingerprint {
    bytes: Arc<[u8]>,
}

impl GrammarFingerprint {
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Self;
}

#[derive(Clone)]
pub struct Grammar {
    language: tree_sitter::Language,
    packed: tree_sitter_squatter::Grammar,
    fingerprint: GrammarFingerprint,
}

impl Cache {
    pub fn open(root: impl AsRef<Path>, options: CacheOptions) -> io::Result<Arc<Self>>;
    pub fn loader(self: &Arc<Self>) -> Loader;
    pub fn grammar(
        &self,
        language: &tree_sitter::Language,
        fingerprint: GrammarFingerprint,
    ) -> Result<Grammar, tree_sitter_squatter::Error>;
}

impl Loader {
    pub async fn load<S: Source>(
        &self,
        source: &mut S,
        grammar: &Grammar,
        options: LoadOptions<'_>,
    ) -> Result<LoadResult, LoadError>;

    pub async fn unchecked_load<S: Source>(
        &self,
        source: &mut S,
        grammar: &Grammar,
        options: LoadOptions<'_>,
    ) -> Result<Option<UncheckedTree>, LoadError>;

    pub async fn check_hash<S: Source>(
        &self,
        source: &mut S,
        tree: UncheckedTree,
        canceler: Canceler<'_>,
    ) -> Result<LoadResult, LoadError>;

    pub fn drop_packer(&mut self);
}

#[derive(Default)]
pub struct LoadOptions<'a> {
    pub features: TreeFeatures,
    pub canceler: Canceler<'a>,
}

#[must_use]
pub enum LoadResult {
    Loaded(LoadedTree),
    Miss(CacheMiss),
}

#[must_use]
pub struct UncheckedTree {
    grammar: Grammar,
    features: TreeFeatures,
    source_identity: SourceIdentity,
    tree: BackedTree,
}

impl UncheckedTree {
    pub fn tree(&self) -> &Tree;
}

#[must_use]
pub struct CacheMiss {
    grammar: Grammar,
    features: TreeFeatures,
    source_identity: SourceIdentity,
}

impl CacheMiss {
    pub fn parse<I: ParserInput>(
        self,
        loader: &mut Loader,
        input: &mut I,
        options: ParseOptions<'_>,
        handle_write: impl FnOnce(CacheWrite),
    ) -> Result<LoadedTree, LoadError>;
}

#[derive(Default)]
pub struct ParseOptions<'a> {
    pub initial_group_capacity: u32,
    pub repack: bool,
    pub canceler: Canceler<'a>,
}

#[derive(Clone)]
pub enum LoadedTree {
    Owned(Arc<Tree>),
    Backed(Arc<BackedTree>),
}

impl LoadedTree {
    pub fn tree(&self) -> &Tree;
    pub fn transaction_backed(&self) -> bool;
    pub fn detach(&self) -> Result<Self, tree_sitter_squatter::Error>;
}

#[must_use = "apply, transfer, queue, or explicitly discard this write"]
pub struct CacheWrite {
    cache: Arc<Cache>,
    path: PathBuf,
    metadata: FileMetadata,
    grammar_fingerprint: GrammarFingerprint,
    features: TreeFeatures,
    source_identity: SourceIdentity,
    preprocessing: PreprocessingInfo,
    tree: Arc<Tree>,
    source: Arc<[u8]>,
}

impl CacheWrite {
    pub fn apply(&self, options: ApplyOptions<'_>) -> Result<WriteOutcome, CacheError>;
}

#[derive(Default)]
pub struct ApplyOptions<'a> {
    pub canceler: Canceler<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    Published,
    AlreadyPresent,
    Busy,
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Cache(CacheError),
    Language(tree_sitter::LanguageError),
    Packing(tree_sitter_squatter::Error),
    Cancelled,
    SourceChanged { expected: SourceIdentity, actual: SourceIdentity },
    ParseFailed,
}

#[derive(Debug)]
pub enum CacheError {
    Io(io::Error),
    Storage(Box<dyn std::error::Error + Send + Sync>),
    Unavailable,
    Cancelled,
    InvalidData,
    Incompatible,
}

// Both errors implement Display and std::error::Error, preserving underlying causes.
```

`load` and `check_hash` share `LoadResult`; the outer `Result<_, LoadError>`
distinguishes an operational failure from a normal cache miss.
An unchecked miss has no processed source identity, so it cannot construct the
parse-ready `CacheMiss` shown above. Keep `Option<UncheckedTree>` unless that path
needs a separate continuation for hashing or preparing the source.

## `source` — input capture

`Source` owns the logical path, preprocessing policy, and access to a capture.
Its asynchronous operations yield already processed bytes; the cache never
decodes them again. `ParserInput` is the synchronous view used after preparation:
reads can revisit offsets, must perform no I/O, and return an empty chunk only at
EOF. Mutable cursors are allowed; text is immutable. Borrowed chunks refer to the
snapshot, not scratch overwritten by the next read. Tree offsets and points use
processed coordinates.

This design requires `Send` sources, readers, prepared inputs, and operation
futures, expressed without boxing or choosing an executor. Move an owned snapshot
to a worker and prepare there, or use scoped work for a prepared borrow.
`SourceFile` delegates blocking I/O and decoding to its backend; backend selection
remains an integration decision.

Paths are plain native `Path` values. Clients choose a consistent root and
relative spelling and supply that same identity for source and prepared input
throughout loading, hash checking, and parsing. Lookup takes the path from
`Source`; publication takes it from `ParserInput`. Unchecked trees and misses
do not retain a path to check against later inputs.
The library does not validate, normalize, canonicalize, or compare those paths.
`SourceFile::open(root, path)` opens `root.join(path)` using ordinary filesystem
semantics and preserves `path` as its identity, including symlink components.
The path selects a cache record, not an output filename or a containment boundary.

File captures use one open handle, checking raw length, raw byte count, and mtime
before and after reading. Mtimes support equality, never ordering or arithmetic.
Changes return `WouldBlock`, cancellation returns `Interrupted`, and failed
captures produce no usable result. These checks do not promise atomic filesystem
snapshots. Supplied memory snapshots must represent the described disk capture;
arbitrary edited buffers cannot claim its metadata.

`hash` returns the fingerprint of a checked capture without retaining text where
preprocessing streams. `reader` starts at processed byte zero and must produce the
same identity for the same capture. Detection fallback resets the speculative
decoder and hash; it cannot append replacement output to an abandoned prefix.
`prepare` completes all fallible reading before returning an immutable parser
view. A new read or hash may capture newer contents; prepared bytes stay fixed.

Hashing uses streaming XXH3-128, seed zero, and processed byte length, independent
of chunk boundaries. `FileContents` computes and retains its fingerprint;
`ChunkSource` indexes borrowed chunks without flattening them. Publication uses
the metadata and preprocessing description from the validated prepared input.
Empty chunks are omitted from the index so they cannot look like premature EOF;
`chunk_offsets` contains each chunk's start and a final total-length boundary.
Changing a file's preprocessing policy invalidates its prepared capture.

```rust
use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};
use futures_io::AsyncRead;
use tree_sitter::Point;

use crate::{Canceler, text::{PreprocessingInfo, TextPreprocessing}};

pub trait Source: Send {
    type Reader<'a>: AsyncRead + Unpin + Send where Self: 'a;
    type Input<'a>: ParserInput + Send where Self: 'a;

    fn path(&self) -> &Path;
    fn preprocessing(&self) -> TextPreprocessing;

    fn metadata(&mut self)
        -> impl Future<Output = io::Result<FileMetadata>> + Send;
    fn reader(&mut self, options: ReadOptions<'_>)
        -> impl Future<Output = io::Result<Self::Reader<'_>>> + Send;
    fn prepare(&mut self, options: ReadOptions<'_>)
        -> impl Future<Output = io::Result<Self::Input<'_>>> + Send;
    fn hash(&mut self, options: ReadOptions<'_>)
        -> impl Future<Output = io::Result<FileFingerprint>> + Send;
}

pub trait ParserInput {
    type Chunk: AsRef<[u8]>;

    fn path(&self) -> &Path;
    fn metadata(&self) -> FileMetadata;
    fn preprocessing_info(&self) -> &PreprocessingInfo;
    fn read(&mut self, byte_offset: usize, position: Point) -> Self::Chunk;
}

#[derive(Default)]
pub struct ReadOptions<'a> {
    pub canceler: Canceler<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileModificationTime(SystemTime);

impl FileModificationTime {
    pub fn new(time: SystemTime) -> Self;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    pub byte_len: u64,
    pub mtime: FileModificationTime,
}

impl FileMetadata {
    pub fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceIdentity {
    pub hash: u128,
    pub byte_len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFingerprint {
    pub metadata: FileMetadata,
    pub preprocessing: PreprocessingInfo,
    pub source: SourceIdentity,
}

pub struct SourceFile {
    path: PathBuf,
    file: FileBackend,
    preprocessing: TextPreprocessing,
    prepared: Option<FileContents>,
}

pub struct FileContents {
    path: PathBuf,
    bytes: Arc<[u8]>,
    fingerprint: FileFingerprint,
}

pub struct ChunkSource<'a> {
    path: PathBuf,
    chunks: Vec<&'a [u8]>,
    chunk_offsets: Vec<usize>,
    fingerprint: FileFingerprint,
}

impl SourceFile {
    pub async fn open(root: impl AsRef<Path>, path: impl AsRef<Path>) -> io::Result<Self>;
    pub fn with_preprocessing(self, preprocessing: TextPreprocessing) -> Self;
    pub async fn read(&mut self, options: ReadOptions<'_>) -> io::Result<FileContents>;
}

impl FileContents {
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

// Each adapter implements Source; concrete reader/input types are omitted here.
// FileContents::prepare borrows its existing bytes without copying or preprocessing.

pub async fn hash_source(
    input: impl AsyncRead + Unpin,
    canceler: Canceler<'_>,
) -> io::Result<SourceIdentity>;
```

## `text` — preprocessing

Implement this namespace in a standalone `tree_squatter_text` crate and re-export
it as `tree_squatter_persistence::text`. It depends on neither persistence nor Zed
crates and owns the shared cancellation type, avoiding a dependency cycle.
Persistence re-exports `Canceler` at its root for normal operation options.

`TextPreprocessing` is an opaque profile with two constructors. `none()` preserves
bytes. The default, `zed()`, performs Zed's initial-load binary admission,
encoding detection and decoding, BOM removal, malformed-input replacement, and
CRLF/lone-CR normalization. Disk sources use this default. Already processed
snapshots carry a description without applying the profile again.

Extract Zed's decoding functions and tests verbatim at the proposal's pinned
revision, preserving attribution and licenses, with isolated storage/scheduling
adapters. The standalone crate also needs explicit encoding selection, reload
using the current encoding, BOM overrides, and forced non-Unicode decoding without
BOM handling. Specify those lower level entry points during extraction; the API
below is the cache-facing surface.

`PreprocessingInfo` describes a capture without selecting later decoding or tree
identity. Encoding names are canonical; requested encoding records explicit
selection, effective encoding is absent for unchanged bytes, and line endings
describe input before normalization. A custom source records explicit decoder
choices here; a reused tree's stored record need not describe the current capture.

`apply` synchronously writes into caller-owned storage, buffering uncertain output
when fallback cannot retract it. Discard partial output on error or cancellation.
Adapters needing the capture description use the standalone decoder's reporting
API. `Canceler::default()` cannot cancel: both queries return false and `cancel`
does nothing. A supplied flag uses relaxed atomic loads and stores.

```rust
use std::{io, sync::atomic::AtomicBool};

#[derive(Clone, Copy, Default)]
pub struct Canceler<'a>(Option<&'a AtomicBool>);

impl<'a> Canceler<'a> {
    pub fn new(flag: &'a AtomicBool) -> Self;
    pub fn can_cancel(&self) -> bool;
    pub fn is_cancelled(&self) -> bool;
    pub fn cancel(&self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextPreprocessing {
    mode: PreprocessingMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PreprocessingMode {
    None,
    Zed,
}

impl Default for TextPreprocessing { /* zed() */ }

impl TextPreprocessing {
    pub fn none() -> Self;
    pub fn zed() -> Self;
    pub fn apply(
        &self,
        input: impl io::Read,
        output: impl io::Write,
        options: PreprocessingOptions<'_>,
    ) -> io::Result<()>;
}

#[derive(Default)]
pub struct PreprocessingOptions<'a> {
    pub canceler: Canceler<'a>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreprocessingInfo {
    pub mode: TextPreprocessing,
    pub requested_encoding: Option<String>,
    pub encoding: Option<String>,
    pub had_bom: bool,
    pub line_ending: Option<LineEnding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}
```
