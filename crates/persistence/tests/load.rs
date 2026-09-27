mod common;
use common::{ChildProcess, language, load};

use std::{
    fs,
    ops::ControlFlow,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tree_squatter_persistence::{
    CACHE_DIRECTORY, LoadError, LoadOptions, Options, Persistence, WriteOutcome, WritePolicy,
};

#[test]
fn miss_hit_and_old_reader_survives_update() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), b"{\"old\": [1, 2]}\r\n").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let first = cache
        .load_with_options(
            Path::new("file.json"),
            &language(),
            &mut tree_sitter::Parser::new(),
            LoadOptions {
                pack: tree_sitter_squatter::PackOptions {
                    initial_group_capacity: 128,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap()
        .file;
    assert!(!first.cache_hit());
    assert!(first.tree().group_capacity() > first.tree().group_count());
    let reader = load(&cache);
    assert!(reader.cache_hit());
    assert_eq!(reader.tree().group_capacity(), reader.tree().group_count());
    assert_eq!(
        reader.tree().as_bytes(),
        first.tree().repack().unwrap().as_bytes()
    );
    assert_eq!(reader.source(), b"{\"old\": [1, 2]}\r\n");
    fs::write(root.path().join("file.json"), b"[false, true]").unwrap();
    let new = load(&cache);
    assert!(!new.cache_hit());
    assert!(load(&cache).cache_hit());
    assert_eq!(
        new.tree()
            .root_node()
            .named_child(tree_sitter_squatter::NamedChildIx::new(0))
            .unwrap()
            .kind(),
        "array"
    );
    assert_eq!(
        reader
            .tree()
            .root_node()
            .named_child(tree_sitter_squatter::NamedChildIx::new(0))
            .unwrap()
            .kind(),
        "object"
    );
    assert_eq!(reader.source(), first.source());
    drop(cache);
    assert!(reader.tree().root_node().preorder().count() > 4);
    let mut files: Vec<_> = fs::read_dir(root.path().join(CACHE_DIRECTORY))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    files.sort();
    assert_eq!(files, ["squat.coop-lock", "squat.mdb", "squat.mdb-lock"]);
}

#[test]
fn deferred_disabled_and_cancelled_publication() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "123").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let mut parser = tree_sitter::Parser::new();
    let result = cache
        .load_with_options(
            Path::new("file.json"),
            &language(),
            &mut parser,
            LoadOptions {
                pack: tree_sitter_squatter::PackOptions::default(),
                write: WritePolicy::Deferred,
                cancel: None,
            },
        )
        .unwrap();
    let pending = result.pending_write.unwrap();
    let disabled = || {
        cache
            .load_with_options(
                Path::new("file.json"),
                &language(),
                &mut tree_sitter::Parser::new(),
                LoadOptions {
                    pack: tree_sitter_squatter::PackOptions::default(),
                    write: WritePolicy::Disabled,
                    cancel: None,
                },
            )
            .unwrap()
    };
    assert!(!disabled().file.cache_hit());
    assert!(
        pending
            .publish_with_cancellation(&AtomicBool::new(true))
            .is_err()
    );
    assert!(!disabled().file.cache_hit());
    assert_eq!(pending.publish().unwrap(), WriteOutcome::Published);
    assert!(disabled().file.cache_hit());
    assert_eq!(pending.publish().unwrap(), WriteOutcome::AlreadyPresent);
}

#[test]
fn stale_deferred_writer_cannot_create_wrong_hit() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "123").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let old = cache
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
    fs::write(root.path().join("file.json"), "456").unwrap();
    load(&cache);
    old.pending_write.unwrap().publish().unwrap();
    let current = load(&cache);
    assert_eq!(current.source(), b"456");
    assert!(current.cache_hit());
}

#[test]
fn reset_cancelled_parser_and_clear_included_ranges() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[1,2,3]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let mut parser = tree_sitter::Parser::new();
    let tree_sitter_language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    parser.set_language(&tree_sitter_language).unwrap();
    let source = format!("[{}0]", "0,".repeat(100_000));
    let mut cancel = |_: &tree_sitter::ParseState| ControlFlow::Break(());
    assert!(
        parser
            .parse_with_options(
                &mut |i, _| source.as_bytes().get(i..).unwrap_or_default(),
                None,
                Some(tree_sitter::ParseOptions::new().progress_callback(&mut cancel))
            )
            .is_none()
    );
    parser
        .set_included_ranges(&[tree_sitter::Range {
            start_byte: 1,
            end_byte: 2,
            start_point: tree_sitter::Point::new(0, 1),
            end_point: tree_sitter::Point::new(0, 2),
        }])
        .unwrap();
    let result = cache
        .load(Path::new("file.json"), &language(), &mut parser)
        .unwrap();
    assert_eq!(
        result
            .tree()
            .root_node()
            .named_child(tree_sitter_squatter::NamedChildIx::new(0))
            .unwrap()
            .kind(),
        "array"
    );
    assert_eq!(result.tree().root_node().end_byte(), 7);
    assert!(load(&cache).cache_hit());
}

