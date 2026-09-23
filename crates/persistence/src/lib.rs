//! Exact-disk-byte source/tree loading with atomic LMDB persistence.
//!
//! Owned cache hits are the default; transaction-backed hits are opt-in.
//! Cache errors fall back to parsing;
//! optional deferred writes never hold a database transaction while queued.
//! See the crate README for implemented scope and remaining design milestones.

mod identity;
mod maintenance;
mod snapshot;
mod store;
mod transfer;
mod work;
pub use identity::{Grammar, GrammarFingerprint};
pub use maintenance::{
    EvictionOutcome, Maintenance, MaintenanceProgress, MaintenanceState, MissingSweep, SidecarKind,
};
pub use store::{CacheError, WriteOutcome};

use identity::Request;
use std::{
    fs::OpenOptions,
    io::{self, Read},
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use store::Store;

/// Cache directory path for the target's native byte order.
pub const CACHE_DIRECTORY: &str = if cfg!(target_endian = "big") {
    ".tree-sitter/big-endian"
} else {
    ".tree-sitter"
};

#[derive(Clone, Debug)]
pub struct Options {
    /// Fixed LMDB map ceiling, a multiple of the system page size. Full maps cause
    /// write fallback, never forced resize. Invalid options disable caching.
    pub map_size: usize,
    pub symbol_presence: bool,
    /// Store source row/column positions. Point-free trees expose byte offsets
    /// as columns on row zero.
    pub points: bool,
    /// Maximum cooperative wait in synchronous loads; zero allows duplicate work immediately.
    pub cooperation_wait: std::time::Duration,
    pub read: ReadPolicy,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            map_size: 256 * 1024 * 1024,
            symbol_presence: true,
            points: true,
            cooperation_wait: std::time::Duration::from_millis(50),
            read: ReadPolicy::Owned,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadPolicy {
    #[default]
    Owned,
    /// Prefer a pinned LMDB snapshot. Misalignment or the per-environment local
    /// limit of 32 owners falls back to a copy. Live snapshots delay page reuse.
    PreferTransactionBacked,
}

#[derive(Clone)]
enum LoadedTree {
    Owned(Arc<tree_sitter_squatter::Tree>),
    Backed(Arc<tree_sitter_squatter::BackedTree>),
}
impl LoadedTree {
    fn tree(&self) -> &tree_sitter_squatter::Tree {
        match self {
            Self::Owned(tree) => tree,
            Self::Backed(tree) => tree,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WritePolicy {
    #[default]
    Inline,
    Deferred,
    /// Return transferable publication work even when no cache is open. The
    /// caller hands it to another process; this policy never publishes inline.
    Transfer,
    Disabled,
}

pub struct LoadOptions<'a> {
    pub pack: tree_sitter_squatter::PackOptions,
    pub write: WritePolicy,
    pub cancellation: Option<&'a AtomicBool>,
}
impl Default for LoadOptions<'_> {
    fn default() -> Self {
        Self {
            pack: tree_sitter_squatter::PackOptions::default(),
            write: WritePolicy::default(),
            cancellation: None,
        }
    }
}
impl LoadOptions<'_> {
    fn cancelled(&self) -> bool {
        self.cancellation
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }
    fn check(&self) -> Result<(), LoadError> {
        if self.cancelled() {
            Err(LoadError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub enum LoadError {
    InvalidPath,
    Source(io::Error),
    TooLarge,
    Language(tree_sitter::LanguageError),
    Cancelled,
    ParseFailed,
    Pack(tree_sitter_squatter::Error),
}
impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "load failed: {self:?}")
    }
}
impl std::error::Error for LoadError {}
impl From<io::Error> for LoadError {
    fn from(error: io::Error) -> Self {
        Self::Source(error)
    }
}

#[derive(Clone)]
pub struct LoadedFile {
    source: Arc<[u8]>,
    tree: LoadedTree,
    hit: bool,
    cleanup: Option<(Arc<Store>, Arc<Request>)>,
}
impl LoadedFile {
    pub fn source(&self) -> &[u8] {
        &self.source
    }
    pub fn tree(&self) -> &tree_sitter_squatter::Tree {
        self.tree.tree()
    }
    pub fn transaction_backed(&self) -> bool {
        matches!(self.tree, LoadedTree::Backed(_))
    }
    /// Return an owned copy. Existing aliases keep their snapshots until dropped.
    /// Auxiliary semantics are not revalidated while detaching.
    pub fn detach(&self) -> Result<Self, tree_sitter_squatter::Error> {
        let LoadedTree::Backed(tree) = &self.tree else {
            return Ok(self.clone());
        };
        let tree = tree.detach()?;
        Ok(Self {
            tree: LoadedTree::Owned(Arc::new(tree)),
            ..self.clone()
        })
    }
    pub fn cache_hit(&self) -> bool {
        self.hit
    }
    /// Delete one persisted sidecar for this tree, preserving the core and other sidecar.
    /// Existing readers retain their data; later loads or publishers can rebuild it.
    pub fn evict_sidecar(
        &self,
        kind: SidecarKind,
        cancellation: Option<&AtomicBool>,
    ) -> Result<EvictionOutcome, CacheError> {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(CacheError::Cancelled);
        }
        match &self.cleanup {
            Some((store, request)) => store.evict_sidecar(request, kind, cancellation),
            None => Ok(EvictionOutcome::Absent),
        }
    }

    /// Retire older cached generations independently of loading/publication.
    /// The work stops if another publication supersedes this generation.
    pub fn maintenance(&self) -> Option<Maintenance> {
        self.cleanup
            .as_ref()
            .map(|(store, request)| Maintenance::keep(store.clone(), request))
    }
}

/// Executor-neutral load result. A deferred request owns its captured source and
/// retains no database transaction, parser borrow, or work lock.
pub enum LoadStep {
    Ready(LoadResult),
    Deferred(PendingLoad),
}

pub struct PendingLoad {
    request: Arc<Request>,
    source: Arc<[u8]>,
    grammar: Grammar,
    store: Option<Arc<Store>>,
    pack: tree_sitter_squatter::PackOptions,
    write: WritePolicy,
    read: ReadPolicy,
    persistable: bool,
}

pub struct LoadResult {
    pub file: LoadedFile,
    pub pending_write: Option<PendingWrite>,
}

/// Per-worker parser and lazy packing scratch. Reuses grammar-derived tables
/// and scratch across grammar changes; cache hits do not allocate a packing context.
pub struct LoadContext {
    parser: tree_sitter::Parser,
    packing: Option<tree_sitter_squatter::PackContext>,
}
impl Default for LoadContext {
    fn default() -> Self {
        Self {
            parser: tree_sitter::Parser::new(),
            packing: None,
        }
    }
}

impl LoadContext {
    /// Release packing scratch while keeping the parser and prepared grammar.
    pub fn trim(&mut self) {
        if let Some(packing) = &mut self.packing {
            packing.trim();
        }
    }
}

/// Complete, immutable work. No LMDB transaction is retained until execution.
pub struct PendingWrite {
    store: Option<Arc<Store>>,
    request: Arc<Request>,
    grammar: Grammar,
    file: LoadedFile,
}
impl PendingWrite {
    /// Optional generation cleanup to execute after successful publication.
    pub fn maintenance(&self) -> Option<Maintenance> {
        self.file.maintenance()
    }
    /// Busy work can be retried later using this same item.
    pub fn publish(&self) -> Result<WriteOutcome, CacheError> {
        self.publish_with_cancellation(&AtomicBool::new(false))
    }
    pub fn publish_with_cancellation(
        &self,
        cancellation: &AtomicBool,
    ) -> Result<WriteOutcome, CacheError> {
        let store = self.store.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "publication requires an open cache",
            )
        })?;
        store.publish(
            &self.request,
            self.file.source(),
            self.file.tree(),
            &self.grammar,
            || cancellation.load(Ordering::Relaxed),
        )
    }
}

