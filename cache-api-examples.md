# Cache API client examples

Examples against [the proposal](persistence-api-proposal.md), not the current
implementation. Async snippets run inside application async functions returning
`ExampleResult<_>`. Parsing, publication, and maintenance run on application workers;
no particular executor or worker-pool API is assumed. Examples are alternatives,
not a single program sharing consumed values.

| Client pattern | Example |
|---|---|
| Open a cache and reuse a loader | 1 |
| Read disk contents with default or no preprocessing | 2 |
| Use already-preprocessed contiguous input | 3 |
| Cache-only lookup | 4 |
| Parse and publish immediately | 5 |
| Parse and discard writes | 6 |
| Queue writes and retry | 7 |
| Parse a miss on another worker | 8 |
| Transfer publication to another process | 9 |
| Avoid cache creation; continue without a store | 10 |
| Use a rope without flattening | 11 |
| Preprocess directly into rope storage | 12 |
| Speculative structural work during verification | 13 |
| Share the cache and drop packing scratch | 14 |
| Cancellation | 15 |
| Transaction-backed trees and detachment | 16 |
| Packing variants | 17 |
| Source changes and unsaved buffers | 18 |
| Queries needing source text | 19 |
| Explicit maintenance | 20 |
| Verify without retaining source text | 21 |
| Metadata, reading, and hashing utilities | 22 |
| Different preprocessing choices with identical output | 23 |

## 1. Setup and loader reuse

The grammar provider supplies `language` and `fingerprint`. The client chooses
`root` and a consistent project-relative `path`. Plain paths are accepted without
validation, normalization, or canonicalization. Symlink components remain in the
logical identity even when their targets are outside the project. Retain the
grammar and loader across jobs using the appropriate cache for each source.

```rust
use std::{io, path::Path, sync::{Arc, atomic::AtomicBool}};
use tree_sitter::Point;
use tree_squatter::PackOptions;
use tree_squatter_persistence::*;

type ExampleResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

let mut file = SourceFile::open(root, path).await?;
let cache = Cache::open(root, CacheOptions::default())?;
let language = cache.language(&language, fingerprint)?;
let mut loader = cache.loader();
```

## 2. Read disk contents

```rust
let mut contents = file.read(ReadOptions::default()).await?;
let loaded = loader.load(&mut contents, &language, LoadOptions::default()).await?;
```

`SourceFile` defaults to `TextPreprocessing::zed()`. `read` returns an immutable
capture of processed bytes, raw metadata, and descriptive preprocessing information.
`FileContents` implements `Source` without decoding again. It can reuse its computed
fingerprint because its bytes are immutable. To retain unchanged bytes instead:

```rust
let mut file = SourceFile::open(root, path).await?
    .with_preprocessing(TextPreprocessing::none());
let contents = file.read(ReadOptions::default()).await?;
```

Raw byte length and processed byte length can differ. Cancellation or detected
metadata changes invalidate the capture, including partial hashes.

## 3. Already-preprocessed contiguous input

The caller supplies the project-relative path and raw metadata from the matching
capture, plus its preprocessing description. `bytes` must be the unchanged
processed snapshot. Keeping the path consistent is the client's responsibility.

```rust
let mut contents = FileContents::from_preprocessed(path.to_path_buf(), bytes, metadata, preprocessing_info);
let loaded = loader.load(&mut contents, &language, LoadOptions::default()).await?;
let mut input = contents.prepare(ReadOptions::default()).await?;
let suffix = input.read(0, Point::new(0, 0));
```

The prepared view borrows storage; the initial read can return the whole buffer.
Subsequent calls may revisit offsets. Construction computes the processed identity;
it does not trust a caller-supplied digest or preprocess bytes again.

## 4. Cache-only lookup

```rust
let tree = match loader.load(&mut source, &language, LoadOptions::default()).await? {
    LoadResult::Loaded(tree) => Some(tree),
    LoadResult::Miss(_) => None,
};
```

No preparation or parsing is needed. A file source can hash without retaining its
text where its preprocessing supports streaming.

