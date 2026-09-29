#![cfg(target_os = "linux")]

mod common;
use common::{ChildProcess, language, load};

use std::{fs, ops::ControlFlow, os::fd::AsRawFd, path::Path};
use tree_squatter::{ParseOptions, traits::ParseStateLike};
use tree_squatter_persistence::*;

#[test]
#[ignore = "subprocess helper"]
fn child_work_owner() {
    let Some(path) = std::env::var_os("TSQ_WORK_LOCK") else {
        return;
    };
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as _;
    lock.l_whence = libc::SEEK_SET as _;
    lock.l_start = 1;
    lock.l_len = 256;
    assert_eq!(
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) },
        0
    );
    common::wait_until_killed();
}

fn owner(root: &Path) -> ChildProcess {
    ChildProcess::start(
        "child_work_owner",
        "TSQ_WORK_LOCK",
        root.join(CACHE_DIRECTORY).join("squat.coop-lock"),
    )
}

#[test]
fn deferred_capture_survives_owner_death_and_source_change() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.json");
    fs::write(&path, "[1]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let mut context = tree_squatter_persistence::LoadContext::default();
    let owner = owner(root.path());
    let LoadStep::Deferred(pending) = cache
        .load_step_with_context(
            Path::new("file.json"),
            &language(),
            &mut context,
            LoadOptions::default(),
        )
        .unwrap()
    else {
        panic!("expected deferral")
    };
    let LoadStep::Deferred(pending) = pending
        .resume_with_context(&mut context, ParseOptions::default())
        .unwrap()
    else {
        panic!("owner is still live")
    };
    fs::write(&path, "[2]").unwrap();
    drop(owner);
    let mut reports = 0;
    let mut progress = |state: &dyn ParseStateLike| {
        assert!(!state.is_converting());
        reports += 1;
        ControlFlow::Continue(())
    };
    let LoadStep::Ready(result) = pending
        .resume_with_context(
            &mut context,
            ParseOptions::new().progress_callback(&mut progress),
        )
        .unwrap()
    else {
        panic!("dead owner retained ownership")
    };
    assert!(reports > 0);
    assert_eq!(result.file.source(), b"[1]");
    let current = load(&cache);
    assert_eq!(current.source(), b"[2]");
    assert!(!current.cache_hit());
}

#[test]
fn wait_budget_bypasses_live_owner_and_cancellation_stops_deferred_work() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "true").unwrap();
    let cache = Persistence::open(
        root.path(),
        Options {
            cooperation_wait: std::time::Duration::from_millis(5),
            ..Options::default()
        },
    )
    .unwrap();
    let _owner = owner(root.path());
    let LoadStep::Deferred(pending) = cache
        .load_step(
            Path::new("file.json"),
            &language(),
            &mut tree_squatter::Parser::new(),
            LoadOptions::default(),
        )
        .unwrap()
    else {
        panic!("expected deferral")
    };
    assert!(matches!(
        pending.resume(
            &mut tree_squatter::Parser::new(),
            ParseOptions::new()
                .progress_callback(&mut |_: &dyn ParseStateLike| ControlFlow::Break(()))
        ),
        Err(LoadError::Cancelled)
    ));
    let mut captured = false;
    let mut waiting_checks = 0;
    let mut cancel_wait = |state: &dyn ParseStateLike| {
        assert!(!state.is_converting());
        captured |= state.current_byte_offset() == 4;
        if captured && state.current_byte_offset() == 0 {
            waiting_checks += 1;
            if waiting_checks == 2 {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    };
    let waiting_cache = Persistence::open(
        root.path(),
        Options {
            cooperation_wait: std::time::Duration::from_secs(10),
            ..Options::default()
        },
    )
    .unwrap();
    assert!(matches!(
        waiting_cache.load_with_context(
            Path::new("file.json"),
            &language(),
            &mut LoadContext::default(),
            LoadOptions {
                parse: ParseOptions::new().progress_callback(&mut cancel_wait),
                ..Default::default()
            },
        ),
        Err(LoadError::Cancelled)
    ));
    assert_eq!(waiting_checks, 2);
    let start = std::time::Instant::now();
    let result = load(&cache);
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(result.source(), b"true");
    assert!(!result.cache_hit());
    // A fresh probe sees the result even while the advisory work owner remains.
    assert!(load(&cache).cache_hit());
}

#[test]
fn deferred_contender_reuses_winner_publication() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[42]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let _owner = owner(root.path());
    let LoadStep::Deferred(contender) = cache
        .load_step(
            Path::new("file.json"),
            &language(),
            &mut tree_squatter::Parser::new(),
            LoadOptions::default(),
        )
        .unwrap()
    else {
        panic!("expected deferral")
    };
    let LoadStep::Deferred(winner) = cache
        .load_step(
            Path::new("file.json"),
            &language(),
            &mut tree_squatter::Parser::new(),
            LoadOptions::default(),
        )
        .unwrap()
    else {
        panic!("expected deferral")
    };
    assert!(
        !winner
            .parse_now_with_context(
                &mut tree_squatter_persistence::LoadContext::default(),
                ParseOptions::default()
            )
            .unwrap()
            .file
            .cache_hit()
    );
    // No language is installed: a hit must not need to initialize this parser.
    let mut parser = tree_squatter::Parser::new();
    let LoadStep::Ready(result) = contender
        .resume(&mut parser, ParseOptions::default())
        .unwrap()
    else {
        panic!("publication must take precedence over the busy work lock")
    };
    assert!(result.file.cache_hit());
    assert_eq!(result.file.source(), b"[42]");
    assert!(parser.language().is_none());
}
