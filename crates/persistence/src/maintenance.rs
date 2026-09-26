use crate::{
    CacheError,
    identity::Request,
    store::{Database, Store, gate},
};
use heed::RoTxn;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidecarKind {
    Presence,
    Points,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvictionOutcome {
    Evicted,
    Absent,
    Busy,
}

impl Store {
    pub(crate) fn evict_sidecar(
        &self,
        request: &Request,
        kind: SidecarKind,
        cancel: Option<&AtomicBool>,
    ) -> Result<EvictionOutcome, CacheError> {
        let check = || {
            if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                Err(CacheError::Cancelled)
            } else {
                Ok(())
            }
        };
        check()?;
        let Some(_guard) = gate(&self.writer)? else {
            return Ok(EvictionOutcome::Busy);
        };
        let database = match kind {
            SidecarKind::Presence => self.presence,
            SidecarKind::Points => self.points,
        };
        let mut tx = self.env.write_txn()?;
        let deleted = database.delete(&mut tx, &request.tree_key)?;
        check()?;
        tx.commit()?;
        Ok(if deleted {
            EvictionOutcome::Evicted
        } else {
            EvictionOutcome::Absent
        })
    }
}

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
    tx: &RoTxn<'_>,
    db: Database,
    start: &[u8],
    prefix: &[u8],
) -> Result<Option<Vec<u8>>, CacheError> {
    let entry = if start.is_empty() {
        db.first(tx)?
    } else {
        db.get_greater_than_or_equal_to(tx, start)?
    };
    Ok(entry
        .filter(|(key, _)| key.starts_with(prefix))
        .map(|(key, _)| key.to_vec()))
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
        cancel: Option<&AtomicBool>,
    ) -> Result<MaintenanceProgress, CacheError> {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
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
            let mut result = pending.step(budget, cancel)?;
            if matches!(
                result.state,
                MaintenanceState::Complete | MaintenanceState::Superseded
            ) {
                self.pending = None;
                result.state = MaintenanceState::More;
            }
            return Ok(result);
        }
        let tx = self.store.env.read_txn()?;
        let Some(key) = first_key(&tx, self.store.paths, &self.next, &[])? else {
            self.complete = true;
            return Ok(progress(MaintenanceState::Complete, 0));
        };
        let encoded = self
            .store
            .paths
            .get(&tx, &key)?
            .expect("key exists in this snapshot")
            .to_vec();
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

pub(crate) fn decode_path(bytes: &[u8]) -> Option<PathBuf> {
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
        let path_id = crate::identity::digest("tree-squatter path v0", &path);
        let tx = store.env.read_txn()?;
        let expected_current = match store.current.get(&tx, &path_id)? {
            Some(value) if value.len() == 72 => value.to_vec(),
            _ => return Ok(None),
        };
        if store.paths.get(&tx, &path_id)? != Some(path.as_slice()) {
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
        cancel: Option<&AtomicBool>,
    ) -> Result<MaintenanceProgress, CacheError> {
        self.step_cancelled(budget, || {
            cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
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
        let mut tx = self.store.env.write_txn()?;
        if self.store.current.get(&tx, &self.path_id)? != Some(self.expected_current.as_slice())
            || self.store.paths.get(&tx, &self.path_id)? != Some(self.path.as_slice())
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
                        db.delete(&mut tx, &key)?;
                        if phase == Phase::Trees {
                            self.store.presence.delete(&mut tx, &key)?;
                            self.store.points.delete(&mut tx, &key)?;
                        }
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
                        // Retain a distinct marker for each retirement so deferred
                        // writers cannot mistake it for their original absent state.
                        let retired = (tx.id() as u64).to_le_bytes();
                        self.store.current.put(&mut tx, &self.path_id, &retired)?;
                        self.store.paths.delete(&mut tx, &self.path_id)?;
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
    use crate::{IdentifiedLanguage, LanguageIdentity, Options, Persistence};

    #[test]
    fn cancellation_rolls_back_batch_and_continuation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file.json");
        let cache = Persistence::open(root.path(), Options::default()).unwrap();
        let tree_sitter_language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let language = IdentifiedLanguage::new(
            tree_sitter_squatter::Language::new(&tree_sitter_language).unwrap(),
            LanguageIdentity::new(&tree_sitter_language, "json"),
        );
        let mut parser = tree_sitter::Parser::new();
        std::fs::write(&path, "1").unwrap();
        cache
            .load(std::path::Path::new("file.json"), &language, &mut parser)
            .unwrap();
        std::fs::write(&path, "2").unwrap();
        let newest = cache
            .load(std::path::Path::new("file.json"), &language, &mut parser)
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
                .load(std::path::Path::new("file.json"), &language, &mut parser)
                .unwrap()
                .cache_hit()
        );
        let result = work.step(100, None).unwrap();
        assert_eq!(result.state, MaintenanceState::Complete);
        assert_eq!(result.deleted, 2);
    }
}
