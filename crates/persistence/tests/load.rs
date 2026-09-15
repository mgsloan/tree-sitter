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
    CACHE_DIRECTORY, Grammar, GrammarFingerprint, LoadError, LoadOptions, Options, Persistence,
    WriteOutcome, WritePolicy,
};

fn grammar() -> Grammar {
    // Synthetic provider identity scoped to this test fixture, not a production
    // grammar fingerprint. Every test pairs it with the same packaged JSON parser.
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    Grammar::new(
        tree_sitter_squatter::Grammar::new(&language).unwrap(),
        GrammarFingerprint([42; 32]),
    )
}
fn load(cache: &Persistence) -> tree_squatter_persistence::LoadedFile {
    cache
        .load(
            Path::new("input.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
        )
        .unwrap()
}

#[test]
fn miss_hit_and_old_reader_survives_update() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("input.json"), b"{\"old\": [1, 2]}\r\n").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let first = load(&cache);
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
    fs::write(root.path().join("input.json"), b"[false, true]").unwrap();
    let new = load(&cache);
    assert!(!new.cache_hit());
    assert!(load(&cache).cache_hit());
    assert_eq!(
        new.tree().root_node().named_child(0).unwrap().kind(),
        "array"
    );
    assert_eq!(
        reader.tree().root_node().named_child(0).unwrap().kind(),
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
    assert_eq!(files, ["cooperation.lock", "data.mdb", "lock.mdb"]);
}

#[test]
fn deferred_disabled_and_cancelled_publication() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("input.json"), "123").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let mut parser = tree_sitter::Parser::new();
    let result = cache
        .load_with_options(
            Path::new("input.json"),
            &grammar(),
            &mut parser,
            LoadOptions {
                write: WritePolicy::Deferred,
                cancellation: None,
            },
        )
        .unwrap();
    let pending = result.pending_write.unwrap();
    let disabled = || {
        cache
            .load_with_options(
                Path::new("input.json"),
                &grammar(),
                &mut tree_sitter::Parser::new(),
                LoadOptions {
                    write: WritePolicy::Disabled,
                    cancellation: None,
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
    fs::write(root.path().join("input.json"), "123").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let old = cache
        .load_with_options(
            Path::new("input.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
            LoadOptions {
                write: WritePolicy::Deferred,
                cancellation: None,
            },
        )
        .unwrap();
    fs::write(root.path().join("input.json"), "456").unwrap();
    load(&cache);
    old.pending_write.unwrap().publish().unwrap();
    let current = load(&cache);
    assert_eq!(current.source(), b"456");
    assert!(current.cache_hit());
}

#[test]
fn reset_cancelled_parser_and_clear_included_ranges() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("input.json"), "[1,2,3]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let mut parser = tree_sitter::Parser::new();
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    parser.set_language(&language).unwrap();
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
        .load(Path::new("input.json"), &grammar(), &mut parser)
        .unwrap();
    assert_eq!(
        result.tree().root_node().named_child(0).unwrap().kind(),
        "array"
    );
    assert_eq!(result.tree().root_node().end_byte(), 7);
    assert!(load(&cache).cache_hit());
}

#[test]
fn cancellation_never_creates_entry() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("input.json"), "[1]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let cancellation = AtomicBool::new(true);
    let result = cache.load_with_options(
        Path::new("input.json"),
        &grammar(),
        &mut tree_sitter::Parser::new(),
        LoadOptions {
            write: WritePolicy::Inline,
            cancellation: Some(&cancellation),
        },
    );
    assert!(matches!(result, Err(LoadError::Cancelled)));
    cancellation.store(false, Ordering::Relaxed);
    assert!(!load(&cache).cache_hit());
}

#[test]
fn packing_variants_and_invalid_syntax_are_cacheable() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("input.json"), "[\n1,").unwrap();
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
    assert!(!load(&b).cache_hit());
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
    fs::write(blocked.path().join("input.json"), "true").unwrap();
    fs::write(blocked.path().join(CACHE_DIRECTORY), "do not change").unwrap();
    let cache = Persistence::open(blocked.path(), Options::default()).unwrap();
    assert!(!load(&cache).cache_hit());
    assert_eq!(
        fs::read(blocked.path().join(CACHE_DIRECTORY)).unwrap(),
        b"do not change"
    );

    let root = tempfile::tempdir().unwrap();
    let source = format!("\"{}\"", "x".repeat(1024 * 1024));
    fs::write(root.path().join("input.json"), &source).unwrap();
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
            Path::new("input.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
            LoadOptions {
                write: WritePolicy::Deferred,
                cancellation: None,
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
    fs::write(root.path().join("input.json"), "[1,2,3]").unwrap();
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
                            .named_child(0)
                            .unwrap()
                            .named_child_count(),
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
    symlink(
        outside.path().join("source"),
        root.path().join("input.json"),
    )
    .unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    assert!(!load(&cache).cache_hit());
    assert!(!load(&cache).cache_hit());
    let other = tempfile::tempdir().unwrap();
    fs::write(other.path().join("input.json"), "1").unwrap();
    symlink(outside.path(), other.path().join(CACHE_DIRECTORY)).unwrap();
    let cache = Persistence::open(other.path(), Options::default()).unwrap();
    assert!(!load(&cache).cache_hit());
    assert!(!outside.path().join("data.mdb").exists());
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
    fs::write(root.path().join("input.json"), "{\"cross_process\": true}").unwrap();
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
    fs::write(root.path().join("input.json"), "[true]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    assert!(!load(&cache).cache_hit());
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join(CACHE_DIRECTORY).join("cooperation.lock"))
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

/// Supply the pre-migration load test executable, built from the same native
/// runtime/grammar sources. Its child_load entry point is the compatibility probe.
#[test]
#[ignore = "requires TSQ_LEGACY_LOAD_TEST_BIN from the lmdb 0.8 backend"]
fn legacy_backend_round_trip() {
    let legacy = std::env::var_os("TSQ_LEGACY_LOAD_TEST_BIN").expect("legacy test executable");
    let current = std::env::current_exe().unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("input.json"), "{\"old_writer\": true}").unwrap();
    let probe = |binary: &std::path::Path, expected: &str| {
        let output = std::process::Command::new(binary)
            .args(["--exact", "child_load", "--nocapture"])
            .env("TSQ_TEST_PROJECT", root.path())
            .env("TSQ_TEST_EXPECT_HIT", expected)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    probe(Path::new(&legacy), "no");
    probe(&current, "yes");
    fs::write(root.path().join("input.json"), "{\"heed_writer\": true}").unwrap();
    probe(&current, "no");
    probe(Path::new(&legacy), "yes");
    probe(&current, "yes");
}

#[test]
#[ignore = "subprocess helper"]
fn child_writer_lock() {
    use std::io::{Read, Write};
    let Some(path) = std::env::var_os("TSQ_TEST_LOCK") else {
        return;
    };
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    lock.lock().unwrap();
    println!("WRITER_LOCK_READY");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0]);
}

