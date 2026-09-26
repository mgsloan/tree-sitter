mod common;
use common::{language, load};

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
fn stale_cleanup_stops_and_late_writer_cannot_restore_retired_records() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.json");
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    fs::write(&path, "1").unwrap();
    let deferred = cache
        .load_with_options(
            Path::new("file.json"),
            &language(),
            &mut tree_sitter::Parser::new(),
            LoadOptions {
                pack: tree_sitter_squatter::PackOptions::default(),
                write: WritePolicy::Deferred,
                cancel: None,
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
    assert_eq!(
        deferred.pending_write.unwrap().publish().unwrap(),
        WriteOutcome::AlreadyPresent
    );
    fs::write(&path, "1").unwrap();
    assert!(!load(&cache).cache_hit());
}

#[test]
fn missing_cleanup_rejects_deferred_first_writers_across_recreations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.json");
    let cache = Persistence::open(root.path(), Options::default()).unwrap();

    for _ in 0..3 {
        fs::write(&path, "1").unwrap();
        let deferred = cache
            .load_with_options(
                Path::new("file.json"),
                &language(),
                &mut tree_sitter::Parser::new(),
                LoadOptions {
                    write: WritePolicy::Deferred,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!deferred.file.cache_hit());
        let pending = deferred.pending_write.unwrap();
        assert!(!load(&cache).cache_hit());
        assert!(load(&cache).cache_hit());

        fs::remove_file(&path).unwrap();
        let mut cleanup = cache
            .maintenance_missing(Path::new("file.json"))
            .unwrap()
            .unwrap();
        assert_eq!(finish(&mut cleanup), 2);
        assert_eq!(pending.publish().unwrap(), WriteOutcome::AlreadyPresent);
        assert!(
            cache
                .maintenance_missing(Path::new("file.json"))
                .unwrap()
                .is_none()
        );
    }
    fs::write(&path, "1").unwrap();
    assert!(!load(&cache).cache_hit());
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

#[test]
fn sidecars_can_be_evicted_independently_of_core_and_readers() {
    for read in [ReadPolicy::Owned, ReadPolicy::PreferTransactionBacked] {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("file.json"),
            format!("[{}0]", "1,\n".repeat(4096)),
        )
        .unwrap();
        let cache = Persistence::open(
            root.path(),
            Options {
                read,
                ..Options::default()
            },
        )
        .unwrap();
        load(&cache);
        let reader = load(&cache);
        assert_eq!(
            reader.transaction_backed(),
            read == ReadPolicy::PreferTransactionBacked
        );
        let core = reader.tree().as_bytes().to_vec();
        let presence = reader.tree().presence_cache().unwrap().as_bytes().to_vec();
        let points = reader.tree().point_data().unwrap().as_bytes().to_vec();

        for kind in [SidecarKind::Presence, SidecarKind::Points] {
            assert!(matches!(
                reader.evict_sidecar(kind, Some(&AtomicBool::new(true))),
                Err(CacheError::Cancelled)
            ));
            assert_eq!(
                reader.evict_sidecar(kind, None).unwrap(),
                EvictionOutcome::Evicted
            );
            assert_eq!(
                reader.evict_sidecar(kind, None).unwrap(),
                EvictionOutcome::Absent
            );

            let without = cache
                .load_with_options(
                    Path::new("file.json"),
                    &language(),
                    &mut tree_sitter::Parser::new(),
                    LoadOptions {
                        pack: tree_sitter_squatter::PackOptions {
                            symbol_presence: kind != SidecarKind::Presence,
                            points: kind != SidecarKind::Points,
                            ..Default::default()
                        },
                        write: WritePolicy::Deferred,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert!(without.file.cache_hit());
            assert!(without.pending_write.is_none());
            assert_eq!(without.file.tree().as_bytes(), core);

            let rebuilt = cache
                .load_with_options(
                    Path::new("file.json"),
                    &language(),
                    &mut tree_sitter::Parser::new(),
                    LoadOptions {
                        write: WritePolicy::Deferred,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert!(rebuilt.file.cache_hit());
            assert_eq!(rebuilt.file.tree().as_bytes(), core);
            assert_eq!(
                rebuilt.file.tree().presence_cache().unwrap().as_bytes(),
                presence
            );
            assert_eq!(rebuilt.file.tree().point_data().unwrap().as_bytes(), points);
            assert_eq!(
                rebuilt.pending_write.unwrap().publish().unwrap(),
                WriteOutcome::Published
            );
            assert_eq!(reader.tree().as_bytes(), core);
            assert_eq!(reader.tree().presence_cache().unwrap().as_bytes(), presence);
            assert_eq!(reader.tree().point_data().unwrap().as_bytes(), points);
        }
    }
}
