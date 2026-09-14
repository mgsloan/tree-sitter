use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

use crate::identity::{Grammar, Request};
use lmdb::{Database, DatabaseFlags, Environment, EnvironmentFlags, Transaction, WriteFlags};

const SCHEMA: &[u8] = b"tree-squatter-persistence owned prototype 2";

pub(crate) struct Store {
    pub(crate) env: Environment,
    pub(crate) paths: Database,
    pub(crate) sources: Database,
    pub(crate) trees: Database,
    pub(crate) current: Database,
    pub(crate) writer: Mutex<File>,
    work: crate::work::WorkLocks,
    // Retain the directory behind /proc/self/fd when opening on Linux.
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
        let before = store.env.begin_ro_txn().unwrap();
        assert_eq!(
            store
                .publish(&request, b"[1]", &tree, &grammar, || false)
                .unwrap(),
            WriteOutcome::Published
        );
        assert_eq!(
            before.get(store.sources, &request.source_key),
            Err(lmdb::Error::NotFound)
        );
        assert_eq!(
            before.get(store.trees, &request.tree_key),
            Err(lmdb::Error::NotFound)
        );
        let after = store.env.begin_ro_txn().unwrap();
        assert_eq!(
            after.get(store.sources, &request.source_key).unwrap(),
            b"[1]"
        );
        assert!(after.get(store.trees, &request.tree_key).is_ok());
        drop(after);
        drop(before);

        let guard = gate(&store.writer).unwrap().unwrap();
        let mut tx = store.env.begin_rw_txn().unwrap();
        tx.put(
            store.trees,
            &request.tree_key,
            &b"broken",
            WriteFlags::empty(),
        )
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
}

#[derive(Debug)]
pub enum CacheError {
    Io(io::Error),
    Database(lmdb::Error),
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
impl From<lmdb::Error> for CacheError {
    fn from(error: lmdb::Error) -> Self {
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
        let env = Environment::new()
            .set_max_dbs(5)
            .set_max_readers(256)
            .set_map_size(map_size)
            .set_flags(EnvironmentFlags::NO_TLS)
            .open(&anchor)?;
        let meta = match env.open_db(Some("meta")) {
            Ok(db) => db,
            Err(lmdb::Error::NotFound) if !existed => {
                env.create_db(Some("meta"), DatabaseFlags::empty())?
            }
            Err(lmdb::Error::NotFound) => return Err(CacheError::IncompatibleSchema),
            Err(error) => return Err(error.into()),
        };
        let mut tx = env.begin_rw_txn()?;
        match tx.get(meta, &b"schema") {
            Ok(value) if value == SCHEMA => (),
            Ok(_) => return Err(CacheError::IncompatibleSchema),
            Err(lmdb::Error::NotFound) => tx.put(meta, &b"schema", &SCHEMA, WriteFlags::empty())?,
            Err(error) => return Err(error.into()),
        }
        tx.commit()?;
        let paths = env.create_db(Some("paths"), DatabaseFlags::empty())?;
        let sources = env.create_db(Some("sources"), DatabaseFlags::empty())?;
        let trees = env.create_db(Some("trees"), DatabaseFlags::empty())?;
        let current = env.create_db(Some("current"), DatabaseFlags::empty())?;
        directory.sync_all()?;
        drop(guard);
        let store = Arc::new(Self {
            env,
            paths,
            sources,
            trees,
            current,
            writer,
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
        let tx = self.env.begin_ro_txn().ok()?;
        if tx.get(self.paths, &&request.source_key[..32]).ok()? != request.path
            || tx.get(self.sources, &request.source_key).ok()? != source
        {
            return None;
        }
        let value = tx.get(self.trees, &request.tree_key).ok()?;
        let slab = request.decode(value)?;
        // Initial milestone retains the existing stricter checked loader until
        // its safety-only split is audited. No checksum is introduced.
        let tree = tree_sitter_squatter::Tree::from_bytes(&grammar.language, slab).ok()?;
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
        let mut tx = self.env.begin_rw_txn()?;
        match tx.get(self.paths, &&request.source_key[..32]) {
            Ok(path) if path != request.path => return Err(CacheError::PathCollision),
            Ok(_) | Err(lmdb::Error::NotFound) => (),
            Err(error) => return Err(error.into()),
        }
        tx.put(
            self.paths,
            &&request.source_key[..32],
            &request.path,
            WriteFlags::empty(),
        )?;
        // The three records become visible in the same durable transaction.
        if tx.get(self.sources, &request.source_key).ok() != Some(source) {
            tx.put(
                self.sources,
                &request.source_key,
                &source,
                WriteFlags::empty(),
            )?;
        }
        tx.put(self.trees, &request.tree_key, &value, WriteFlags::empty())?;
        tx.put(
            self.current,
            &&request.source_key[..32],
            &request.source_key,
            WriteFlags::empty(),
        )?;
        if cancelled() {
            return Err(CacheError::Cancelled);
        }
        tx.commit()?;
        Ok(WriteOutcome::Published)
    }
}