## 5. Parse and publish immediately

Prepare asynchronously, then run the synchronous parse on a worker. Here the
source is owned by that worker or the application uses scoped work for the borrow.

```rust
let mut input = source.prepare(ReadOptions::default()).await?;
let mut publication = None;
let mut retry_writes = Vec::new();
let tree = miss.parse(&mut loader, &mut input, ParseOptions::default(), |write| {
    let outcome = write.publish(PublishOptions::default());
    if matches!(&outcome, Ok(WriteOutcome::Busy)) {
        retry_writes.push(write);
    }
    publication = Some(outcome);
})?;
if let Some(Err(error)) = publication {
    eprintln!("cache publication failed: {error}");
}
```

Publication errors do not invalidate the tree. The handler returns `()` and runs
once for an eligible successful parse; cache hits produce no write work.

## 6. Parse without publishing

```rust
let mut input = source.prepare(ReadOptions::default()).await?;
let tree = miss.parse(&mut loader, &mut input, ParseOptions::default(), drop)?;
```

This discards publication work explicitly. Preparing `PendingWrite` can still copy
source bytes under the current source-storing schema.

## 7. Queue publication and retry contention

```rust
let mut writes = Vec::new();
let tree = miss.parse(&mut loader, &mut input, ParseOptions::default(), |write| {
    writes.push(write);
})?;
let mut retry_later = Vec::new();
for write in writes {
    match write.publish(PublishOptions::default()) {
        Ok(WriteOutcome::Published | WriteOutcome::AlreadyPresent) => {}
        Ok(WriteOutcome::Busy) => retry_later.push(write),
        Err(error) => eprintln!("cache publication failed: {error}"),
    }
}
```

Keep `retry_later` in application state and schedule another attempt. For a
background writer, move owned work through a channel:

```rust
let (sender, receiver) = std::sync::mpsc::channel::<PendingWrite>();
let writer = std::thread::spawn(move || {
    let mut retry_later = Vec::new();
    for write in receiver {
        match write.publish(PublishOptions::default()) {
            Ok(WriteOutcome::Published | WriteOutcome::AlreadyPresent) => {}
            Ok(WriteOutcome::Busy) => retry_later.push(write),
            Err(error) => eprintln!("cache publication failed: {error}"),
        }
    }
    retry_later
});
let mut unsent = None;
let tree = miss.parse(&mut loader, &mut input, ParseOptions::default(), |write| {
    if let Err(error) = sender.send(write) {
        unsent = Some(error.0);
    }
})?;
drop(sender);
let mut retry_later = writer.join().expect("writer panicked");
retry_later.extend(unsent);
```

Joining here is illustrative synchronous coordination, not an executor scheduling
recommendation. A production async client awaits its worker pool's completion.

## 8. Parse a miss on another worker

Prepare outside the parser worker. This scoped-thread example permits an input
borrowing its source; the prepared input, miss, and loader must be `Send`.

```rust
let mut input = contents.prepare(ReadOptions::default()).await?;
let mut worker_loader = cache.loader();
let (tree, writes) = std::thread::scope(|scope| {
    scope.spawn(move || -> ExampleResult<_> {
        let mut writes = Vec::new();
        let tree = miss.parse(
            &mut worker_loader,
            &mut input,
            ParseOptions::default(),
            |write| writes.push(write),
        )?;
        Ok((tree, writes))
    }).join().expect("parser worker panicked")
})?;
```

The scope itself waits synchronously. Use it from an application worker, not an
async executor thread that must remain responsive. Long-lived queued jobs should
own their immutable source snapshot and prepare the view within its lifetime.
A miss never rechecks the cache or waits for another writer; publication may return
`AlreadyPresent` if another worker has published meanwhile.

## 9. Transfer publication to another process

The retained transfer API carries raw metadata, processed bytes, and descriptive
preprocessing information. Update its frame format for the new schema. Both
processes use matching grammar/runtime identities.