pub struct Persistence {
    root: PathBuf,
    store: Option<Arc<Store>>,
    options: Options,
}
impl Persistence {
    /// Prepare shared tables, restoring the expensive dictionary directly from
    /// an LMDB read transaction when available. No transaction is retained.
    pub fn prepare_grammar(
        &self,
        language: &tree_sitter::Language,
        fingerprint: GrammarFingerprint,
    ) -> Result<Grammar, tree_sitter_squatter::Error> {
        let prepared = match self
            .store
            .as_ref()
            .and_then(|store| store.prepare_grammar(language, fingerprint))
        {
            Some(prepared) => prepared,
            None => tree_sitter_squatter::Grammar::new(language)?,
        };
        Ok(Grammar::new(prepared, fingerprint))
    }

    pub fn sweep_missing(&self) -> Option<MissingSweep> {
        self.store
            .as_ref()
            .map(|store| MissingSweep::new(store.clone(), self.root.clone()))
    }

    /// Reclaim LMDB reader slots abandoned by crashed processes, never live readers.
    pub fn check_stale_readers(&self) -> Result<usize, CacheError> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        Ok(store.env.clear_stale_readers()?)
    }
    /// Construct optional cleanup for a deleted source. Every batch rechecks
    /// absence; a recreated file or newer publication stops the task.
    pub fn maintenance_missing(&self, path: &Path) -> Result<Option<Maintenance>, CacheError> {
        let (path, encoded) = identity::path(path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        match &self.store {
            Some(store) => Maintenance::missing(store.clone(), encoded, self.root.join(path)),
            None => Ok(None),
        }
    }
    /// Cache opening failures are nonfatal. Root resolution must succeed.
    /// The cache directory must be trusted and accessed by cooperating writers.
    pub fn open(root: impl AsRef<Path>, options: Options) -> io::Result<Self> {
        Self::open_impl(root.as_ref(), options, true)
    }

    /// Open an existing cache without creating directories, files, or schema.
    /// A missing cache still supports parsing and `WritePolicy::Transfer`.
    pub fn open_existing(root: impl AsRef<Path>, options: Options) -> io::Result<Self> {
        Self::open_impl(root.as_ref(), options, false)
    }

    fn open_impl(root: &Path, options: Options, create: bool) -> io::Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "project root"));
        }
        let store = if create {
            Store::open(&root, options.map_size)
        } else {
            Store::open_existing(&root, options.map_size)
        }
        .ok();
        Ok(Self {
            root,
            store,
            options,
        })
    }

    pub fn load(
        &self,
        path: &Path,
        grammar: &Grammar,
        parser: &mut tree_sitter::Parser,
    ) -> Result<LoadedFile, LoadError> {
        let options = LoadOptions {
            pack: tree_sitter_squatter::PackOptions {
                symbol_presence: self.options.symbol_presence,
                points: self.options.points,
                ..tree_sitter_squatter::PackOptions::default()
            },
            ..LoadOptions::default()
        };
        Ok(self.load_with_options(path, grammar, parser, options)?.file)
    }

    pub fn load_with_options(
        &self,
        path: &Path,
        grammar: &Grammar,
        parser: &mut tree_sitter::Parser,
        options: LoadOptions<'_>,
    ) -> Result<LoadResult, LoadError> {
        self.load_impl(path, grammar, parser, options, None)
    }

    pub fn load_with_context(
        &self,
        path: &Path,
        grammar: &Grammar,
        context: &mut LoadContext,
        options: LoadOptions<'_>,
    ) -> Result<LoadResult, LoadError> {
        self.load_impl(
            path,
            grammar,
            &mut context.parser,
            options,
            Some(&mut context.packing),
        )
    }

    fn load_impl(
        &self,
        path: &Path,
        grammar: &Grammar,
        parser: &mut tree_sitter::Parser,
        options: LoadOptions<'_>,
        mut packing: Option<&mut Option<tree_sitter_squatter::PackContext>>,
    ) -> Result<LoadResult, LoadError> {
        let mut pending = self.capture(path, grammar, &options)?;
        let started = std::time::Instant::now();
        loop {
            let cooperate = started.elapsed() < self.options.cooperation_wait;
            match pending.attempt(
                parser,
                options.cancellation,
                cooperate,
                packing.as_deref_mut(),
            )? {
                LoadStep::Ready(result) => return Ok(result),
                LoadStep::Deferred(next) => {
                    pending = next;
                    options.check()?;
                    let remaining = self
                        .options
                        .cooperation_wait
                        .saturating_sub(started.elapsed());
                    std::thread::sleep(remaining.min(std::time::Duration::from_millis(5)));
                }
            }
        }
    }

    /// Attempt once, returning captured work immediately if another parser owns it.
    pub fn load_step(
        &self,
        path: &Path,
        grammar: &Grammar,
        parser: &mut tree_sitter::Parser,
        options: LoadOptions<'_>,
    ) -> Result<LoadStep, LoadError> {
        self.capture(path, grammar, &options)?
            .resume(parser, options.cancellation)
    }

    /// Nonblocking load using reusable worker scratch.
    pub fn load_step_with_context(
        &self,
        path: &Path,
        grammar: &Grammar,
        context: &mut LoadContext,
        options: LoadOptions<'_>,
    ) -> Result<LoadStep, LoadError> {
        self.capture(path, grammar, &options)?
            .resume_with_context(context, options.cancellation)
    }

    fn capture(
        &self,
        path: &Path,
        grammar: &Grammar,
        options: &LoadOptions<'_>,
    ) -> Result<PendingLoad, LoadError> {
        let (path, encoded) = identity::path(path)?;
        options.check()?;
        let source_path = self.root.join(path);
        let mut open = OpenOptions::new();
        open.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Avoid blocking on a FIFO before metadata can reject it.
            open.custom_flags(libc::O_NONBLOCK);
        }
        let mut input = open.open(&source_path)?;
        let metadata = input.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source is not a regular file",
            )
            .into());
        }
        if metadata.len() > u32::MAX as u64 {
            return Err(LoadError::TooLarge);
        }
        let mut source = Vec::new();
        let mut chunk = [0; 64 * 1024];
        loop {
            options.check()?;
            let count = input.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            if source
                .len()
                .checked_add(count)
                .is_none_or(|len| len > u32::MAX as usize)
            {
                return Err(LoadError::TooLarge);
            }
            source.extend_from_slice(&chunk[..count]);
        }
        let source: Arc<[u8]> = source.into();
        let pack = options.pack;
        let mut request =
            Request::new(encoded, &source, grammar, pack.symbol_presence, pack.points);
        // Symlinked files outside the project can be read but are not persisted.
        let persistable = source_path
            .canonicalize()
            .is_ok_and(|p| p.starts_with(&self.root));
        let store = self.store.as_ref().filter(|_| persistable);
        if let Some(store) = store {
            request.current_guard = store.current_guard(&request);
        }
        Ok(PendingLoad {
            request: Arc::new(request),
            source,
            grammar: grammar.clone(),
            store: store.cloned(),
            pack,
            write: options.write,
            read: self.options.read,
            persistable,
        })
    }
}

