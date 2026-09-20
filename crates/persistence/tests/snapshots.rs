#[cfg(feature = "rust-core")]
extern crate tree_squatter_rust as tree_sitter_squatter;

mod common;
use common::{ChildProcess, load};

use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
};
use tree_squatter_persistence::*;

#[test]
#[ignore = "subprocess helper"]
fn child_snapshot_holder() {
    let Some(root) = std::env::var_os("TSQ_SNAPSHOT_ROOT") else {
        return;
    };
    let cache = cache(Path::new(&root));
    let reader = load(&cache);
    assert!(reader.transaction_backed());
    common::wait_until_killed();
    drop(reader);
}

#[test]
fn crashed_snapshot_reader_slot_is_reclaimable() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), source(1)).unwrap();
    let cache = cache(root.path());
    load(&cache);
    let child = ChildProcess::start("child_snapshot_holder", "TSQ_SNAPSHOT_ROOT", root.path());
    child.kill();
    assert_eq!(cache.check_stale_readers().unwrap(), 1);
    assert_eq!(cache.check_stale_readers().unwrap(), 0);
    assert!(load(&cache).transaction_backed());
}

fn source(value: u8) -> String {
    format!("[{}0]", format!("{value},").repeat(4096))
}
fn cache(root: &Path) -> Persistence {
    Persistence::open(
        root,
        Options {
            read: ReadPolicy::PreferTransactionBacked,
            ..Options::default()
        },
    )
    .unwrap()
}

#[test]
fn mapped_readers_survive_concurrent_publication_and_cleanup() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), source(1)).unwrap();
    let cache = cache(root.path());
    assert!(!load(&cache).cache_hit());
    let reader = load(&cache);
    assert!(reader.cache_hit());
    assert!(reader.transaction_backed());
    let expected = reader.tree().as_bytes().to_vec();
    let expected_nodes = reader.tree().root_node().preorder().count();
    let barrier = Arc::new(Barrier::new(3));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let reader = reader.clone();
            let barrier = barrier.clone();
            let expected = expected.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..64 {
                    assert_eq!(reader.tree().as_bytes(), expected);
                    assert_eq!(reader.tree().root_node().preorder().count(), expected_nodes);
                }
                reader
            })
        })
        .collect();
    barrier.wait();
    for value in 2..5 {
        fs::write(root.path().join("file.json"), source(value)).unwrap();
        let current = load(&cache);
        assert!(!current.cache_hit());
        let mut cleanup = current.maintenance().unwrap();
        loop {
            if cleanup.step(8, None).unwrap().state == MaintenanceState::Complete {
                break;
            }
        }
    }
    drop(cache);
    for worker in workers {
        drop(worker.join().unwrap());
    }
    assert_eq!(reader.source(), source(1).as_bytes());
    assert_eq!(reader.tree().as_bytes(), expected);
    // Final transaction abort is allowed on a thread other than its creator.
    std::thread::spawn(move || drop(reader)).join().unwrap();
}

#[test]
fn snapshot_admission_falls_back_and_detach_does_not_revoke_aliases() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), source(1)).unwrap();
    let cache = cache(root.path());
    load(&cache);
    let readers: Vec<_> = (0..32).map(|_| load(&cache)).collect();
    assert!(readers.iter().all(LoadedFile::transaction_backed));
    let fallback = load(&cache);
    assert!(fallback.cache_hit());
    assert!(!fallback.transaction_backed());
    let detached = readers[0].detach().unwrap();
    assert!(!detached.transaction_backed());
    assert_eq!(detached.tree().as_bytes(), readers[0].tree().as_bytes());
    let alias = readers[0].clone();
    drop(readers);
    let extra: Vec<_> = (0..31).map(|_| load(&cache)).collect();
    assert!(extra.iter().all(LoadedFile::transaction_backed));
    assert!(!load(&cache).transaction_backed());
    drop(alias);
    assert!(load(&cache).transaction_backed());
    drop(extra);
    drop(cache);
    assert_eq!(detached.source(), source(1).as_bytes());
}

#[test]
fn default_reads_do_not_pin_transactions() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), source(1)).unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    load(&cache);
    let hit = load(&cache);
    assert!(hit.cache_hit());
    assert!(!hit.transaction_backed());
}