```rust
let mut transfer_error = None;
let tree = miss.parse(&mut loader, &mut input, ParseOptions::default(), |write| {
    transfer_error = write.write_transfer(&mut stream).err();
})?;
// Report transfer_error independently of successful parsing.
```

The receiving process uses its own shared cache:

```rust
let consumer = Cache::open(root, CacheOptions::default())?;
let write = consumer.read_transfer(&mut stream, &language, maximum_frame_bytes)?;
let outcome = write.publish(PublishOptions::default())?;
```

`transfer_len` remains available for framing. A busy write can be retained and
retried; transport retry/framing policy belongs to the caller.

## 10. Avoid cache creation or continue without a store

```rust
let cache = Cache::open(
    root,
    CacheOptions { create_cache_if_absent: false, ..Default::default() },
)?;
let mut loader = cache.loader();
let loaded = loader.load(&mut source, &language, LoadOptions::default()).await?;
```

A handle that starts without a store remains without one. Parsing still works;
write work can be transferred, but local publication errors. Existing caches are
still writable. An invalid project root can fail `open`.

## 11. Existing rope without flattening

`ChunkSource` indexes borrowed preprocessed chunks without copying their bytes.
`metadata` and `preprocessing_info` describe the disk capture represented by the
unchanged rope, not an edited live buffer.

```rust
let mut source = ChunkSource::from_preprocessed(
    path.to_path_buf(),
    snapshot.chunks().map(str::as_bytes),
    metadata,
    preprocessing_info,
);
let loaded = loader.load(&mut source, &language, LoadOptions::default()).await?;
let mut input = source.prepare(ReadOptions::default()).await?;
let tree = match loaded {
    LoadResult::Loaded(tree) => tree,
    LoadResult::Miss(miss) => miss.parse(
        &mut loader, &mut input, ParseOptions::default(), |write| writes.push(write),
    )?,
};
```

A native rope adapter can implement `Source` and `ParserInput` using its own cursor
instead of this chunk index. Reads may revisit offsets; empty slices signal EOF.
Source hashing traverses chunks in order. Publication may still copy source bytes.

## 12. Preprocess directly into a rope

On a blocking worker, the low-level synchronous utility can feed an application
`io::Write` adapter that appends validated UTF-8 into rope storage:

```rust
struct RopeWriter<'a> {
    rope: &'a mut rope::Rope,
    pending: Vec<u8>,
}
impl io::Write for RopeWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        };
        if valid != 0 {
            self.rope.push(std::str::from_utf8(&self.pending[..valid]).unwrap());
            self.pending.drain(..valid);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}
let mut snapshot = rope::Rope::new();
let mut output = RopeWriter { rope: &mut snapshot, pending: Vec::new() };
TextPreprocessing::zed().apply(&mut file, &mut output, PreprocessingOptions::default())?;
if !output.pending.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "incomplete UTF-8 input").into());
}
drop(output);
```

Discard the partial rope on error. `apply` may buffer before emitting when encoding
selection is uncertain. An async rope source performs this work through its I/O
backend and prepares the rope before synchronous parser access. The standalone
preprocessing crate must expose observed preprocessing information to that adapter;
`apply`'s unit-returning convenience API alone does not report it.

## 13. Structural analysis while the file loads

Move the candidate to an analysis worker while the source independently loads.
The worker returns the candidate and owned results through a runtime-independent
oneshot; verification then uses the immutable capture's computed fingerprint.

```rust
let candidate = loader.preview(&mut file, &language, LoadOptions::default()).await?;
if let Some(candidate) = candidate {
    let (sender, receiver) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let count = candidate.tree().root_node().preorder().count();
        let _ = sender.send((candidate, count));
    });
    let mut contents = file.read(ReadOptions::default()).await?;
    let (candidate, speculative_count) = receiver.await?;
    match candidate.verify(&mut contents, VerifyOptions::default()).await? {
        Verification::Confirmed(tree) => {
            // Keep speculative_count and the confirmed tree.
        }
        Verification::Changed(loaded) => {
            // Discard speculative_count; loaded is a new exact hit or a miss.
        }
    }
}
```