#[test]
fn writer_death_releases_admission_without_stale_files() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("input.json"), "[true]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let result = cache
        .load_with_options(
            Path::new("input.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
            LoadOptions {
                write: WritePolicy::Deferred,
                cancellation: None,
            },
        )
        .unwrap();
    let write = result.pending_write.unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_writer_lock", "--ignored", "--nocapture"])
        .env(
            "TSQ_TEST_LOCK",
            root.path().join(CACHE_DIRECTORY).join("cooperation.lock"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "child exited before locking"
        );
        if line.trim() == "WRITER_LOCK_READY" {
            break;
        }
    }
    // Child owns admission, not an LMDB transaction. Parent must not block.
    let outcome = write.publish().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(outcome, WriteOutcome::Busy);
    assert_eq!(write.publish().unwrap(), WriteOutcome::Published);
    assert!(load(&cache).cache_hit());
}

#[test]
fn worker_context_switches_grammars_and_loads_restored_dictionary() {
    use tree_squatter_persistence::{LoadContext, LoadStep};
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("input.json"), "[1, 2]").unwrap();
    fs::write(root.path().join("input.cs"), "class C { int x; }").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let json_language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let c_sharp_language = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
    };
    let json = cache
        .prepare_grammar(&json_language, GrammarFingerprint([42; 32]))
        .unwrap();
    let c_sharp = cache
        .prepare_grammar(&c_sharp_language, GrammarFingerprint([43; 32]))
        .unwrap();
    let mut context = LoadContext::default();
    for _ in 0..3 {
        for (grammar, path, kind) in [
            (&json, "input.json", "document"),
            (&c_sharp, "input.cs", "compilation_unit"),
        ] {
            let result = cache
                .load_with_context(
                    Path::new(path),
                    grammar,
                    &mut context,
                    LoadOptions {
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
        .prepare_grammar(&c_sharp_language, GrammarFingerprint([43; 32]))
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
