mod common;
use common::{grammar, load};

use std::{fs, path::Path, sync::atomic::AtomicBool};
use tree_squatter_persistence::*;

fn finish(work: &mut Maintenance) -> usize {
    let mut deleted = 0;
    for _ in 0..100 {
        let progress = work.step(1, None).unwrap();
        assert!(progress.examined <= 1);
        deleted += progress.deleted;
        if progress.state == MaintenanceState::Complete {
            return deleted;
        }
        assert_eq!(progress.state, MaintenanceState::More);
    }
    panic!("maintenance did not finish")
}

#[test]
fn bounded_cleanup_preserves_current_variants_and_old_readers() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.json");
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    fs::write(&path, "[1]").unwrap();
    let reader = load(&cache);
    fs::write(&path, "[2]").unwrap();
    load(&cache);
    fs::write(&path, "[3]").unwrap();
    let current = load(&cache);
    let other = Persistence::open(
        root.path(),
        Options {
            symbol_presence: false,
            ..Options::default()
        },
    )
    .unwrap();
    load(&other);
    let mut work = current.maintenance().unwrap();
    assert_eq!(work.step(0, None).unwrap().examined, 0);
    assert!(matches!(
        work.step(1, Some(&AtomicBool::new(true))),
        Err(CacheError::Cancelled)
    ));
    assert_eq!(finish(&mut work), 4); // two old trees and their two source records
    assert!(load(&cache).cache_hit());
    assert!(load(&other).cache_hit());
    assert_eq!(reader.source(), b"[1]");
    assert_eq!(
        reader.tree().root_node().named_child(0).unwrap().kind(),
        "array"
    );
    fs::write(&path, "[1]").unwrap();
    assert!(!load(&cache).cache_hit());
}

#[test]
fn stale_cleanup_stops_and_late_writer_restores_complete_records() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.json");
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    fs::write(&path, "1").unwrap();
    let deferred = cache
        .load_with_options(
            Path::new("file.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
            LoadOptions {
                write: WritePolicy::Deferred,
                cancellation: None,
            },
        )
        .unwrap();
    load(&cache);
    let old = load(&cache);
    let mut stale = old.maintenance().unwrap();
    fs::write(&path, "2").unwrap();
    let new = load(&cache);
    assert_eq!(
        stale.step(1, None).unwrap().state,
        MaintenanceState::Superseded
    );
    assert_eq!(finish(&mut new.maintenance().unwrap()), 2);
    deferred.pending_write.unwrap().publish().unwrap();
    fs::write(&path, "1").unwrap();
    assert!(load(&cache).cache_hit());
}

#[test]
fn sweep_finds_deleted_sources_without_loading_them() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.json");
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    fs::write(&path, "true").unwrap();
    load(&cache);
    fs::remove_file(&path).unwrap();
    let mut sweep = cache.sweep_missing().unwrap();
    assert!(matches!(
        sweep.step(1, Some(&AtomicBool::new(true))),
        Err(CacheError::Cancelled)
    ));
    let mut deleted = 0;
    let mut complete = false;
    for _ in 0..20 {
        let result = sweep.step(1, None).unwrap();
        assert!(result.examined <= 1);
        deleted += result.deleted;
        if result.state == MaintenanceState::Complete {
            complete = true;
            break;
        }
    }
    assert!(complete);
    assert_eq!(deleted, 2);
    assert!(
        cache
            .maintenance_missing(Path::new("file.json"))
            .unwrap()
            .is_none()
    );
    fs::write(&path, "true").unwrap();
    assert!(!load(&cache).cache_hit());
    assert_eq!(cache.check_stale_readers().unwrap(), 0);
}

#[test]
fn recreation_cancels_missing_file_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.json");
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    fs::write(&path, "0").unwrap();
    load(&cache);
    fs::remove_file(&path).unwrap();
    let mut work = cache
        .maintenance_missing(Path::new("file.json"))
        .unwrap()
        .unwrap();
    fs::write(&path, "0").unwrap();
    assert_eq!(
        work.step(1, None).unwrap().state,
        MaintenanceState::Superseded
    );
    assert!(load(&cache).cache_hit());
}