`file` is a `SourceFile`; `CachedCandidate` must be `Send`. The application may
reuse an analysis pool instead of starting a thread per job. Source-dependent
queries require matching text and cannot run speculatively against another capture.

## 14. Shared cache and scratch lifetime

```rust
let mut worker_loader = cache.loader();
let shared_cache = Arc::clone(&cache);
drop(cache);
worker_loader.drop_packer();
```

Each loader owns an `Arc<Cache>` and may outlive its creator. Use one loader per
parser worker. `drop_packer` sets the optional packer to `None`; the next parse
recreates it. `shared_cache` can create other independent loaders.

## 15. Cancellation

```rust
let flag = AtomicBool::new(false);
let cancellation = Canceler::new(&flag);
let loaded = loader.load(&mut source,
    &language,
    LoadOptions { cancellation, ..Default::default() },
).await?;
if let LoadResult::Miss(miss) = loaded {
    let mut input = source.prepare(ReadOptions {
        cancellation,
        ..Default::default()
    }).await?;
    let tree = miss.parse(
        &mut loader,
        &mut input,
        ParseOptions { cancellation, ..Default::default() },
        drop,
    )?;
}
```

A controller sharing the flag can call `cancel()` during work. Options borrow it;
`..Default::default()` requires no explicit lifetime. Defaults disable cancellation.
Use the same pattern for verification, preprocessing, and publication. Cancelling
an in-progress async read must be handled by the adapter; merely setting the flag
does not wake an arbitrary I/O backend. Dropping a preparation future must leave
no valid partial capture and allow a later operation to restart.

```rust
cancellation.cancel();
assert!(matches!(
    write.publish(PublishOptions { cancellation, ..Default::default() }),
    Err(CacheError::Cancelled)
));
let outcome = write.publish(PublishOptions::default())?;
```

Cancellation after a commit does not undo publication. Parse cancellation produces
no publication callback.

## 16. Transaction-backed trees

```rust
let cache = Cache::open(
    root,
    CacheOptions { read: ReadPolicy::PreferTransactionBacked, ..Default::default() },
)?;
if tree.transaction_backed() {
    let alias = tree.clone();
    let detached = tree.detach()?;
    assert!(!detached.transaction_backed());
    drop(tree);
    drop(alias);
}
```

`detached` owns its slab. Other aliases keep the original transaction pinned until
dropped. Backed reads can fall back to owned storage; fresh parses are owned.

## 17. Packing variants

```rust
let loaded = loader.load(&mut source,
    &language,
    LoadOptions {
        pack: PackOptions {
            points: false,
            symbol_presence: false,
            repack: true,
            ..Default::default()
        },
        ..Default::default()
    },
).await?;
```

Points and symbol presence select cache variants. Repacking and initial capacity
only control fresh packing. Without points, point APIs expose byte columns on row
zero. Preprocessing is chosen by the source, not load options or cached settings.

## 18. Changed files and unsaved buffers

An immutable `FileContents` or rope snapshot remains usable with its miss after
the live file changes. A disk source reread may produce different bytes; parsing
then fails the miss's identity check instead of publishing under the old hash.
Repeat lookup for the new capture. Metadata and preprocessing descriptions on a
write come from the actual prepared snapshot that passed validation.

Unsaved buffers cannot invent raw disk metadata. They remain outside the current
persistence contract; use direct parsing and packing with prepared editor input:

```rust
let mut parser = tree_sitter::Parser::new();
parser.set_language(&language)?;
let mut read = |offset, point| input.read(offset, point);
let native = parser.parse_with_options(&mut read, None, None)
    .ok_or_else(|| io::Error::other("parse did not complete"))?;
let language = tree_squatter::Language::new(&language)?;
let mut packer = tree_squatter::TreePacker::new()?;
let tree = packer.pack(&language, &native)?;
```

## 19. Queries using source text

