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
pub use identity::{IdentifiedLanguage, LanguageIdentity, LanguageVersion};
pub use maintenance::{
    EvictionOutcome, Maintenance, MaintenanceProgress, MaintenanceState, MissingSweep, SidecarKind,
};
pub use store::{CacheError, WriteOutcome};

use identity::Request;
use std::{
    fs::OpenOptions,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};
use store::Store;
use tree_squatter::{
    PackedParseOptions, ParseOptions, Parser, ParserError, traits::ParseStateLike,
};

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
    Owned(Arc<tree_squatter::Tree>),
    Backed(Arc<tree_squatter::BackedTree>),
}
impl LoadedTree {
    fn tree(&self) -> &tree_squatter::Tree {
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

/// The shared callback covers parsing, packing, and load cancellation checks.
/// Outside parsing and packing, both phase flags are false: offsets report bytes
/// captured while reading, zero while waiting or probing, and source length when
/// returning or publishing a completed tree. Only completed trees report errors.
#[derive(Default)]
pub struct LoadOptions<'a> {
    pub pack: tree_squatter::PackOptions,
    pub write: WritePolicy,
    pub parse: ParseOptions<'a>,
}

impl LoadOptions<'_> {
    pub fn reborrow(&mut self) -> LoadOptions<'_> {
        LoadOptions {
            pack: self.pack,
            write: self.write,
            parse: self.parse.reborrow(),
        }
    }
}

struct LoadState {
    byte: usize,
    has_error: bool,
}

impl ParseStateLike for LoadState {
    fn current_byte_offset(&self) -> usize {
        self.byte
    }
    fn has_error(&self) -> bool {
        self.has_error
    }
    fn is_converting(&self) -> bool {
        false
    }
    fn current_byte_offset_descends(&self) -> bool {
        false
    }
}

fn cancelled(options: &mut ParseOptions<'_>, byte: usize, has_error: bool) -> bool {
    options
        .progress_callback
        .as_mut()
        .is_some_and(|callback| callback(&LoadState { byte, has_error }).is_break())
}

