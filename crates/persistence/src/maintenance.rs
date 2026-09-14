use crate::{
    CacheError,
    identity::Request,
    store::{Store, gate},
};
use lmdb::{Cursor, Database, Transaction};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Each call inspects/deletes at most the requested number of candidate records.
/// No LMDB transaction or cursor survives a call or an executor yield.
pub struct Maintenance {
    store: Arc<Store>,
    path_id: [u8; 32],
    path: Vec<u8>,
    expected_current: Vec<u8>,
    keep: Option<[u8; 72]>,
    missing_path: Option<PathBuf>,
    phase: Phase,
    next: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Trees,
    Sources,
    Finish,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceState {
    More,
    Complete,
    Busy,
    Superseded,
}

#[derive(Clone, Copy, Debug)]
pub struct MaintenanceProgress {
    pub state: MaintenanceState,
    pub examined: usize,
    pub deleted: usize,
}

pub(crate) fn first_key(
    tx: &impl Transaction,
    db: Database,
    start: &[u8],
    prefix: &[u8],
) -> Result<Option<Vec<u8>>, CacheError> {
    let cursor = tx.open_ro_cursor(db)?;
    let positioned = if start.is_empty() {
        cursor.get(None, None, lmdb_sys::MDB_FIRST)
    } else {
        cursor.get(Some(start), None, lmdb_sys::MDB_SET_RANGE)
    };
    match positioned {
        Ok(_) => (),
        Err(lmdb::Error::NotFound) => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    match cursor.get(None, None, lmdb_sys::MDB_GET_CURRENT) {
        Ok((Some(key), _)) if key.starts_with(prefix) => Ok(Some(key.to_vec())),
        Ok(_) | Err(lmdb::Error::NotFound) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Finds cache paths that no longer exist, without requiring another load of them.
pub struct MissingSweep {
    store: Arc<Store>,
    root: PathBuf,
    next: Vec<u8>,
    pending: Option<Maintenance>,
    complete: bool,
}
impl MissingSweep {
    pub(crate) fn new(store: Arc<Store>, root: PathBuf) -> Self {
        Self {
            store,
            root,
            next: vec![],
            pending: None,
            complete: false,
        }
    }

    pub fn step(
        &mut self,
        budget: usize,
        cancellation: Option<&AtomicBool>,
    ) -> Result<MaintenanceProgress, CacheError> {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(CacheError::Cancelled);
        }
        let progress = |state, examined| MaintenanceProgress {
            state,
            examined,
            deleted: 0,
        };
        if self.complete {
            return Ok(progress(MaintenanceState::Complete, 0));
        }
        if budget == 0 {
            return Ok(progress(MaintenanceState::More, 0));
        }
        if let Some(pending) = &mut self.pending {
            let mut result = pending.step(budget, cancellation)?;
            if matches!(
                result.state,
                MaintenanceState::Complete | MaintenanceState::Superseded
            ) {
                self.pending = None;
                result.state = MaintenanceState::More;
            }
            return Ok(result);
        }
        let tx = self.store.env.begin_ro_txn()?;
        let Some(key) = first_key(&tx, self.store.paths, &self.next, &[])? else {
            self.complete = true;
            return Ok(progress(MaintenanceState::Complete, 0));
        };
        let encoded = tx.get(self.store.paths, &key)?.to_vec();
        drop(tx);
        if let Some(path) = decode_path(&encoded) {
            let absolute = self.root.join(path);
            match std::fs::metadata(&absolute) {
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.pending = Maintenance::missing(self.store.clone(), encoded, absolute)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.next = key;
        self.next.push(0);
        Ok(progress(MaintenanceState::More, 1))
    }
}

fn decode_path(bytes: &[u8]) -> Option<PathBuf> {
    let (&tag, encoded) = bytes.split_first()?;
    if tag != u8::from(cfg!(windows)) {
        return None;
    }
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(encoded.to_vec()))
    };
    #[cfg(not(unix))]
    let path = PathBuf::from(std::str::from_utf8(encoded).ok()?);
    let (clean, roundtrip) = crate::identity::path(&path).ok()?;
    (roundtrip == bytes).then_some(clean)
}

impl Maintenance {
    pub(crate) fn keep(store: Arc<Store>, request: &Request) -> Self {
        let path_id = request.source_key[..32].try_into().unwrap();
        Self {
            store,
            path_id,
            path: request.path.clone(),
            expected_current: request.source_key.to_vec(),
            keep: Some(request.source_key),
            missing_path: None,
            phase: Phase::Trees,
            next: path_id.to_vec(),
        }
    }

    pub(crate) fn missing(
        store: Arc<Store>,
        path: Vec<u8>,
        absolute: PathBuf,
    ) -> Result<Option<Self>, CacheError> {
        let path_id = crate::identity::digest("tree-squatter path v1", &path);
        let tx = store.env.begin_ro_txn()?;
        let expected_current = match tx.get(store.current, &path_id) {
            Ok(value) if value.len() == 72 => value.to_vec(),
            Ok(_) | Err(lmdb::Error::NotFound) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if tx.get(store.paths, &path_id)? != path {
            return Err(CacheError::PathCollision);
        }
        drop(tx);
        Ok(Some(Self {
            store,
            path_id,
            path,
            expected_current,
            keep: None,
            missing_path: Some(absolute),
            phase: Phase::Trees,
            next: path_id.to_vec(),
        }))
    }

    pub fn step(
        &mut self,
        budget: usize,
        cancellation: Option<&AtomicBool>,
    ) -> Result<MaintenanceProgress, CacheError> {
        self.step_cancelled(budget, || {
            cancellation.is_some_and(|flag| flag.load(Ordering::Relaxed))
        })
    }

    fn step_cancelled(
        &mut self,
        budget: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<MaintenanceProgress, CacheError> {
        let progress = |state, examined, deleted| MaintenanceProgress {
            state,
            examined,
            deleted,
        };
        if cancelled() {
            return Err(CacheError::Cancelled);
        }
        if self.phase == Phase::Done {
            return Ok(progress(MaintenanceState::Complete, 0, 0));
        }
        if budget == 0 {
            return Ok(progress(MaintenanceState::More, 0, 0));
        }
        // Do filesystem work outside the write transaction. Permission errors
        // propagate rather than being interpreted as absence.
        if let Some(path) = &self.missing_path {
            match std::fs::metadata(path) {
                Ok(_) => {
                    self.phase = Phase::Done;
                    return Ok(progress(MaintenanceState::Superseded, 0, 0));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let Some(_guard) = gate(&self.store.writer)? else {
            return Ok(progress(MaintenanceState::Busy, 0, 0));
        };
        let mut tx = self.store.env.begin_rw_txn()?;
        if tx.get(self.store.current, &self.path_id).ok() != Some(self.expected_current.as_slice())
            || tx.get(self.store.paths, &self.path_id).ok() != Some(self.path.as_slice())
        {
            self.phase = Phase::Done;
            return Ok(progress(MaintenanceState::Superseded, 0, 0));
        }
        // Continuation advances only after commit: cancellation/failed commits
        // retry the same candidates rather than silently skipping them.
        let mut phase = self.phase;
        let mut next = self.next.clone();
        let mut examined = 0;
        let mut deleted = 0;
        while examined < budget && phase != Phase::Done {
            if cancelled() {
                return Err(CacheError::Cancelled);
            }
            match phase {
                Phase::Trees | Phase::Sources => {
                    let db = if phase == Phase::Trees {
                        self.store.trees
                    } else {
                        self.store.sources
                    };
                    let Some(key) = first_key(&tx, db, &next, &self.path_id)? else {
                        phase = if phase == Phase::Trees {
                            Phase::Sources
                        } else {
                            Phase::Finish
                        };
                        next = self.path_id.to_vec();
                        continue;
                    };
                    examined += 1;
                    let expected_len = if phase == Phase::Trees { 104 } else { 72 };
                    let retain = key.len() != expected_len
                        || self.keep.as_ref().is_some_and(|keep| key.starts_with(keep));
                    // A late publisher can insert keys before our continuation.
                    // Never delete a source if any tree still references it.
                    let referenced = phase == Phase::Sources
                        && first_key(&tx, self.store.trees, &key, &key)?.is_some();
                    if !retain && !referenced {
                        tx.del(db, &key, None)?;
                        deleted += 1;
                    }
                    next = key;
                    next.push(0);
                }
                Phase::Finish => {
                    examined += 1;
                    if self.keep.is_none()
                        && first_key(&tx, self.store.trees, &self.path_id, &self.path_id)?.is_none()
                        && first_key(&tx, self.store.sources, &self.path_id, &self.path_id)?
                            .is_none()
                    {
                        tx.del(self.store.current, &self.path_id, None)?;
                        tx.del(self.store.paths, &self.path_id, None)?;
                    }
                    phase = Phase::Done;
                }
                Phase::Done => unreachable!(),
            }
        }
        if cancelled() {
            return Err(CacheError::Cancelled);
        }
        tx.commit()?;
        self.phase = phase;
        self.next = next;
        Ok(progress(
            if phase == Phase::Done {
                MaintenanceState::Complete
            } else {
                MaintenanceState::More
            },
            examined,
            deleted,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Grammar, GrammarFingerprint, Options, Persistence};

    #[test]
    fn cancellation_rolls_back_batch_and_continuation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file.json");
        let cache = Persistence::open(root.path(), Options::default()).unwrap();
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let grammar = Grammar::new(language, GrammarFingerprint([42; 32]));
        let mut parser = tree_sitter::Parser::new();
        std::fs::write(&path, "1").unwrap();
        cache
            .load(std::path::Path::new("file.json"), &grammar, &mut parser)
            .unwrap();
        std::fs::write(&path, "2").unwrap();
        let newest = cache
            .load(std::path::Path::new("file.json"), &grammar, &mut parser)
            .unwrap();
        let mut work = newest.maintenance().unwrap();
        let checks = std::cell::Cell::new(0);
        // Start and two tree candidates pass; cancel after an old tree has
        // been deleted inside the uncommitted transaction.
        assert!(matches!(
            work.step_cancelled(100, || {
                let previous = checks.get();
                checks.set(previous + 1);
                previous >= 3
            }),
            Err(CacheError::Cancelled)
        ));
        std::fs::write(&path, "1").unwrap();
        assert!(
            cache
                .load(std::path::Path::new("file.json"), &grammar, &mut parser)
                .unwrap()
                .cache_hit()
        );
        let result = work.step(100, None).unwrap();
        assert_eq!(result.state, MaintenanceState::Complete);
        assert_eq!(result.deleted, 2);
    }
}