#[test]
fn cancellation_never_creates_entry() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[1]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let cancel = AtomicBool::new(true);
    let result = cache.load_with_options(
        Path::new("file.json"),
        &language(),
        &mut tree_sitter::Parser::new(),
        LoadOptions {
            pack: tree_sitter_squatter::PackOptions::default(),
            write: WritePolicy::Inline,
            cancel: Some(&cancel),
        },
    );
    assert!(matches!(result, Err(LoadError::Cancelled)));
    cancel.store(false, Ordering::Relaxed);
    assert!(!load(&cache).cache_hit());
}

#[test]
fn packing_variants_and_invalid_syntax_are_cacheable() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[\n1,").unwrap();
    let a = Persistence::open(root.path(), Options::default()).unwrap();
    let b = Persistence::open(
        root.path(),
        Options {
            symbol_presence: false,
            ..Options::default()
        },
    )
    .unwrap();
    let c = Persistence::open(
        root.path(),
        Options {
            points: false,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(!load(&a).cache_hit());
    assert!(load(&b).cache_hit());
    let without_points = load(&c);
    assert!(!without_points.cache_hit());
    assert!(!without_points.tree().has_points());
    let root = without_points.tree().root_node();
    assert_eq!(root.start_position().row, 0);
    assert_eq!(root.end_position().row, 0);
    assert_eq!(root.end_position().column, root.end_byte());
    assert!(load(&a).cache_hit());
    assert!(load(&b).cache_hit());
    assert!(load(&c).cache_hit());
    assert!(load(&a).tree().root_node().has_error());
}

#[test]
fn unavailable_cache_and_full_map_fall_back() {
    let blocked = tempfile::tempdir().unwrap();
    fs::write(blocked.path().join("file.json"), "true").unwrap();
    fs::write(blocked.path().join(CACHE_DIRECTORY), "do not change").unwrap();
    let cache = Persistence::open(blocked.path(), Options::default()).unwrap();
    assert!(!load(&cache).cache_hit());
    assert_eq!(
        fs::read(blocked.path().join(CACHE_DIRECTORY)).unwrap(),
        b"do not change"
    );

    let root = tempfile::tempdir().unwrap();
    let source = format!("\"{}\"", "x".repeat(1024 * 1024));
    fs::write(root.path().join("file.json"), &source).unwrap();
    let cache = Persistence::open(
        root.path(),
        Options {
            map_size: 128 * 1024,
            ..Options::default()
        },
    )
    .unwrap();
    let result = cache
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
    assert!(matches!(
        result.pending_write.unwrap().publish(),
        Err(tree_squatter_persistence::CacheError::Database(
            heed::Error::Mdb(heed::MdbError::MapFull)
        ))
    ));
    assert_eq!(result.file.source(), source.as_bytes());
    assert!(!load(&cache).cache_hit());
}

#[test]
fn multiple_owned_readers() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[1,2,3]").unwrap();
    let cache = Arc::new(Persistence::open(root.path(), Options::default()).unwrap());
    load(&cache);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let cache = cache.clone();
            scope.spawn(move || {
                for _ in 0..20 {
                    let file = load(&cache);
                    assert!(file.cache_hit());
                    assert_eq!(file.source(), b"[1,2,3]");
                    assert_eq!(
                        file.tree()
                            .root_node()
                            .named_child(tree_sitter_squatter::NamedChildIx::new(0))
                            .unwrap()
                            .named_child_count()
                            .get(),
                        3
                    );
                }
            });
        }
    });
}

#[cfg(unix)]
#[test]
fn source_symlink_outside_root_and_cache_symlinks() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("source"), "0").unwrap();
    symlink(outside.path().join("source"), root.path().join("file.json")).unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    assert!(!load(&cache).cache_hit());
    assert!(!load(&cache).cache_hit());
    let other = tempfile::tempdir().unwrap();
    fs::write(other.path().join("file.json"), "1").unwrap();
    symlink(outside.path(), other.path().join(CACHE_DIRECTORY)).unwrap();
    let cache = Persistence::open(other.path(), Options::default()).unwrap();
    assert!(!load(&cache).cache_hit());
    assert!(!outside.path().join("squat.mdb").exists());
}

#[test]
fn child_load() {
    let Some(root) = std::env::var_os("TSQ_TEST_PROJECT") else {
        return;
    };
    let cache = Persistence::open(root, Options::default()).unwrap();
    let file = load(&cache);
    let expected = std::env::var("TSQ_TEST_EXPECT_HIT").unwrap() == "yes";
    assert_eq!(file.cache_hit(), expected);
}

