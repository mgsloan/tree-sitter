# Cache API proposal

Proposed API, not implemented. Signatures below omit method bodies and unchanged
maintenance, transfer, and storage APIs.

Rename the exported `tree_sitter_squatter::PackContext` to `TreePacker`, retaining
its `new`, `pack`, `pack_with_options`, and `trim` methods.

One shared project cache, one mutable loader per worker. `load` never
parses; a miss retains the exact source bytes for later parsing.
Each loader owns an `Arc<Cache>` and can move independently between threads.
`Cache` is `Send + Sync` through its fields, but does not implement `Clone`.
Clone the `Arc` to share it; no manual unsafe trait implementations are needed.

The directory registry shares `Arc<Cache>` instead of `Arc<Store>`.
Pending work, maintenance, and snapshot admission owners
retain `Arc<Cache>`. The policy for repeated opens with conflicting
`CacheOptions` remains to be decided.

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
    // Reads and hashes disk bytes, then checks the cache. Never waits for a parser.
    pub fn load(
        &self,
        path: &Path,
        grammar: &Grammar,
        options: LoadOptions<'_>,
    ) -> Result<LoadResult, LoadError>;

    // The next parse that needs packing recreates the packer lazily.
    pub fn drop_packer(&mut self) {
        self.packer = None;
    }
}

#[must_use]
pub enum LoadResult {
    Loaded(LoadedFile),
    Miss(CacheMiss),
}

// Owns captured bytes, grammar, originating cache, and packing settings.
// Retains no parser borrow, database transaction, or work lock.
#[must_use]
pub struct CacheMiss {
    cache: Arc<Cache>,
    request: Arc<Request>,
    source: Arc<[u8]>,
    grammar: Grammar,
    pack: PackOptions,
    persistable: bool,
}

impl CacheMiss {
    // Parses the captured source without rechecking the cache or waiting for other work.
    pub fn parse(
        self,
        loader: &mut Loader,
        options: ParseOptions<'_>,
        handle_write: impl FnOnce(PendingWrite),
    ) -> Result<LoadedFile, LoadError>;
}

#[derive(Clone)]
pub struct LoadedFile {
    source: Arc<[u8]>,
    tree: LoadedTree,
}

#[derive(Clone)]
enum LoadedTree {
    // Tree owns its slab allocation; no database transaction is retained.
    Owned(Arc<tree_sitter_squatter::Tree>),
    // Tree reads LMDB bytes held alive by an owning read transaction.
    Backed(Arc<tree_sitter_squatter::BackedTree>),
}

impl LoadedFile {
    pub fn source(&self) -> &[u8];
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

#[derive(Default)]
pub struct LoadOptions<'a> {
    pub pack: PackOptions,
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
    cache: Arc<Cache>,
    request: Arc<Request>,
    grammar: Grammar,
    file: LoadedFile,
}

impl PendingWrite {
    // Does not consume the work; busy or failed publication can be retried.
    pub fn publish(&self, options: PublishOptions<'_>) -> Result<WriteOutcome, CacheError>;
}

```

`CacheMiss` uses the supplied loader only for scratch; its originating cache
and captured settings remain authoritative. `load` never parses. `parse` always
parses the captured bytes unless cancelled or an error occurs; it does not reread
the source, recheck the cache, or coordinate with another worker. Waiting for
another worker's publication is deferred to a later version. All options structs
implement `Default` as documented above.

Every successful, persistence-eligible parse invokes `handle_write` once before
returning the file. Cache hits, failed loads, and ineligible files do not invoke
it. The callback receives an owned `PendingWrite` with all captured publication
data and no open transaction or parser borrow. Work is supplied even without an
open local store, allowing transfer to another process; local `publish` then
returns an error. Callbacks are not retained after `parse` returns.

The client chooses whether to publish immediately, enqueue, transfer, or discard.
The library never publishes automatically. Publication errors and cancellation
are handled by the callback independently of the successful load. Dropping work
performs no publication.

```rust
let cache = Cache::open(root, CacheOptions::default())?;
let mut loader = cache.loader();
let file = match loader.load(path, &grammar, LoadOptions::default())? {
    LoadResult::Loaded(file) => file,
    LoadResult::Miss(miss) => miss.parse(
        &mut loader,
        ParseOptions::default(),
        |write| write_queue.push(write),
    )?,
};

// On the write worker, given an owned PendingWrite and cancellation flag:
// let outcome = write.publish(PublishOptions { cancellation: Canceler::new(&cancelled) })?;
// Busy work can be retried using the same PendingWrite.

// To discard writes, pass `drop` as the handler.
// To publish inline, call write.publish(PublishOptions::default()) in the handler
// and handle its outcome there.
```

The required callback makes write handling explicit at each parsing entry point.
`#[must_use]` on `PendingWrite` is advisory; Rust does not enforce eventual
publication inside the callback.
