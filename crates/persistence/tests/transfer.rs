mod common;
use common::grammar_with_identity as language;

use std::{fs, path::Path};
use tree_squatter_persistence::*;

fn load(cache: &Persistence, write: WritePolicy) -> LoadResult {
    cache
        .load_with_options(
            Path::new("file.json"),
            &language(42),
            &mut tree_sitter::Parser::new(),
            LoadOptions {
                pack: tree_sitter_squatter::PackOptions::default(),
                write,
                cancel: None,
            },
        )
        .unwrap()
}

#[test]
fn transfer_survives_producer_and_keeps_original_capture() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[1,\n2,3]").unwrap();
    let producer = Persistence::open_existing(root.path(), Options::default()).unwrap();
    let loaded = load(&producer, WritePolicy::Transfer);
    assert!(!root.path().join(CACHE_DIRECTORY).exists());
    let points = loaded.file.tree().point_data().unwrap().as_bytes().to_vec();
    let pending = loaded.pending_write.unwrap();
    let mut bytes = vec![];
    pending.write_transfer(&mut bytes).unwrap();
    assert_eq!(pending.transfer_len(), Some(bytes.len()));
    drop(pending);
    drop(loaded.file);
    drop(producer);
    fs::write(root.path().join("file.json"), "[4]").unwrap();
    let consumer = Persistence::open(root.path(), Options::default()).unwrap();
    let work = consumer
        .read_transfer(bytes.as_slice(), &language(42), bytes.len())
        .unwrap();
    assert_eq!(work.publish().unwrap(), WriteOutcome::Published);
    assert!(!load(&consumer, WritePolicy::Disabled).file.cache_hit());
    fs::write(root.path().join("file.json"), "[1,\n2,3]").unwrap();
    let original = load(&consumer, WritePolicy::Disabled).file;
    assert!(original.cache_hit());
    assert_eq!(original.source(), b"[1,\n2,3]");
    assert_eq!(original.tree().point_data().unwrap().as_bytes(), points);
    assert_eq!(
        original.tree().root_node().end_position(),
        tree_sitter::Point::new(1, 4)
    );
}

#[test]
fn transfer_rejects_bad_identity_lengths_and_truncation() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[1]").unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let mut bytes = vec![];
    load(&cache, WritePolicy::Transfer)
        .pending_write
        .unwrap()
        .write_transfer(&mut bytes)
        .unwrap();
    assert!(
        cache
            .read_transfer(bytes.as_slice(), &language(43), bytes.len())
            .is_err()
    );
    assert!(
        cache
            .read_transfer(bytes.as_slice(), &language(42), bytes.len() - 1)
            .is_err()
    );
    for end in [0, 8, 32, 216, bytes.len() - 1] {
        assert!(
            cache
                .read_transfer(&bytes[..end], &language(42), bytes.len())
                .is_err()
        );
    }
    let mut overflow = bytes.clone();
    overflow[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(
        cache
            .read_transfer(overflow.as_slice(), &language(42), bytes.len())
            .is_err()
    );
    bytes[200 + 1 + "file.json".len()] ^= 1; // captured source bytes
    assert!(
        cache
            .read_transfer(bytes.as_slice(), &language(42), bytes.len())
            .is_err()
    );
    assert!(!load(&cache, WritePolicy::Disabled).file.cache_hit());
}

#[test]
fn transfer_reads_consecutive_frames_without_waiting_for_eof() {
    // Model an open connection: attempting to read beyond the available frames
    // would block instead of returning EOF.
    struct OpenStream<'a>(&'a [u8]);
    impl std::io::Read for OpenStream<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() && !buffer.is_empty() {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            std::io::Read::read(&mut self.0, buffer)
        }
    }

    let root = tempfile::tempdir().unwrap();
    let cache = Persistence::open(root.path(), Options::default()).unwrap();
    let mut bytes = vec![];
    let mut lengths = vec![];
    for source in ["[1]", "[2,3]"] {
        fs::write(root.path().join("file.json"), source).unwrap();
        let pending = load(&cache, WritePolicy::Transfer).pending_write.unwrap();
        lengths.push(pending.transfer_len().unwrap());
        pending.write_transfer(&mut bytes).unwrap();
    }

    let mut stream = OpenStream(&bytes);
    for (index, source) in ["[1]", "[2,3]"].into_iter().enumerate() {
        let pending = cache
            .read_transfer(&mut stream, &language(42), lengths[index])
            .unwrap();
        assert_eq!(stream.0.len(), lengths[index + 1..].iter().sum());
        assert_eq!(pending.publish().unwrap(), WriteOutcome::Published);
        fs::write(root.path().join("file.json"), source).unwrap();
        let loaded = load(&cache, WritePolicy::Disabled).file;
        assert!(loaded.cache_hit());
        assert_eq!(loaded.source(), source.as_bytes());
    }
}

#[cfg(unix)]
#[test]
fn outside_root_sources_cannot_be_transferred() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    fs::write(outside.path(), "[]").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("file.json")).unwrap();
    let cache = Persistence::open_existing(root.path(), Options::default()).unwrap();
    assert!(load(&cache, WritePolicy::Transfer).pending_write.is_none());
    assert!(!root.path().join(CACHE_DIRECTORY).exists());
}

#[test]
fn transfer_preserves_points_policy() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.json"), "[\n0]").unwrap();
    for points in [false, true] {
        let cache = Persistence::open(
            root.path(),
            Options {
                points,
                ..Default::default()
            },
        )
        .unwrap();
        let pending = cache
            .load_with_options(
                Path::new("file.json"),
                &language(42),
                &mut tree_sitter::Parser::new(),
                LoadOptions {
                    pack: tree_sitter_squatter::PackOptions {
                        points,
                        ..Default::default()
                    },
                    write: WritePolicy::Transfer,
                    cancel: None,
                },
            )
            .unwrap()
            .pending_write
            .unwrap();
        let mut bytes = Vec::new();
        pending.write_transfer(&mut bytes).unwrap();
        assert_eq!(pending.transfer_len(), Some(bytes.len()));
        let imported = cache
            .read_transfer(bytes.as_slice(), &language(42), bytes.len())
            .unwrap();
        assert_eq!(imported.publish().unwrap(), WriteOutcome::Published);
        let opposite = Persistence::open(
            root.path(),
            Options {
                points: !points,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            opposite
                .read_transfer(bytes.as_slice(), &language(42), bytes.len())
                .is_err()
        );
    }
}