#[test]
fn independent_processes_reopen_persisted_contents() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "{\"cross_process\": true}").unwrap();
    for expected in ["no", "yes"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_load", "--nocapture"])
            .env("TSQ_TEST_PROJECT", root.path())
            .env("TSQ_TEST_EXPECT_HIT", expected)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn independent_reader_opens_while_writer_admission_is_held() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[true]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    assert!(!load(&cache).cache_hit());
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join(CACHE_DIRECTORY).join("squat.coop-lock"))
        .unwrap();
    lock.lock().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_load", "--nocapture"])
        .env("TSQ_TEST_PROJECT", root.path())
        .env("TSQ_TEST_EXPECT_HIT", "yes")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "subprocess helper"]
fn child_writer_lock() {
    let Some(path) = std::env::var_os("TSQ_TEST_LOCK") else {
        return;
    };
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    lock.lock().unwrap();
    common::wait_until_killed();
}

#[test]
fn writer_death_releases_admission_without_stale_files() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[true]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let result = cache
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
    let write = result.pending_write.unwrap();
    let child = ChildProcess::start(
        "child_writer_lock",
        "TSQ_TEST_LOCK",
        root.path().join(CACHE_DIRECTORY).join("squat.coop-lock"),
    );
    // Child owns admission, not an LMDB transaction. Parent must not block.
    let outcome = write.publish().unwrap();
    child.kill();
    assert_eq!(outcome, WriteOutcome::Busy);
    assert_eq!(write.publish().unwrap(), WriteOutcome::Published);
    assert!(load(&cache).cache_hit());
}

#[test]
fn worker_context_switches_grammars_and_loads_restored_dictionary() {
    use tree_squatter_persistence::{LoadContext, LoadStep};
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[1, 2]").unwrap();
    fs::write(root.path().join("input.cs"), "class C { int x; }").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let json_language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let c_sharp_language = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
    };
    let json = cache.prepare_language(&json_language, "json").unwrap();
    let c_sharp = cache
        .prepare_language(&c_sharp_language, "c_sharp")
        .unwrap();
    let mut context = LoadContext::default();
    for _ in 0..3 {
        for (language, path, kind) in [
            (&json, "file.json", "document"),
            (&c_sharp, "input.cs", "compilation_unit"),
        ] {
            let result = cache
                .load_with_context(
                    Path::new(path),
                    language,
                    &mut context,
                    LoadOptions {
                        pack: tree_sitter_squatter::PackOptions::default(),
                        write: WritePolicy::Disabled,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert!(!result.file.cache_hit());
            assert_eq!(result.file.tree().root_node().kind(), kind);
            assert!(!result.file.tree().root_node().has_error());
        }
    }
    context.trim();
    let result = cache
        .load_step_with_context(
            Path::new("input.cs"),
            &c_sharp,
            &mut context,
            LoadOptions::default(),
        )
        .unwrap();
    assert!(matches!(result, LoadStep::Ready(_)));
    let restored = cache
        .prepare_language(&c_sharp_language, "c_sharp")
        .unwrap();
    let hit = cache
        .load_with_context(
            Path::new("input.cs"),
            &restored,
            &mut context,
            LoadOptions::default(),
        )
        .unwrap();
    assert!(hit.file.cache_hit());
}

#[test]
fn side_data_policy_applies_to_hits_and_late_publication() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[\n1,2]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let mut parser = tree_sitter::Parser::new();
    let load_with = |presence, points, write, parser: &mut tree_sitter::Parser| {
        cache
            .load_with_options(
                Path::new("file.json"),
                &language(),
                parser,
                LoadOptions {
                    pack: tree_sitter_squatter::PackOptions {
                        symbol_presence: presence,
                        points,
                        ..tree_sitter_squatter::PackOptions::default()
                    },
                    write,
                    cancel: None,
                },
            )
            .unwrap()
    };
    let bare = load_with(false, false, WritePolicy::Inline, &mut parser);
    assert!(!bare.file.cache_hit());
    assert!(!bare.file.tree().has_points());
    assert!(bare.file.tree().presence_cache().is_none());
    let presence = load_with(true, false, WritePolicy::Deferred, &mut parser);
    assert!(presence.file.cache_hit());
    assert!(presence.file.tree().presence_cache().is_some());
    assert_eq!(
        presence.pending_write.unwrap().publish().unwrap(),
        WriteOutcome::Published
    );
    let with_side_data = load_with(true, true, WritePolicy::Deferred, &mut parser);
    assert!(!with_side_data.file.cache_hit());
    assert!(with_side_data.file.tree().has_points());
    assert!(with_side_data.file.tree().presence_cache().is_some());
    assert_eq!(
        with_side_data
            .pending_write
            .as_ref()
            .unwrap()
            .publish()
            .unwrap(),
        WriteOutcome::Published
    );
    let stored = load_with(true, true, WritePolicy::Deferred, &mut parser);
    assert!(stored.file.cache_hit());
    assert!(stored.pending_write.is_none());
    for (presence, points) in [(false, true), (true, false), (false, false)] {
        let result = load_with(presence, points, WritePolicy::Disabled, &mut parser);
        assert!(result.file.cache_hit());
        assert_eq!(result.file.tree().presence_cache().is_some(), presence);
        assert_eq!(result.file.tree().has_points(), points);
    }
}
