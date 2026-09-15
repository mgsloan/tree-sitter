#![cfg(target_os = "linux")]
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::fd::AsRawFd,
    path::Path,
    process::{Child, Command, Stdio},
    sync::atomic::AtomicBool,
};
use tree_squatter_persistence::*;

fn grammar() -> Grammar {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    Grammar::new(language, GrammarFingerprint([42; 32]))
}

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
    println!("OWNER_READY");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0]);
}

struct Owner(Child);
impl Owner {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_work_owner", "--ignored", "--nocapture"])
            .env(
                "TSQ_WORK_LOCK",
                root.join(".tree-squatter/cooperation.lock"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line.trim() == "OWNER_READY" {
                break;
            }
        }
        Self(child)
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn deferred_capture_survives_owner_death_and_source_change() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.json");
    fs::write(&path, "[1]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let owner = Owner::start(root.path());
    let LoadStep::Deferred(pending) = cache
        .load_step(
            Path::new("file.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
            LoadOptions::default(),
        )
        .unwrap()
    else {
        panic!("expected deferral")
    };
    let LoadStep::Deferred(pending) = pending
        .resume(&mut tree_sitter::Parser::new(), None)
        .unwrap()
    else {
        panic!("owner is still live")
    };
    fs::write(&path, "[2]").unwrap();
    drop(owner);
    let LoadStep::Ready(result) = pending
        .resume(&mut tree_sitter::Parser::new(), None)
        .unwrap()
    else {
        panic!("dead owner retained ownership")
    };
    assert_eq!(result.file.source(), b"[1]");
    let current = cache
        .load(
            Path::new("file.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
        )
        .unwrap();
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
    let _owner = Owner::start(root.path());
    let LoadStep::Deferred(pending) = cache
        .load_step(
            Path::new("file.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
            LoadOptions::default(),
        )
        .unwrap()
    else {
        panic!("expected deferral")
    };
    assert!(matches!(
        pending.resume(
            &mut tree_sitter::Parser::new(),
            Some(&AtomicBool::new(true))
        ),
        Err(LoadError::Cancelled)
    ));
    let start = std::time::Instant::now();
    let result = cache
        .load(
            Path::new("file.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
        )
        .unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(result.source(), b"true");
    assert!(!result.cache_hit());
    // A fresh probe sees the result even while the advisory work owner remains.
    assert!(
        cache
            .load(
                Path::new("file.json"),
                &grammar(),
                &mut tree_sitter::Parser::new()
            )
            .unwrap()
            .cache_hit()
    );
}

#[test]
fn deferred_contender_reuses_winner_publication() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[42]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let _owner = Owner::start(root.path());
    let LoadStep::Deferred(contender) = cache
        .load_step(
            Path::new("file.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
            LoadOptions::default(),
        )
        .unwrap()
    else {
        panic!("expected deferral")
    };
    let LoadStep::Deferred(winner) = cache
        .load_step(
            Path::new("file.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
            LoadOptions::default(),
        )
        .unwrap()
    else {
        panic!("expected deferral")
    };
    assert!(
        !winner
            .parse_now(&mut tree_sitter::Parser::new(), None)
            .unwrap()
            .file
            .cache_hit()
    );
    // No language is installed: a hit must not need to initialize this parser.
    let mut parser = tree_sitter::Parser::new();
    let LoadStep::Ready(result) = contender.resume(&mut parser, None).unwrap() else {
        panic!("publication must take precedence over the busy work lock")
    };
    assert!(result.file.cache_hit());
    assert_eq!(result.file.source(), b"[42]");
    assert!(parser.language().is_none());
}