impl PendingLoad {
    pub fn resume(
        self,
        parser: &mut tree_sitter::Parser,
        cancellation: Option<&AtomicBool>,
    ) -> Result<LoadStep, LoadError> {
        self.attempt(parser, cancellation, true, None)
    }

    /// Explicit escape hatch for callers whose wait budget has expired.
    pub fn parse_now(
        self,
        parser: &mut tree_sitter::Parser,
        cancellation: Option<&AtomicBool>,
    ) -> Result<LoadResult, LoadError> {
        match self.attempt(parser, cancellation, false, None)? {
            LoadStep::Ready(result) => Ok(result),
            LoadStep::Deferred(_) => unreachable!("cooperation disabled"),
        }
    }

    pub fn resume_with_context(
        self,
        context: &mut LoadContext,
        cancellation: Option<&AtomicBool>,
    ) -> Result<LoadStep, LoadError> {
        self.attempt(
            &mut context.parser,
            cancellation,
            true,
            Some(&mut context.packing),
        )
    }

    pub fn parse_now_with_context(
        self,
        context: &mut LoadContext,
        cancellation: Option<&AtomicBool>,
    ) -> Result<LoadResult, LoadError> {
        match self.attempt(
            &mut context.parser,
            cancellation,
            false,
            Some(&mut context.packing),
        )? {
            LoadStep::Ready(result) => Ok(result),
            LoadStep::Deferred(_) => unreachable!("cooperation disabled"),
        }
    }

