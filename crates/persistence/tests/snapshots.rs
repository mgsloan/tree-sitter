use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
};
use tree_squatter_persistence::*;

#[test]
#[ignore = "subprocess helper"]
fn child_snapshot_holder() {
    use std::io::{Read, Write};
    let Some(root) = std::env::var_os("TSQ_SNAPSHOT_ROOT") else {
        return;
    };
    let cache = cache(Path::new(&root));
    let reader = load(&cache);
    assert!(reader.transaction_backed());
    println!("SNAPSHOT_READY");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0]);
    drop(reader);
}

#[test]
fn crashed_snapshot_reader_slot_is_reclaimable() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), source(1)).unwrap();
    let cache = cache(root.path());
    load(&cache);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_snapshot_holder",
            "--ignored",
            "--nocapture",
        ])
        .env("TSQ_SNAPSHOT_ROOT", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(output.read_line(&mut line).unwrap(), 0);
        if line.trim() == "SNAPSHOT_READY" {
            break;
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(cache.check_stale_readers().unwrap(), 1);
    assert_eq!(cache.check_stale_readers().unwrap(), 0);
    assert!(load(&cache).transaction_backed());
}

fn grammar() -> Grammar {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    Grammar::new(language, GrammarFingerprint([42; 32]))
}
fn source(value: u8) -> String {
    format!("[{}0]", format!("{value},").repeat(4096))
}
fn load(cache: &Persistence) -> LoadedFile {
    cache
        .load(
            Path::new("file.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
        )
        .unwrap()
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