```rust
let query = tree_squatter::Query::new(&language, "(_) @node")?;
let mut cursor = tree_squatter::QueryCursor::new();
let mut execution = cursor.execute(&query, tree.tree().root_node(), contents.bytes());
while let Some((matched, index)) = execution.next_capture() {
    let node = matched.captures[index].node;
    let text = &contents.bytes()[node.byte_range()];
}
if let Some(error) = execution.error() {
    return Err(error.into());
}
```

The bytes must match the confirmed tree. The current query API requires contiguous
text; a rope-aware query text provider remains separate work.

## 20. Maintenance

```rust
let mut pending_sweeps = Vec::new();
if let Some(mut sweep) = cache.sweep_missing() {
    let progress = sweep.step(64, None)?;
    if matches!(progress.state, MaintenanceState::More | MaintenanceState::Busy) {
        pending_sweeps.push(sweep);
    }
}
let mut pending_cleanups = Vec::new();
if let Some(mut cleanup) = cache.maintenance_missing(deleted_path)? {
    let progress = cleanup.step(64, None)?;
    if matches!(progress.state, MaintenanceState::More | MaintenanceState::Busy) {
        pending_cleanups.push(cleanup);
    }
}
let reclaimed = cache.check_stale_readers()?;
```

Retain queues and schedule further steps. Dropping work stops it. Existing
maintenance cancellation still uses `Option<&AtomicBool>`; migrating those options
and replacing live-file generation cleanup remain unspecified.

## 21. Verification without retaining source

```rust
let mut source = SourceFile::open(root, path).await?;
let candidate = loader.preview(&mut source, &language, LoadOptions::default()).await?;
let confirmed = match candidate {
    Some(candidate) => match candidate.verify(&mut source, VerifyOptions::default()).await? {
        Verification::Confirmed(tree) => Some(tree),
        Verification::Changed(_) => None,
    },
    None => None,
};
```

This cache-only client declines mismatches, including alternative exact hits.
`SourceFile::hash` streams preprocessed bytes into XXH3 where possible; no parser
input or rope is prepared. On decoding fallback it resets the partial hash and
uses the copied buffered decoder. `verify_file` is unnecessary because both file
and memory sources implement the same trait.

## 22. Metadata, contents, and hashing

```rust
let mut file = SourceFile::open(root, path).await?;
let observed = file.metadata().await?;
let flag = AtomicBool::new(false);
let fingerprint = file.hash(ReadOptions {
    cancellation: Canceler::new(&flag),
    ..Default::default()
}).await?;
let mut contents = file.read(ReadOptions::default()).await?;
let reader = contents.reader(ReadOptions::default()).await?;
let recomputed = hash_source(reader, Canceler::default()).await?;
assert_eq!(recomputed, contents.fingerprint().source);
```

`observed.byte_len` and `fingerprint.metadata.byte_len` are raw lengths;
`fingerprint.source.byte_len` is processed length. Independent file operations can
observe different versions. In-memory operations use the recorded capture.
`hash_source` reads already-preprocessed bytes; it never consults cached settings.

## 23. Different preprocessing choices

An automatic detector and an explicitly configured encoding may produce identical
UTF-8 bytes. Their preprocessing descriptions differ, but their source identities
match and they can reuse the same tree when other key fields agree.

```rust
let automatic = automatic_source.hash(ReadOptions::default()).await?;
let explicit = explicit_source.hash(ReadOptions::default()).await?;
let same_parser_input = automatic.source == explicit.source;
```

Here the two sources are caller-configured adapters; the standalone decoder
supports explicit encodings even though `TextPreprocessing` currently exposes only
`none()` and `zed()`. Stored preprocessing describes the published capture. It does
not influence decoding, filter candidates, or establish the current file's encoding.

## Remaining integration choices

- Select a disk I/O adapter and settle `Send` guarantees for source futures.
- Report observed decoding metadata through the standalone decoder's public API.
- Avoid repeated hashing when parsing an already-verified immutable snapshot.
- Avoid preparing source copies when write work will immediately be discarded.
- Add rope-aware query text and live-file generation cleanup interfaces.
