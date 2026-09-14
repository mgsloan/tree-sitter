//! Read transactions are used only during construction, then sealed as storage.
//! There is no fabricated Rust lifetime, reset/renew API, or shared LMDB handle.
use crate::{
    identity::{Grammar, Request},
    store::Store,
};
use std::{
    ptr::NonNull,
    sync::{Arc, atomic::Ordering},
};
use tree_sitter_squatter::{BackedTree, StableSlab, Tree};

// Leave most of the 256 environment reader slots available for short operations.
pub(crate) const MAX_BACKED_READERS: usize = 32;

struct Permit(Arc<Store>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.backed_readers.fetch_sub(1, Ordering::Relaxed);
    }
}

struct Snapshot {
    raw: NonNull<lmdb_sys::MDB_txn>,
    // Keeps the environment open until after abort, and releases admission last.
    _permit: Permit,
}
impl Snapshot {
    fn open(store: &Arc<Store>) -> Option<Self> {
        store
            .backed_readers
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < MAX_BACKED_READERS).then_some(count + 1)
            })
            .ok()?;
        let permit = Permit(store.clone());
        let mut raw = std::ptr::null_mut();
        // Store opens with MDB_NOTLS. Read-only transactions may move between
        // threads if their calls are serialized. This handle is private and is
        // only called through exclusive construction access, then once at drop.
        let status = unsafe {
            lmdb_sys::mdb_txn_begin(
                store.env.env(),
                std::ptr::null_mut(),
                lmdb_sys::MDB_RDONLY,
                &mut raw,
            )
        };
        if status != 0 {
            return None;
        }
        Some(Self {
            raw: NonNull::new(raw).expect("LMDB returned a null successful transaction"),
            _permit: permit,
        })
    }

    fn get(&mut self, db: lmdb::Database, key: &[u8]) -> Option<&[u8]> {
        let mut key = lmdb_sys::MDB_val {
            mv_size: key.len(),
            mv_data: key.as_ptr().cast_mut().cast(),
        };
        let mut value = lmdb_sys::MDB_val {
            mv_size: 0,
            mv_data: std::ptr::null_mut(),
        };
        // LMDB does not modify input keys. Returned read-only storage lives until
        // this transaction ends; the returned borrow prevents concurrent calls.
        let status =
            unsafe { lmdb_sys::mdb_get(self.raw.as_ptr(), db.dbi(), &mut key, &mut value) };
        if status != 0 {
            return None;
        }
        if value.mv_size == 0 {
            return Some(&[]);
        }
        Some(unsafe { std::slice::from_raw_parts(value.mv_data.cast(), value.mv_size) })
    }
}
impl Drop for Snapshot {
    fn drop(&mut self) {
        // No descriptor/borrow can remain: Snapshot is owned exclusively by the
        // StableSlab, which BackedTree drops after its native descriptor.
        unsafe { lmdb_sys::mdb_txn_abort(self.raw.as_ptr()) };
    }
}

struct SnapshotSlab {
    pointer: NonNull<u8>,
    length: usize,
    _snapshot: Snapshot,
}
// After construction, no LMDB calls occur until exclusive final destruction.
// Readers only inspect immutable mapped pages pinned by this read transaction.
// MDB_NOTLS permits final abort on another thread. The environment is retained
// and never resized; neither a transaction pointer nor mutable bytes are exposed.
unsafe impl Send for SnapshotSlab {}
unsafe impl Sync for SnapshotSlab {}
unsafe impl StableSlab for SnapshotSlab {
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.length) }
    }
}

pub(crate) fn get(
    store: &Arc<Store>,
    request: &Request,
    source: &[u8],
    grammar: &Grammar,
) -> Option<BackedTree> {
    let mut snapshot = Snapshot::open(store)?;
    if snapshot.get(store.paths, &request.source_key[..32])? != request.path
        || snapshot.get(store.sources, &request.source_key)? != source
    {
        return None;
    }
    let slab = request.decode(snapshot.get(store.trees, &request.tree_key)?)?;
    let pointer = NonNull::new(slab.as_ptr().cast_mut())?;
    let length = slab.len();
    let owner = SnapshotSlab {
        pointer,
        length,
        _snapshot: snapshot,
    };
    // The native loader checks the actual address, not merely the envelope's
    // offset. Misaligned values release their snapshot and use the owned path.
    let tree = Tree::from_owned_slab(&grammar.language, owner).ok()?;
    if tree
        .root_node()
        .preorder()
        .any(|node| node.end_byte() > source.len())
    {
        return None;
    }
    Some(tree)
}
