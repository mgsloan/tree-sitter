use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicUsize},
    },
};

use crate::identity::{CurrentGuard, Grammar, Request};
use heed::{Env, EnvOpenOptions, WithoutTls, types::Bytes};

pub(crate) type Database = heed::Database<Bytes, Bytes>;

// Prototype formats stay at version 0; no persisted data needs backward compatibility.
const SCHEMA: &[u8] = b"tree-squatter-persistence side data prototype 0";

pub(crate) struct Store {
    pub(crate) env: Env<WithoutTls>,
    pub(crate) paths: Database,
    pub(crate) sources: Database,
    pub(crate) trees: Database,
    pub(crate) presence: Database,
    pub(crate) points: Database,
    pub(crate) grammars: Database,
    pub(crate) current: Database,
    pub(crate) writer: Mutex<File>,
    pub(crate) backed_readers: AtomicUsize,
    work: crate::work::WorkLocks,
    // Retain application-side directory identity/sidecar access. Heed canonicalizes
    // its open path, so this does not anchor LMDB's own pathname resolution.
    _directory: File,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GrammarFingerprint;

    #[test]
    fn source_and_tree_publish_atomically_and_corruption_is_a_miss() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path(), 1024 * 1024).unwrap();
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let grammar = Grammar::new(
            tree_sitter_squatter::Grammar::new(&language).unwrap(),
            GrammarFingerprint([42; 32]),
        );
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let native = parser.parse(b"[1]", None).unwrap();
        let tree = tree_sitter_squatter::Tree::pack_with_options(
            &grammar.prepared,
            &native,
            tree_sitter_squatter::PackOptions {
                initial_group_capacity: 128,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(tree.group_capacity() > tree.group_count());
        let original = tree.as_bytes().to_vec();
        let request = Request::new(b"test.json".to_vec(), b"[1]", &grammar, true, true);
        // Cancel after the reservation has been filled, immediately before
        // commit: no source, tree, path, or current-generation record may escape.
        let checks = std::cell::Cell::new(0);
        assert!(matches!(
            store.publish(&request, b"[1]", &tree, &grammar, || {
                checks.set(checks.get() + 1);
                checks.get() == 2
            }),
            Err(CacheError::Cancelled)
        ));
        let before = store.env.read_txn().unwrap();
        for (name, database, key) in [
            ("sources", store.sources, request.source_key.as_slice()),
            ("trees", store.trees, request.tree_key.as_slice()),
            (
                "grammars",
                store.grammars,
                crate::identity::grammar_key(grammar.fingerprint).as_slice(),
            ),
            ("paths", store.paths, &request.source_key[..32]),
            ("current", store.current, &request.source_key[..32]),
        ] {
            assert!(database.get(&before, key).unwrap().is_none(), "{name}");
        }
        assert_eq!(
            store
                .publish(&request, b"[1]", &tree, &grammar, || false)
                .unwrap(),
            WriteOutcome::Published
        );
        assert_eq!(
            store.sources.get(&before, &request.source_key).unwrap(),
            None
        );
        assert_eq!(store.trees.get(&before, &request.tree_key).unwrap(), None);
        let after = store.env.read_txn().unwrap();
        let stored = request
            .decode(store.trees.get(&after, &request.tree_key).unwrap().unwrap())
            .unwrap();
        assert_eq!(stored, tree.repack().unwrap().as_bytes());
        assert!(stored.len() < original.len());
        assert_eq!(tree.as_bytes(), original);
        assert_eq!(
            store.sources.get(&after, &request.source_key).unwrap(),
            Some(b"[1]".as_slice())
        );
        assert!(
            store
                .trees
                .get(&after, &request.tree_key)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .grammars
                .get(&after, &crate::identity::grammar_key(grammar.fingerprint))
                .unwrap(),
            Some([].as_slice())
        );
        drop(after);
        drop(before);

        let guard = gate(&store.writer).unwrap().unwrap();
        let mut tx = store.env.write_txn().unwrap();
        store
            .trees
            .put(&mut tx, &request.tree_key, b"broken")
            .unwrap();
        tx.commit().unwrap();
        drop(guard);
        assert!(store.get(&request, b"[1]", &grammar).is_none());
        assert_eq!(
            store
                .publish(&request, b"[1]", &tree, &grammar, || false)
                .unwrap(),
            WriteOutcome::Published
        );
        assert!(store.get(&request, b"[1]", &grammar).is_some());

        let guard = gate(&store.writer).unwrap().unwrap();
        assert_eq!(
            store
                .publish(&request, b"[1]", &tree, &grammar, || false)
                .unwrap(),
            WriteOutcome::Busy
        );
        drop(guard);
    }

    #[test]
    fn heed_keeps_durable_locking_and_shares_environment_aliases() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path(), 1024 * 1024).unwrap();
        let flags = store.env.flags().unwrap().unwrap();
        assert!(!flags.intersects(
            heed::EnvFlags::NO_SYNC
                | heed::EnvFlags::NO_META_SYNC
                | heed::EnvFlags::NO_LOCK
                | heed::EnvFlags::WRITE_MAP
                | heed::EnvFlags::MAP_ASYNC
        ));
        let other = Store::open(&root.path().join("."), 1024 * 1024).unwrap();
        assert!(Arc::ptr_eq(&store, &other));
        #[cfg(unix)]
        {
            let alias_root = tempfile::tempdir().unwrap();
            let alias = alias_root.path().join("alias");
            std::os::unix::fs::symlink(root.path(), &alias).unwrap();
            assert!(Arc::ptr_eq(
                &store,
                &Store::open(&alias, 1024 * 1024).unwrap()
            ));
        }
    }

    #[test]
    fn wide_supertype_dictionary_round_trips_through_lmdb() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path(), 1024 * 1024).unwrap();
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
        };
        let grammar = Grammar::new(
            tree_sitter_squatter::Grammar::new(&language).unwrap(),
            GrammarFingerprint([7; 32]),
        );
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let native = parser.parse(b"class C {}", None).unwrap();
        let tree = tree_sitter_squatter::Tree::pack(&grammar.prepared, &native).unwrap();
        let request = Request::new(b"test.cs".to_vec(), b"class C {}", &grammar, true, true);
        store
            .publish(&request, b"class C {}", &tree, &grammar, || false)
            .unwrap();
        let expected = tree.grammar_cache().unwrap();
        assert!(!expected.is_empty());
        drop(tree);

        let restored = store
            .prepare_grammar(&language, grammar.fingerprint)
            .unwrap();
        assert_eq!(restored.cache().unwrap(), expected);
        assert!(store.get(&request, b"class C {}", &grammar).is_some());
        let mut tx = store.env.write_txn().unwrap();
        store
            .grammars
            .put(
                &mut tx,
                &crate::identity::grammar_key(grammar.fingerprint),
                b"invalid",
            )
            .unwrap();
        tx.commit().unwrap();
        assert!(
            store
                .prepare_grammar(&language, grammar.fingerprint)
                .is_none()
        );
        let persistence = crate::Persistence::open(root.path(), crate::Options::default()).unwrap();
        let rebuilt = persistence
            .prepare_grammar(&language, grammar.fingerprint)
            .unwrap();
        assert_eq!(rebuilt.prepared.cache().unwrap(), expected);
    }
}

