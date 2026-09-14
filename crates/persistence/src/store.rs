use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, atomic::AtomicUsize},
};

use crate::identity::{Grammar, Request};
use heed::{Env, EnvOpenOptions, WithoutTls, types::Bytes};

pub(crate) type Database = heed::Database<Bytes, Bytes>;

const SCHEMA: &[u8] = b"tree-squatter-persistence owned prototype 2";

pub(crate) struct Store {
    pub(crate) env: Env<WithoutTls>,
    pub(crate) paths: Database,
    pub(crate) sources: Database,
    pub(crate) trees: Database,
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
        let grammar = Grammar::new(language.clone(), GrammarFingerprint([42; 32]));
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let native = parser.parse(b"[1]", None).unwrap();
        let tree = tree_sitter_squatter::Tree::pack(&native)
            .unwrap()
            .repack()
            .unwrap();
        let request = Request::new(b"test.json".to_vec(), b"[1]", &grammar, true);
        let before = store.env.read_txn().unwrap();
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
        let cache = root.join(".tree-squatter");
        match fs::create_dir(&cache) {
            Ok(()) => {
                File::open(root)?.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
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
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
        }
        let lock_file = options.open(anchor.join("cooperation.lock"))?;
        let work = crate::work::WorkLocks::new(lock_file.try_clone()?);
        let writer = Mutex::new(lock_file);
        let Some(guard) = gate(&writer)? else {
            return Err(
                io::Error::new(io::ErrorKind::WouldBlock, "cache initialization busy").into(),
            );
        };
        let existed = anchor.join("data.mdb").exists();
        // Cooperating writers, trusted local directory, native locking/sync,
        // one retained environment per inode, and no resizing of live mappings.
        let env = unsafe {
            EnvOpenOptions::new()
                .read_txn_without_tls()
                .max_dbs(5)
                .max_readers(256)
                .map_size(map_size)
                .open(&anchor)?
        };
        // Open all handles and initialize schema in one admitted transaction.
        // Committing also publishes DBI metadata for subsequent transactions.
        let mut tx = env.write_txn()?;
        let meta: Database = match env.open_database(&tx, Some("meta"))? {
            Some(db) => db,
            None if !existed => env.create_database(&mut tx, Some("meta"))?,
            None => return Err(CacheError::IncompatibleSchema),
        };
        match meta.get(&tx, b"schema")? {
            Some(value) if value == SCHEMA => (),
            Some(_) => return Err(CacheError::IncompatibleSchema),
            None => meta.put(&mut tx, b"schema", SCHEMA)?,
        }
        let paths = env.create_database(&mut tx, Some("paths"))?;
        let sources = env.create_database(&mut tx, Some("sources"))?;
        let trees = env.create_database(&mut tx, Some("trees"))?;
        let current = env.create_database(&mut tx, Some("current"))?;
        tx.commit()?;
        directory.sync_all()?;
        drop(guard);
        let store = Arc::new(Self {
            env,
            paths,
            sources,
            trees,
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

    pub fn get(
        &self,
        request: &Request,
        source: &[u8],
        grammar: &Grammar,
    ) -> Option<tree_sitter_squatter::Tree> {
        let tx = self.env.read_txn().ok()?;
        if self.paths.get(&tx, &request.source_key[..32]).ok()?? != request.path
            || self.sources.get(&tx, &request.source_key).ok()?? != source
        {
            return None;
        }
        let value = self.trees.get(&tx, &request.tree_key).ok()??;
        let slab = request.decode(value)?;
        // Safety validation does not reconstruct auxiliary index membership.
        let tree =
            tree_sitter_squatter::Tree::from_bytes_safety_checked(&grammar.language, slab).ok()?;
        if tree
            .root_node()
            .preorder()
            .any(|node| node.end_byte() > source.len())
        {
            return None;
        }
        Some(tree)
    }

    pub fn publish(
        &self,
        request: &Request,
        source: &[u8],
        tree: &tree_sitter_squatter::Tree,
        grammar: &Grammar,
        cancelled: impl Fn() -> bool,
    ) -> Result<WriteOutcome, CacheError> {
        let value = request.encode(tree.as_bytes());
        if cancelled() {
            return Err(CacheError::Cancelled);
        }
        let Some(_guard) = gate(&self.writer)? else {
            return Ok(WriteOutcome::Busy);
        };
        if self.get(request, source, grammar).is_some() {
            return Ok(WriteOutcome::AlreadyPresent);
        }
        let mut tx = self.env.write_txn()?;
        if let Some(path) = self.paths.get(&tx, &request.source_key[..32])?
            && path != request.path
        {
            return Err(CacheError::PathCollision);
        }
        self.paths
            .put(&mut tx, &request.source_key[..32], &request.path)?;
        // All records become visible in the same durable transaction.
        if self.sources.get(&tx, &request.source_key)? != Some(source) {
            self.sources.put(&mut tx, &request.source_key, source)?;
        }
        self.trees.put(&mut tx, &request.tree_key, &value)?;
        self.current
            .put(&mut tx, &request.source_key[..32], &request.source_key)?;
        if cancelled() {
            return Err(CacheError::Cancelled);
        }
        tx.commit()?;
        Ok(WriteOutcome::Published)
    }
}
