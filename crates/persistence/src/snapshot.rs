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
    // Heed owns the environment and aborts on drop. This 'static lifetime is
    // supplied by its owning API, never extended from a borrowed transaction.
    tx: heed::RoTxn<'static, heed::WithoutTls>,
    // Field order releases the transaction before its admission permit.
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
    _snapshot: Snapshot,
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
    grammar: &Grammar,
) -> Option<BackedTree> {
    let snapshot = Snapshot::open(store)?;
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
