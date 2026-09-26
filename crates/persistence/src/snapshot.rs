//! Read transactions are used only during construction, then sealed as storage.
//! There is no fabricated Rust lifetime, reset/renew API, or shared LMDB handle.
use crate::{
    identity::{IdentifiedGrammar, Request},
    store::Store,
};
use std::{
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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
    // Heed owns the environment and aborts on drop. This 'static lifetime is
    // supplied by its owning API, never extended from a borrowed transaction.
    tx: heed::RoTxn<'static, heed::WithoutTls>,
    // Field order releases the transaction before its admission permit.
    _permit: Permit,
}
// The transaction is sealed after construction; only immutable mapped bytes
// are shared. Its final drop runs after the last core or sidecar owner.
unsafe impl Sync for Snapshot {}
impl Snapshot {
    fn open(store: &Arc<Store>) -> Option<Self> {
        store
            .backed_readers
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < MAX_BACKED_READERS).then_some(count + 1)
            })
            .ok()?;
        let permit = Permit(store.clone());
        let tx = store.env.clone().static_read_txn().ok()?;
        Some(Self {
            tx,
            _permit: permit,
        })
    }
}

struct SnapshotSlab {
    pointer: NonNull<u8>,
    length: usize,
    _snapshot: Arc<Snapshot>,
}
// After construction, no LMDB calls occur until exclusive final destruction.
// Readers only inspect immutable mapped pages pinned by this read transaction.
// Heed's RoTxn<WithoutTls> is Send, but intentionally not Sync: sharing the
// transaction API is not allowed. Only this sealed owner is Sync, with no calls
// after construction except exclusive final drop. The environment is retained
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
    grammar: &IdentifiedGrammar,
    cancel: Option<&AtomicBool>,
) -> Option<(BackedTree, bool)> {
    let snapshot = Arc::new(Snapshot::open(store)?);
    if store
        .paths
        .get(&snapshot.tx, &request.source_key[..32])
        .ok()??
        != request.path
        || store
            .sources
            .get(&snapshot.tx, &request.source_key)
            .ok()??
            != source
    {
        return None;
    }
    let slab = request.decode(store.trees.get(&snapshot.tx, &request.tree_key).ok()??)?;
    let pointer = NonNull::new(slab.as_ptr().cast_mut())?;
    let length = slab.len();
    let owner = SnapshotSlab {
        pointer,
        length,
        _snapshot: snapshot.clone(),
    };
    // The native loader checks the actual address, not merely the envelope's
    // offset. Misaligned values release their snapshot and use the owned path.
    let mut tree = Tree::from_owned_slab(&grammar.prepared, owner).ok()?;
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
        let loaded = store
            .presence
            .get(&snapshot.tx, &request.tree_key)
            .ok()
            .flatten()
            .and_then(|bytes| {
                let owner = SnapshotSlab {
                    pointer: NonNull::new(bytes.as_ptr().cast_mut())?,
                    length: bytes.len(),
                    _snapshot: snapshot.clone(),
                };
                tree_sitter_squatter::PresenceCache::from_backing(&tree, owner)
                    .ok()
                    .or_else(|| {
                        tree_sitter_squatter::PresenceCache::copy_from_bytes(&tree, bytes).ok()
                    })
            });
        complete &= loaded.is_some();
        let cache =
            loaded.or_else(|| tree_sitter_squatter::PresenceCache::build(&tree, cancel).ok())?;
        tree.set_presence_cache(cache).ok()?;
    }
    if request.points {
        let loaded = store
            .points
            .get(&snapshot.tx, &request.tree_key)
            .ok()
            .flatten()
            .and_then(|bytes| {
                let owner = SnapshotSlab {
                    pointer: NonNull::new(bytes.as_ptr().cast_mut())?,
                    length: bytes.len(),
                    _snapshot: snapshot.clone(),
                };
                tree_sitter_squatter::PointData::from_backing(&tree, owner)
                    .ok()
                    .or_else(|| tree_sitter_squatter::PointData::copy_from_bytes(&tree, bytes).ok())
            });
        complete &= loaded.is_some();
        let points = loaded.or_else(|| {
            let line_index = tree_sitter_squatter::LineIndex::new(source).ok()?;
            tree_sitter_squatter::PointData::build(&tree, &line_index, cancel).ok()
        })?;
        tree.set_point_data(points).ok()?;
    }
    Some((tree, complete))
}