    fn attempt(
        self,
        parser: &mut tree_sitter::Parser,
        cancellation: Option<&AtomicBool>,
        cooperate: bool,
        packing: Option<&mut Option<tree_sitter_squatter::PackContext>>,
    ) -> Result<LoadStep, LoadError> {
        let options = LoadOptions {
            pack: self.pack,
            write: self.write,
            cancellation,
        };
        options.check()?;
        let store = self.store.clone();
        let hit = || {
            store.as_ref().and_then(|store| {
                if self.read == ReadPolicy::PreferTransactionBacked
                    && let Some((tree, complete)) = snapshot::get(
                        store,
                        &self.request,
                        &self.source,
                        &self.grammar,
                        cancellation,
                    )
                {
                    return Some((LoadedTree::Backed(Arc::new(tree)), complete));
                }
                store
                    .get_with_cancel(&self.request, &self.source, &self.grammar, cancellation)
                    .map(|(tree, complete)| (LoadedTree::Owned(Arc::new(tree)), complete))
            })
        };
        let ready = |(tree, complete): (LoadedTree, bool)| {
            let file = LoadedFile {
                source: self.source.clone(),
                tree,
                hit: true,
                cleanup: store
                    .as_ref()
                    .map(|store| (store.clone(), self.request.clone())),
            };
            let pending_write = store
                .as_ref()
                .filter(|_| self.write != WritePolicy::Disabled && !complete)
                .map(|store| PendingWrite {
                    store: Some(store.clone()),
                    request: self.request.clone(),
                    grammar: self.grammar.clone(),
                    file: file.clone(),
                });
            if self.write == WritePolicy::Inline {
                if let Some(write) = &pending_write {
                    let _ = write.publish_with_cancellation(
                        options.cancellation.unwrap_or(&AtomicBool::new(false)),
                    );
                }
                LoadStep::Ready(LoadResult {
                    file,
                    pending_write: None,
                })
            } else {
                LoadStep::Ready(LoadResult {
                    file,
                    pending_write,
                })
            }
        };
        if let Some(tree) = hit() {
            return Ok(ready(tree));
        }
        // Errors disable this optimization. A busy owner instead defers work.
        let _work = if cooperate {
            if let Some(store) = &store {
                match store.work(&self.request) {
                    Ok(Some(guard)) => Some(guard),
                    Ok(None) => return Ok(LoadStep::Deferred(self)),
                    Err(_) => None,
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(tree) = hit() {
            return Ok(ready(tree));
        }
        parser.reset();
        parser
            .set_language(&self.grammar.prepared.language())
            .map_err(LoadError::Language)?;
        parser
            .set_included_ranges(&[])
            .expect("empty ranges are always valid");
        let mut progress = |_: &tree_sitter::ParseState| {
            if options.cancelled() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let tree = parser.parse_with_options(
            &mut |offset, _| self.source.get(offset..).unwrap_or_default(),
            None,
            options
                .cancellation
                .map(|_| tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
        );
        let Some(tree) = tree else {
            parser.reset();
            options.check()?;
            return Err(LoadError::ParseFailed);
        };
        options.check()?;
        let pack_options = self.pack;
        let packed = if let Some(packing) = packing {
            if packing.is_none() {
                *packing = Some(tree_sitter_squatter::PackContext::new().map_err(LoadError::Pack)?);
            }
            packing
                .as_mut()
                .unwrap()
                .pack_with_options(&self.grammar.prepared, &tree, pack_options)
        } else {
            tree_sitter_squatter::Tree::pack_with_options(
                &self.grammar.prepared,
                &tree,
                pack_options,
            )
        }
        .map_err(LoadError::Pack)?;
        let file = LoadedFile {
            source: self.source.clone(),
            tree: LoadedTree::Owned(Arc::new(packed)),
            hit: false,
            cleanup: store
                .as_ref()
                .map(|store| (store.clone(), self.request.clone())),
        };
        let pending_write = match (options.write, store.as_ref()) {
            (WritePolicy::Disabled, _) => None,
            (_, _) if !self.persistable => None,
            (WritePolicy::Transfer, _) | (_, Some(_)) => Some(PendingWrite {
                store: store.clone(),
                request: self.request,
                grammar: self.grammar.clone(),
                file: file.clone(),
            }),
            (_, None) => None,
        };
        if options.write == WritePolicy::Inline {
            if let Some(write) = &pending_write {
                let _ = write.publish_with_cancellation(
                    options.cancellation.unwrap_or(&AtomicBool::new(false)),
                );
            }
            Ok(LoadStep::Ready(LoadResult {
                file,
                pending_write: None,
            }))
        } else {
            Ok(LoadStep::Ready(LoadResult {
                file,
                pending_write,
            }))
        }
    }
}