fn check(options: &mut ParseOptions<'_>, byte: usize, has_error: bool) -> Result<(), LoadError> {
    if cancelled(options, byte, has_error) {
        Err(LoadError::Cancelled)
    } else {
        Ok(())
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
    Pack(tree_squatter::Error),
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

impl From<ParserError> for LoadError {
    fn from(error: ParserError) -> Self {
        match error {
            ParserError::Canceled => Self::Cancelled,
            ParserError::NoLanguage => Self::ParseFailed,
            ParserError::Pack(error) => Self::Pack(error),
        }
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
    pub fn tree(&self) -> &tree_squatter::Tree {
        self.tree.tree()
    }
    pub fn transaction_backed(&self) -> bool {
        matches!(self.tree, LoadedTree::Backed(_))
    }
    /// Return an owned copy. Existing aliases keep their snapshots until dropped.
    /// Auxiliary semantics are not revalidated while detaching.
    pub fn detach(&self) -> Result<Self, tree_squatter::Error> {
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
    pub fn evict_sidecar(&self, kind: SidecarKind) -> Result<EvictionOutcome, CacheError> {
        match &self.cleanup {
            Some((store, request)) => store.evict_sidecar(request, kind),
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
    language: IdentifiedLanguage,
    store: Option<Arc<Store>>,
    pack: tree_squatter::PackOptions,
    write: WritePolicy,
    read: ReadPolicy,
    persistable: bool,
}

pub struct LoadResult {
    pub file: LoadedFile,
    pub pending_write: Option<PendingWrite>,
}

/// Per-worker parser and packing scratch, reused across language changes.
/// Empty packing scratch allocates nothing; cache hits do not initialize a language.
#[derive(Default)]
pub struct LoadContext {
    parser: Parser,
}

impl LoadContext {
    /// Release packing scratch while keeping the parser and prepared language.
    pub fn drop_scratch(&mut self) {
        self.parser.drop_scratch();
    }
}

/// Complete, immutable work. No LMDB transaction is retained until execution.
pub struct PendingWrite {
    store: Option<Arc<Store>>,
    request: Arc<Request>,
    language: IdentifiedLanguage,
    file: LoadedFile,
}
impl PendingWrite {
    /// Optional generation cleanup to execute after successful publication.
    pub fn maintenance(&self) -> Option<Maintenance> {
        self.file.maintenance()
    }
    /// Busy work can be retried later using this same item.
    pub fn publish(&self) -> Result<WriteOutcome, CacheError> {
        self.publish_with_options(ParseOptions::default())
    }
    /// Polls before publication and before commit. Cancellation never rolls back
    /// a completed commit. The callback receives the completed tree's source length.
    pub fn publish_with_options(
        &self,
        mut options: ParseOptions<'_>,
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
            &self.language,
            || {
                cancelled(
                    &mut options,
                    self.file.source().len(),
                    self.file.tree().root_node().has_error(),
                )
            },
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
    pub fn prepare_language(
        &self,
        tree_sitter_language: &tree_sitter::Language,
        fallback_name: &str,
    ) -> Result<IdentifiedLanguage, tree_squatter::Error> {
        let identity = LanguageIdentity::new(tree_sitter_language, fallback_name);
        self.prepare_identified_language(tree_sitter_language, identity)
    }

    /// Prepare a language with a version fallback for parsers without metadata.
    pub fn prepare_language_with_version(
        &self,
        tree_sitter_language: &tree_sitter::Language,
        fallback_name: &str,
        fallback_version: LanguageVersion,
    ) -> Result<IdentifiedLanguage, tree_squatter::Error> {
        let identity = LanguageIdentity::new_with_version(
            tree_sitter_language,
            fallback_name,
            fallback_version,
        );
        self.prepare_identified_language(tree_sitter_language, identity)
    }

    fn prepare_identified_language(
        &self,
        tree_sitter_language: &tree_sitter::Language,
        identity: LanguageIdentity,
    ) -> Result<IdentifiedLanguage, tree_squatter::Error> {
        let prepared = match self
            .store
            .as_ref()
            .and_then(|store| store.prepare_language(tree_sitter_language, identity.hash))
        {
            Some(prepared) => prepared,
            None => tree_squatter::Language::new(tree_sitter_language)?,
        };
        Ok(IdentifiedLanguage::new(prepared, identity))
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
        language: &IdentifiedLanguage,
        parser: &mut Parser,
    ) -> Result<LoadedFile, LoadError> {
        let options = LoadOptions {
            pack: tree_squatter::PackOptions {
                symbol_presence: self.options.symbol_presence,
                points: self.options.points,
                ..tree_squatter::PackOptions::default()
            },
            ..LoadOptions::default()
        };
        Ok(self
            .load_with_options(path, language, parser, options)?
            .file)
    }

    pub fn load_with_options(
        &self,
        path: &Path,
        language: &IdentifiedLanguage,
        parser: &mut Parser,
        options: LoadOptions<'_>,
    ) -> Result<LoadResult, LoadError> {
        self.load_impl(path, language, parser, options)
    }

    pub fn load_with_context(
        &self,
        path: &Path,
        language: &IdentifiedLanguage,
        context: &mut LoadContext,
        options: LoadOptions<'_>,
    ) -> Result<LoadResult, LoadError> {
        self.load_impl(path, language, &mut context.parser, options)
    }

    fn load_impl(
        &self,
        path: &Path,
        language: &IdentifiedLanguage,
        parser: &mut Parser,
        mut options: LoadOptions<'_>,
    ) -> Result<LoadResult, LoadError> {
        let mut pending = self.capture(path, language, &mut options)?;
        let started = std::time::Instant::now();
        loop {
            let cooperate = started.elapsed() < self.options.cooperation_wait;
            match pending.attempt(parser, &mut options.parse, cooperate)? {
                LoadStep::Ready(result) => return Ok(result),
                LoadStep::Deferred(next) => {
                    pending = next;
                    check(&mut options.parse, 0, false)?;
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
        language: &IdentifiedLanguage,
        parser: &mut Parser,
        mut options: LoadOptions<'_>,
    ) -> Result<LoadStep, LoadError> {
        self.capture(path, language, &mut options)?
            .resume(parser, options.parse)
    }

    /// Nonblocking load using reusable worker scratch.
    pub fn load_step_with_context(
        &self,
        path: &Path,
        language: &IdentifiedLanguage,
        context: &mut LoadContext,
        options: LoadOptions<'_>,
    ) -> Result<LoadStep, LoadError> {
        self.load_step(path, language, &mut context.parser, options)
    }

    fn capture(
        &self,
        path: &Path,
        language: &IdentifiedLanguage,
        options: &mut LoadOptions<'_>,
    ) -> Result<PendingLoad, LoadError> {
        let (path, encoded) = identity::path(path)?;
        check(&mut options.parse, 0, false)?;
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
            check(&mut options.parse, source.len(), false)?;
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
        let mut request = Request::new(
            encoded,
            &source,
            language,
            pack.symbol_presence,
            pack.points,
        );
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
            language: language.clone(),
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
        parser: &mut Parser,
        mut options: ParseOptions<'_>,
    ) -> Result<LoadStep, LoadError> {
        self.attempt(parser, &mut options, true)
    }

    /// Explicit escape hatch for callers whose wait budget has expired.
    pub fn parse_now(
        self,
        parser: &mut Parser,
        mut options: ParseOptions<'_>,
    ) -> Result<LoadResult, LoadError> {
        match self.attempt(parser, &mut options, false)? {
            LoadStep::Ready(result) => Ok(result),
            LoadStep::Deferred(_) => unreachable!("cooperation disabled"),
        }
    }

    pub fn resume_with_context(
        self,
        context: &mut LoadContext,
        options: ParseOptions<'_>,
    ) -> Result<LoadStep, LoadError> {
        self.resume(&mut context.parser, options)
    }

    pub fn parse_now_with_context(
        self,
        context: &mut LoadContext,
        options: ParseOptions<'_>,
    ) -> Result<LoadResult, LoadError> {
        self.parse_now(&mut context.parser, options)
    }

    fn attempt(
        self,
        parser: &mut Parser,
        options: &mut ParseOptions<'_>,
        cooperate: bool,
    ) -> Result<LoadStep, LoadError> {
        check(options, 0, false)?;
        let store = self.store.clone();
        let hit = || {
            store.as_ref().and_then(|store| {
                if self.read == ReadPolicy::PreferTransactionBacked
                    && let Some((tree, complete)) =
                        snapshot::get(store, &self.request, &self.source, &self.language)
                {
                    return Some((LoadedTree::Backed(Arc::new(tree)), complete));
                }
                store
                    .get(&self.request, &self.source, &self.language)
                    .map(|(tree, complete)| (LoadedTree::Owned(Arc::new(tree)), complete))
            })
        };
        if let Some((tree, complete)) = hit() {
            return self.finish(tree, true, complete, options);
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
        if let Some((tree, complete)) = hit() {
            return self.finish(tree, true, complete, options);
        }
        parser
            .set_language(&self.language.prepared)
            .map_err(LoadError::Language)?;
        let tree = parser.parse_with_options(
            &self.source,
            PackedParseOptions {
                parse: options.reborrow(),
                pack: self.pack,
            },
        )?;
        self.finish(LoadedTree::Owned(Arc::new(tree)), false, false, options)
    }

    fn finish(
        self,
        tree: LoadedTree,
        hit: bool,
        complete: bool,
        options: &mut ParseOptions<'_>,
    ) -> Result<LoadStep, LoadError> {
        check(
            options,
            self.source.len(),
            tree.tree().root_node().has_error(),
        )?;
        let file = LoadedFile {
            source: self.source,
            tree,
            hit,
            cleanup: self
                .store
                .as_ref()
                .map(|store| (store.clone(), self.request.clone())),
        };
        let mut pending_write = match (self.write, self.store.as_ref()) {
            (WritePolicy::Disabled, _) => None,
            (_, _) if complete || !self.persistable => None,
            (WritePolicy::Transfer, _) | (_, Some(_)) => Some(PendingWrite {
                store: self.store,
                request: self.request,
                language: self.language,
                file: file.clone(),
            }),
            (_, None) => None,
        };
        if self.write == WritePolicy::Inline
            && let Some(write) = pending_write.take()
            && matches!(
                write.publish_with_options(options.reborrow()),
                Err(CacheError::Cancelled)
            )
        {
            return Err(LoadError::Cancelled);
        }
        Ok(LoadStep::Ready(LoadResult {
            file,
            pending_write,
        }))
    }
}