#[derive(Debug)]
pub enum CacheError {
    Io(io::Error),
    Database(heed::Error),
    IncompatibleSchema,
    PathCollision,
    Cancelled,
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cache operation failed: {self:?}")
    }
}
impl std::error::Error for CacheError {}
impl From<io::Error> for CacheError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<heed::Error> for CacheError {
    fn from(error: heed::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    Published,
    AlreadyPresent,
    Busy,
}

// Strong entries deliberately keep LMDB open until process exit. This avoids a
// last-Arc-drop/reopen race and fcntl lock breakage from duplicate environments.
// A later bounded registry must serialize final close with a subsequent reopen.
#[derive(Hash, PartialEq, Eq)]
enum EnvironmentKey {
    #[cfg(unix)]
    Inode(u64, u64),
    #[cfg(not(unix))]
    Path(PathBuf),
}
static STORES: OnceLock<Mutex<HashMap<EnvironmentKey, Arc<Store>>>> = OnceLock::new();

pub(crate) struct Writer<'a>(MutexGuard<'a, File>);
impl Drop for Writer<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(crate) fn gate(file: &Mutex<File>) -> Result<Option<Writer<'_>>, CacheError> {
    let Ok(file) = file.try_lock() else {
        return Ok(None);
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(Writer(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

fn no_link(path: &Path, directory: bool) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if !metadata.file_type().is_symlink()
                && if directory {
                    metadata.is_dir()
                } else {
                    metadata.is_file()
                } =>
        {
            Ok(())
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unexpected cache pathname",
        )),
        Err(error) if !directory && error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

impl Store {
    pub fn open(root: &Path, map_size: usize) -> Result<Arc<Self>, CacheError> {
        Self::open_impl(root, map_size, true)
    }

    pub fn open_existing(root: &Path, map_size: usize) -> Result<Arc<Self>, CacheError> {
        Self::open_impl(root, map_size, false)
    }

    fn open_impl(root: &Path, map_size: usize, create: bool) -> Result<Arc<Self>, CacheError> {
        let cache = root.join(crate::CACHE_DIRECTORY);
        if !create {
            for name in ["data.mdb", "lock.mdb", "cooperation.lock"] {
                fs::metadata(cache.join(name))?;
            }
        }
        if create {
            match fs::create_dir(&cache) {
                Ok(()) => {
                    File::open(root)?.sync_all()?;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
        }
        no_link(&cache, true)?;
        let mut directory_options = OpenOptions::new();
        directory_options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            directory_options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        }
        let directory = directory_options.open(&cache)?;
        let canonical = cache.canonicalize()?;
        #[cfg(unix)]
        let key = {
            use std::os::unix::fs::MetadataExt;
            let metadata = directory.metadata()?;
            EnvironmentKey::Inode(metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let key = EnvironmentKey::Path(canonical.clone());
        let mut registry = STORES.get_or_init(Default::default).lock().unwrap();
        if let Some(store) = registry.get(&key) {
            return Ok(store.clone());
        }
        #[cfg(target_os = "linux")]
        let anchor = {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
        };
        #[cfg(not(target_os = "linux"))]
        let anchor = canonical.clone();
        #[cfg(target_os = "linux")]
        let _ = canonical;
        // LMDB opens its own files. The initial backend assumes cooperating
        // writers in a trusted cache directory, not hostile leaf substitution.
        for name in ["data.mdb", "lock.mdb", "cooperation.lock"] {
            no_link(&anchor.join(name), false)?;
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(create)
            .truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
        }
        let lock_file = options.open(anchor.join("cooperation.lock"))?;
        let work = crate::work::WorkLocks::new(lock_file.try_clone()?);
        let writer = Mutex::new(lock_file);
        // Existing caches need no writer admission to open. In particular, a
        // background maintenance batch must not disable a foreground reader.
        let guard = if anchor.join("data.mdb").exists() {
            None
        } else {
            Some(gate(&writer)?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::WouldBlock, "cache initialization busy")
            })?)
        };
        let existed = anchor.join("data.mdb").exists();
        // Cooperating writers, trusted local directory, native locking/sync,
        // one retained environment per inode, and no resizing of live mappings.
        let env = unsafe {
            EnvOpenOptions::new()
                .read_txn_without_tls()
                .max_dbs(8)
                .max_readers(256)
                .map_size(map_size)
                .open(&anchor)?
        };
        let (paths, sources, trees, presence, points, grammars, current) = if existed {
            let tx = env.read_txn()?;
            let open = |name| -> Result<Database, CacheError> {
                env.open_database(&tx, Some(name))?
                    .ok_or(CacheError::IncompatibleSchema)
            };
            let meta = open("meta")?;
            if meta.get(&tx, b"schema")? != Some(SCHEMA) {
                return Err(CacheError::IncompatibleSchema);
            }
            let databases = (
                open("paths")?,
                open("sources")?,
                open("trees")?,
                open("presence")?,
                open("points")?,
                open("grammars")?,
                open("current")?,
            );
            // A read transaction must commit to publish newly opened DBI
            // handles into this process's environment as well.
            tx.commit()?;
            databases
        } else {
            // Only initial creation needs a write transaction and directory sync.
            let mut tx = env.write_txn()?;
            let meta: Database = env.create_database(&mut tx, Some("meta"))?;
            meta.put(&mut tx, b"schema", SCHEMA)?;
            let databases = (
                env.create_database(&mut tx, Some("paths"))?,
                env.create_database(&mut tx, Some("sources"))?,
                env.create_database(&mut tx, Some("trees"))?,
                env.create_database(&mut tx, Some("presence"))?,
                env.create_database(&mut tx, Some("points"))?,
                env.create_database(&mut tx, Some("grammars"))?,
                env.create_database(&mut tx, Some("current"))?,
            );
            tx.commit()?;
            directory.sync_all()?;
            databases
        };
        drop(guard);
        let store = Arc::new(Self {
            env,
            paths,
            sources,
            trees,
            presence,
            points,
            grammars,
            current,
            writer,
            backed_readers: AtomicUsize::new(0),
            work,
            _directory: directory,
        });
        registry.insert(key, store.clone());
        Ok(store)
    }

    pub fn work(&self, request: &Request) -> io::Result<Option<crate::work::WorkGuard<'_>>> {
        self.work.acquire(&request.tree_key)
    }

    pub fn current_guard(&self, request: &Request) -> CurrentGuard {
        let Ok(tx) = self.env.read_txn() else {
            return CurrentGuard::Unchecked;
        };
        match self.current.get(&tx, &request.source_key[..32]) {
            Ok(Some(bytes)) => bytes
                .try_into()
                .map(CurrentGuard::Current)
                .unwrap_or(CurrentGuard::Unchecked),
            Ok(None) => CurrentGuard::Missing,
            Err(_) => CurrentGuard::Unchecked,
        }
    }

    #[cfg(test)]
    pub fn get(
        &self,
        request: &Request,
        source: &[u8],
        grammar: &Grammar,
    ) -> Option<tree_sitter_squatter::Tree> {
        self.get_with_cancel(request, source, grammar, None)
            .map(|(tree, _)| tree)
    }

    pub fn get_with_cancel(
        &self,
        request: &Request,
        source: &[u8],
        grammar: &Grammar,
        cancellation: Option<&AtomicBool>,
    ) -> Option<(tree_sitter_squatter::Tree, bool)> {
        let tx = self.env.read_txn().ok()?;
        if self.paths.get(&tx, &request.source_key[..32]).ok()?? != request.path
            || self.sources.get(&tx, &request.source_key).ok()?? != source
        {
            return None;
        }
        let value = self.trees.get(&tx, &request.tree_key).ok()??;
        let slab = request.decode(value)?;
        // Core validation is independent of optional sidecar contents.
        let mut tree =
            tree_sitter_squatter::Tree::from_bytes_safety_checked(&grammar.prepared, slab).ok()?;
        if tree
            .root_node()
            .preorder()
            .nodes()
            .any(|node| node.end_byte() > source.len())
        {
            return None;
        }
        let mut complete = true;
        if request.presence {
            let loaded = self
                .presence
                .get(&tx, &request.tree_key)
                .ok()
                .flatten()
                .and_then(|bytes| {
                    tree_sitter_squatter::PresenceCache::copy_from_bytes(&tree, bytes).ok()
                });
            complete &= loaded.is_some();
            let cache = loaded
                .or_else(|| tree_sitter_squatter::PresenceCache::build(&tree, cancellation).ok())?;
            tree.set_presence_cache(cache).ok()?;
        }
        if request.points {
            let loaded = self
                .points
                .get(&tx, &request.tree_key)
                .ok()
                .flatten()
                .and_then(|bytes| {
                    tree_sitter_squatter::PointData::copy_from_bytes(&tree, bytes).ok()
                });
            complete &= loaded.is_some();
            let points = loaded.or_else(|| {
                let source_points = tree_sitter_squatter::SourcePoints::new(source).ok()?;
                tree_sitter_squatter::PointData::build(&tree, &source_points, cancellation).ok()
            })?;
            tree.set_point_data(points).ok()?;
        }
        Some((tree, complete))
    }

    pub fn prepare_grammar(
        &self,
        language: &tree_sitter::Language,
        fingerprint: crate::GrammarFingerprint,
    ) -> Option<tree_sitter_squatter::Grammar> {
        let tx = self.env.read_txn().ok()?;
        let bytes = self
            .grammars
            .get(&tx, &crate::identity::grammar_key(fingerprint))
            .ok()??;
        tree_sitter_squatter::Grammar::from_cache(language, bytes).ok()
    }

    fn publication_state(
        &self,
        request: &Request,
        source: &[u8],
        tree: &tree_sitter_squatter::Tree,
        grammar: &Grammar,
    ) -> Option<(bool, bool)> {
        let tx = self.env.read_txn().ok()?;
        if self.paths.get(&tx, &request.source_key[..32]).ok()?? != request.path
            || self.sources.get(&tx, &request.source_key).ok()?? != source
        {
            return None;
        }
        let slab = request.decode(self.trees.get(&tx, &request.tree_key).ok()??)?;
        let borrowed = tree_sitter_squatter::Tree::from_bytes_borrowed(&grammar.prepared, slab);
        let copied;
        let existing = match &borrowed {
            Ok(tree) => &**tree,
            Err(_) => {
                copied =
                    tree_sitter_squatter::Tree::from_bytes_safety_checked(&grammar.prepared, slab)
                        .ok()?;
                &copied
            }
        };
        if existing
            .root_node()
            .preorder()
            .nodes()
            .any(|node| node.end_byte() > source.len())
        {
            return None;
        }
        // Compare with the completed sidecars supplied for publication. Loading
        // here would copy payloads and rebuild precisely the missing records.
        let matches = |database: Database, expected: Option<&[u8]>| {
            expected.is_none_or(|bytes| {
                database.get(&tx, &request.tree_key).ok().flatten() == Some(bytes)
            })
        };
        Some((
            matches(
                self.presence,
                tree.presence_cache().map(|cache| cache.as_bytes()),
            ),
            matches(
                self.points,
                tree.point_data().map(|points| points.as_bytes()),
            ),
        ))
    }

    pub fn publish(
        &self,
        request: &Request,
        source: &[u8],
        tree: &tree_sitter_squatter::Tree,
        grammar: &Grammar,
        cancelled: impl Fn() -> bool,
    ) -> Result<WriteOutcome, CacheError> {
        if tree.has_points() != request.points
            || tree.presence_cache().is_some() != request.presence
        {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "side data policy mismatch").into(),
            );
        }
        if cancelled() {
            return Err(CacheError::Cancelled);
        }
        let Some(_guard) = gate(&self.writer)? else {
            return Ok(WriteOutcome::Busy);
        };
        let existing = self.publication_state(request, source, tree, grammar);
        let already_present = existing.is_some();
        let (has_presence, has_points) = existing.unwrap_or((false, false));
        if already_present && has_presence && has_points {
            return Ok(WriteOutcome::AlreadyPresent);
        }
        let core_publication = if already_present {
            None
        } else {
            let grammar_cache = tree.grammar_cache().map_err(io::Error::other)?;
            let slab_size = tree.compact_size();
            let prefix_size = request.header.len() + 8;
            let value_size = prefix_size
                .checked_add(slab_size)
                .ok_or_else(|| io::Error::other("cache entry size overflow"))?;
            Some((grammar_cache, slab_size, prefix_size, value_size))
        };
        let mut tx = self.env.write_txn()?;
        let current = self.current.get(&tx, &request.source_key[..32])?;
        let superseded = match &request.current_guard {
            CurrentGuard::Unchecked => false,
            CurrentGuard::Missing => current.is_some_and(|value| value != request.source_key),
            CurrentGuard::Current(expected) => {
                current != Some(expected.as_slice())
                    && current != Some(request.source_key.as_slice())
            }
        };
        if superseded {
            return Ok(WriteOutcome::AlreadyPresent);
        }
        if let Some((grammar_cache, slab_size, prefix_size, value_size)) = core_publication {
            if let Some(path) = self.paths.get(&tx, &request.source_key[..32])?
                && path != request.path
            {
                return Err(CacheError::PathCollision);
            }
            self.paths
                .put(&mut tx, &request.source_key[..32], &request.path)?;
            if self.sources.get(&tx, &request.source_key)? != Some(source) {
                self.sources.put(&mut tx, &request.source_key, source)?;
            }
            self.trees
                .put_reserved(&mut tx, &request.tree_key, value_size, |reserved| {
                    reserved.write_all(&request.header)?;
                    reserved.write_all(&(slab_size as u64).to_le_bytes())?;
                    tree.copy_compact_into(&mut reserved.as_uninit_mut()[prefix_size..])
                        .map_err(io::Error::other)?;
                    // The envelope writes and successful compact copy initialized the
                    // entire reservation. No LMDB operation occurs while it is borrowed.
                    unsafe {
                        reserved.assume_written(value_size);
                    }
                    Ok(())
                })?;
            self.grammars.put(
                &mut tx,
                &crate::identity::grammar_key(grammar.fingerprint),
                &grammar_cache,
            )?;
            self.current
                .put(&mut tx, &request.source_key[..32], &request.source_key)?;
        }
        if !has_presence && let Some(cache) = tree.presence_cache() {
            self.presence
                .put(&mut tx, &request.tree_key, cache.as_bytes())?;
        }
        if !has_points && let Some(points) = tree.point_data() {
            self.points
                .put(&mut tx, &request.tree_key, points.as_bytes())?;
        }
        if cancelled() {
            return Err(CacheError::Cancelled);
        }
        tx.commit()?;
        Ok(WriteOutcome::Published)
    }
}
