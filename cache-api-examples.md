# Cache API client examples

Examples against [the proposal](persistence-api-proposal.md), not the current
implementation. Snippets share the setup below unless stated otherwise. Helpers
such as `capture_file`, `lookup`, and `chunk_reader` are application code.
`SourceFile`, `FileMetadata`, and `hash_source` are proposed library utilities.

| Client pattern | Example |
|---|---|
| Open a cache, prepare grammars, reuse a worker | 1 |
| Load disk bytes, with or without preprocessing | 2 |
| Use existing contiguous input | 3 |
| Inspect the cache without parsing | 4 |
| Parse a miss and publish immediately | 5 |
| Parse without writing | 6 |
| Queue writes and retry contention | 7 |
| Move parsing work and its source to another thread | 8 |
| Transfer writes to another process | 9 |
| Parse when the cache is unavailable; avoid cache creation | 10 |
| Use an existing rope without flattening | 11 |
| Transform streamed input directly into a rope | 12 |
| Start structural work before reading the source | 13 |
| Share a cache across workers; drop packing scratch | 14 |
| Cancel loading, transformation, parsing, verification, or publication | 15 |
| Use transaction-backed trees and detach them | 16 |
| Select packing variants | 17 |
| Handle edits between lookup and parsing | 18 |
| Query trees with source text | 19 |
| Run explicit maintenance | 20 |
| Verify a disk candidate without retaining source text | 21 |
| Read metadata, contents, or a standalone fingerprint | 22 |

## 1. Setup and worker reuse

The grammar provider supplies `language` and its implementation `fingerprint`.
`root` is a project directory; `path` is relative to it. Later snippets execute
inside functions returning `ExampleResult<_>` unless a different return is shown.

```rust
use std::{
    fs::File,
    io::{self, Write},
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use tree_sitter::Point;
use tree_sitter_squatter::PackOptions;
use tree_squatter_persistence::*;

type ExampleResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

let cache = Cache::open(root, CacheOptions::default())?;
let grammar = cache.grammar(&language, fingerprint)?;
let mut loader = cache.loader();
```

Retain `grammar` and `loader` across jobs. Other grammars can use the same loader;
each job supplies its grammar explicitly. `Cache` itself is not cloned: its
returned `Arc` is cloned when sharing it with another owner.

## 2. Capture disk input, optionally transforming it

The application owns the bytes. This helper detects ordinary concurrent changes
by comparing metadata before and after reading the same file handle. That is not
proof of unchanged contents; metadata-only cache results remain speculative.

```rust
struct Capture {
    bytes: Arc<[u8]>,
    file_byte_len: u64,
    mtime: FileModificationTime,
    preprocessing: TextPreprocessing,
}

fn capture_file(
    absolute_path: &Path,
    preprocessing: TextPreprocessing,
    cancellation: Canceler<'_>,
) -> io::Result<Capture> {
    let contents = SourceFile::open(absolute_path)?.read(ReadOptions {
        preprocessing,
        cancellation,
    })?;
    Ok(Capture {
        bytes: contents.bytes.into(),
        file_byte_len: contents.fingerprint.metadata.byte_len,
        mtime: contents.fingerprint.metadata.mtime,
        preprocessing: contents.fingerprint.preprocessing,
    })
}

fn bytes_reader(bytes: &[u8]) -> impl FnMut(usize, Point) -> &[u8] {
    move |offset, _| bytes.get(offset..).unwrap_or_default()
}

fn lookup(
    loader: &Loader,
    path: &Path,
    grammar: &Grammar,
    capture: &Capture,
    cancellation: Canceler<'_>,
) -> Result<LoadResult, LoadError> {
    loader.load(
        path,
        capture.file_byte_len,
        capture.mtime,
        grammar,
        &mut bytes_reader(&capture.bytes),
        LoadOptions {
            preprocessing: capture.preprocessing,
            cancellation,
            ..Default::default()
        },
    )
}
```

Choose unchanged bytes or Zed-compatible decoding and newline handling:

`TextPreprocessing::default()` is `zed()`, including in `LoadOptions::default()`.
Use `none()` explicitly for unchanged bytes.

```rust
let raw = capture_file(
    &root.join(path),
    TextPreprocessing::none(),
    Canceler::default(),
)?;

let capture = capture_file(
    &root.join(path),
    TextPreprocessing::zed(),
    Canceler::default(),
)?;
```

`file_byte_len` is the raw file length, not `capture.bytes.len()`. Tree coordinates
refer to the transformed bytes. An interrupted transform's partial output is
discarded by this helper. `none()` also supports arbitrary bytes; `zed()` detects
and decodes text encodings before normalizing line endings.

## 3. Load already available contiguous input

`capture` can come from the helper or the application's existing file loader.
The source must represent the disk capture described by its metadata.

```rust
let mut read = bytes_reader(&capture.bytes);
let loaded = loader.load(
    path,
    capture.file_byte_len,
    capture.mtime,
    &grammar,
    &mut read,
    LoadOptions {
        preprocessing: capture.preprocessing,
        ..Default::default()
    },
)?;
```

This also handles empty files: the first read returns an empty slice. EOF comes
from the callback, not from the raw file length. Input is hashed but never parsed
by `load`.

## 4. Cache-only access

The application decides what a miss means. No write callback is needed until it
chooses to parse.

```rust
let cached_tree = match lookup(&loader, path, &grammar, &capture, Canceler::default())? {
    LoadResult::Loaded(tree) => Some(tree),
    LoadResult::Miss(miss) => {
        drop(miss); // no parsing, publication, or source ownership to clean up
        None
    }
};
```

Examples 5–9 show alternative policies for a `CacheMiss` obtained from this match.
They are alternatives, not sequential uses of the same consumed miss.

## 5. Parse and publish immediately

Publication has its own outcome. A full/unavailable cache or publication error
does not turn a successfully parsed tree into a parse failure.

```rust
let mut publication = None;
let mut retry_writes = Vec::new();
let tree = match lookup(&loader, path, &grammar, &capture, Canceler::default())? {
    LoadResult::Loaded(tree) => tree,
    LoadResult::Miss(miss) => miss.parse(
        &mut loader,
        &mut bytes_reader(&capture.bytes),
        ParseOptions::default(),
        |write| {
            let outcome = write.publish(PublishOptions::default());
            if matches!(&outcome, Ok(WriteOutcome::Busy)) {
                retry_writes.push(write);
            }
            publication = Some(outcome);
        },
    )?,
};

if let Some(Err(error)) = publication {
    eprintln!("cache publication failed: {error}");
}
// `tree` is usable regardless of the publication outcome.
```

No callback invocation means there was no eligible publication work, such as on
a hit or for an ineligible path. The callback returns `()`: use captured state to
report a publication error to the caller rather than using `?` inside it.

## 6. Parse and deliberately discard writes

```rust
let tree = miss.parse(
    &mut loader,
    &mut bytes_reader(&capture.bytes),
    ParseOptions::default(),
    drop,
)?;
```

This suppresses publication, not preparation of `PendingWrite`. Under the current
proposal, preparing work may still allocate a source copy. A future API could
avoid that cost if discard is a common path.

## 7. Queue writes and retry without holding a transaction

```rust
let mut writes = Vec::new();
let tree = miss.parse(
    &mut loader,
    &mut bytes_reader(&capture.bytes),
    ParseOptions::default(),
    |write| writes.push(write),
)?;
drop(capture); // queued writes retain what publication needs

let mut retry_later = Vec::new();
for write in writes {
    match write.publish(PublishOptions::default()) {
        Ok(WriteOutcome::Published | WriteOutcome::AlreadyPresent) => {}
        Ok(WriteOutcome::Busy) => retry_later.push(write),
        Err(error) => {
            eprintln!("cache publication failed: {error}");
            // This client chooses to discard errors rather than retry them.
        }
    }
}
```

A scheduler can retry `retry_later` later; this is deliberately not a busy loop.
For a background writer, pass the same owned values through a channel:

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
let tree = miss.parse(
    &mut loader,
    &mut bytes_reader(&capture.bytes),
    ParseOptions::default(),
    |write| {
        if let Err(error) = sender.send(write) {
            unsent = Some(error.0);
        }
    },
)?;
drop(sender);
let mut retry_later = writer.join().expect("writer panicked");
retry_later.extend(unsent);
```

Moving work to another process is different: it needs serialization, not an `Arc`.

## 8. Queue a cache miss for parsing elsewhere

The miss does not own the source. Move an immutable snapshot alongside it, and
construct the callback inside the worker. An edited live buffer is not a substitute.

```rust
let worker_cache = Arc::clone(&cache);
let source = Arc::clone(&capture.bytes);
let worker = std::thread::spawn(move || -> ExampleResult<_> {
    let mut loader = worker_cache.loader();
    let mut writes = Vec::new();
    let tree = miss.parse(
        &mut loader,
        &mut bytes_reader(&source),
        ParseOptions::default(),
        |write| writes.push(write),
    )?;
    Ok((tree, writes))
});
let (tree, writes) = worker.join().expect("parser worker panicked")?;
```

Another worker might publish during the delay. `CacheMiss::parse` still parses:
there is no cache recheck or cooperative waiting in this version. Publication
can subsequently return `AlreadyPresent`.

## 9. Transfer publication to another process

The proposal retains `PendingWrite::write_transfer`, `transfer_len`, and
`Cache::read_transfer` from the existing transfer API. The wire format must be
updated to carry the proposed raw metadata and transform profile. Producer and
consumer use the same build and matching grammar identity.

Producer: leave cache creation to the receiver. Here `stream` is a caller-owned
IPC writer; writing happens synchronously inside the callback.

```rust
let producer = Cache::open(
    root,
    CacheOptions { create_cache_if_absent: false, ..Default::default() },
)?;
let mut loader = producer.loader();
let mut transfer_error = None;
let tree = match lookup(&loader, path, &grammar, &capture, Canceler::default())? {
    LoadResult::Loaded(tree) => tree,
    LoadResult::Miss(miss) => miss.parse(
        &mut loader,
        &mut bytes_reader(&capture.bytes),
        ParseOptions::default(),
        |write| {
            transfer_error = write.write_transfer(&mut stream).err();
        },
    )?,
};
// Handle transfer_error independently; the parsed tree remains usable.
```

Receiver: `stream` supplies the frame, and `maximum_frame_bytes` is the caller's
limit. Receiving does not reparse or replace the captured source with today's disk bytes.

```rust
let consumer = Cache::open(root, CacheOptions::default())?;
let write = consumer.read_transfer(&mut stream, &grammar, maximum_frame_bytes)?;
let outcome = write.publish(PublishOptions::default())?;
// Retain write for retry if outcome is Busy.
```

A producer that no longer exists is fine once the frame has been transferred.
Partial transport failures require the application's framing/reconnect policy.

## 10. No cache creation, or an unavailable cache

```rust
let cache = Cache::open(
    root,
    CacheOptions { create_cache_if_absent: false, ..Default::default() },
)?;
let mut loader = cache.loader();
let tree = match lookup(&loader, path, &grammar, &capture, Canceler::default())? {
    LoadResult::Loaded(tree) => tree,
    LoadResult::Miss(miss) => miss.parse(
        &mut loader,
        &mut bytes_reader(&capture.bytes),
        ParseOptions::default(),
        drop,
    )?,
};
```

A missing or unavailable store allows parsing. A bad project root still causes
`Cache::open` to fail. `create_cache_if_absent: false` does not make an existing
cache read-only. A persistable parse without a store still supplies transferable
work; attempting local publication reports an error.
If this handle starts without a store, it stays that way for its lifetime,
even if another process creates the cache. Reusing it through the registry does
not upgrade it.

## 11. Existing rope snapshot, without flattening

This adapter builds an index of borrowed chunks and caches the current chunk.
It copies no source bytes, supports arbitrary byte offsets, and needs no `dyn`.
A production integration can use its rope's native seek cursor instead of this
temporary index. The returned slices borrow the caller's immutable snapshot.

```rust
fn chunk_reader<'a>(
    chunks: impl IntoIterator<Item = &'a [u8]>,
) -> impl FnMut(usize, Point) -> &'a [u8] {
    let mut length = 0;
    let chunks: Vec<_> = chunks.into_iter().filter(|chunk| !chunk.is_empty())
        .map(|chunk| {
            let start = length;
            length += chunk.len();
            (start, chunk)
        })
        .collect();
    let mut current = 0;
    move |offset, _| {
        if offset >= length {
            return &[];
        }
        let (start, chunk) = chunks[current];
        if offset < start || offset >= start + chunk.len() {
            current = chunks.partition_point(|(start, _)| *start <= offset) - 1;
        }
        let (start, chunk) = chunks[current];
        &chunk[offset - start..]
    }
}

let mut read = chunk_reader(snapshot.chunks().map(str::as_bytes));
let preprocessing = TextPreprocessing::zed();
let tree = match loader.load(
    path,
    raw_file_byte_len,
    raw_file_mtime,
    &grammar,
    &mut read,
    LoadOptions { preprocessing, ..Default::default() },
)? {
    LoadResult::Loaded(tree) => tree,
    LoadResult::Miss(miss) => miss.parse(
        &mut loader,
        &mut read,
        ParseOptions::default(),
        |write| writes.push(write),
    )?,
};
```

Zed's [`Rope::chunks`](https://github.com/zed-industries/zed/blob/main/crates/rope/src/rope.rs)
supplies string chunks for this adapter. `snapshot` must be the unchanged loaded
UTF-8 snapshot with Zed preprocessing already applied. The mode describes its
profile; they do not transform it again. Snapshot length is not raw disk length.
The parser/hash path does not flatten it, but the current deferred-write schema
still requires an owned source copy when constructing `PendingWrite`.

## 12. Transform streamed bytes directly into rope storage

`TextPreprocessing::apply` accepts `io::Write`. A small application writer can
collect UTF-8 bytes into rope chunks without making one full-file string. For
example, this writer retains only an incomplete trailing UTF-8 sequence:

```rust
struct RopeWriter<'a> {
    rope: &'a mut rope::Rope,
    pending: Vec<u8>,
}

impl Write for RopeWriter<'_> {
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

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

let mut snapshot = rope::Rope::new();
let mut output = RopeWriter { rope: &mut snapshot, pending: Vec::new() };
preprocessing.apply(&mut file, &mut output, PreprocessingOptions::default())?;
if !output.pending.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "incomplete UTF-8 input").into());
}
drop(output);
// Use snapshot with the chunk_reader from example 11.
```

On an error or cancellation, discard the partial rope. This writer rejects
invalid UTF-8; clients preserving arbitrary bytes use byte storage instead.
`zed()` includes automatic decoding of non-UTF-8 files. Its fallback may buffer
input before writing to this adapter; streaming output does not imply bounded
memory for every encoding-detection path.

## 13. Analyze a candidate while the file loads

This example runs a simple source-independent analysis concurrently with capture.
It keeps that work only if verification confirms the candidate. A preview miss,
changed candidate with another exact hit, and changed candidate requiring parsing
all join the same final flow. `options` uses the intended transform profile.

```rust
let observed = std::fs::metadata(root.join(path))?;
let candidate = loader.preview(
    path,
    observed.len(),
    FileModificationTime::new(observed.modified()?),
    &grammar,
    LoadOptions { preprocessing, ..Default::default() },
)?;

let absolute_path = root.join(path);
let (candidate, speculative_count, capture) = std::thread::scope(|scope| {
    let capture = scope.spawn(|| {
        capture_file(&absolute_path, preprocessing, Canceler::default())
    });
    let analysis = scope.spawn(move || {
        let count = candidate.as_ref().map(|candidate| {
            candidate.tree().root_node().preorder().count()
        });
        (candidate, count)
    });
    let (candidate, count) = analysis.join().expect("analysis panicked");
    let capture = capture.join().expect("capture panicked")?;
    Ok::<_, io::Error>((candidate, count, capture))
})?;

let mut read = bytes_reader(&capture.bytes);
let (loaded, confirmed_count) = match candidate {
    Some(candidate) => match candidate.verify(
        capture.file_byte_len,
        capture.mtime,
        &mut read,
        VerifyOptions::default(),
    )? {
        Verification::Confirmed(tree) => (LoadResult::Loaded(tree), speculative_count),
        Verification::Changed(loaded) => (loaded, None),
    },
    None => (
        lookup(&loader, path, &grammar, &capture, Canceler::default())?,
        None,
    ),
};
let tree = match loaded {
    LoadResult::Loaded(tree) => tree,
    LoadResult::Miss(miss) => miss.parse(
        &mut loader,
        &mut read,
        ParseOptions::default(),
        |write| writes.push(write),
    )?,
};
let count = confirmed_count.unwrap_or_else(|| tree.tree().root_node().preorder().count());
```

Use metadata from the actual capture in `verify`, even if it differs from preview.
Hash verification can confirm equivalent transformed contents despite changed
raw metadata. If reading or verification fails, speculative work is discarded;
it is never promoted to a verified result. Confirmation applies to the captured
snapshot, not to future edits or later disk contents.

## 14. Shared cache, independent workers, reusable scratch

This is a one-job worker; a real worker repeats its loop using the same loader.
The source and relative path move with the job.

```rust
let mut worker_loader = cache.loader();
let worker_grammar = grammar.clone();
let job_path = path.to_path_buf();
let worker = std::thread::spawn(move || -> ExampleResult<_> {
    let tree = match lookup(
        &worker_loader, &job_path, &worker_grammar, &capture, Canceler::default(),
    )? {
        LoadResult::Loaded(tree) => tree,
        LoadResult::Miss(miss) => miss.parse(
            &mut worker_loader,
            &mut bytes_reader(&capture.bytes),
            ParseOptions::default(),
            drop,
        )?,
    };
    worker_loader.drop_packer(); // use during idle periods or after a large job
    // Later parsing recreates packing scratch; the parser is retained.
    Ok(tree)
});
drop(cache); // worker_loader owns its own Arc<Cache>
let tree = worker.join().expect("worker panicked")?;
```

Each worker needs its own loader. Do not put one shared parser behind a mutex
merely to share the cache. Repeated `Cache::open` calls with conflicting options
remain an unresolved policy; reuse the same returned `Arc` for this pattern.

## 15. Cancellation across phases and threads

Use one flag for a job, or separate flags to cancel parsing and writing
independently. Default cancellation is disabled.

```rust
let disabled = Canceler::default();
assert!(!disabled.can_cancel());
disabled.cancel();
assert!(!disabled.is_cancelled());

let flag = Arc::new(AtomicBool::new(false));
let worker_flag = Arc::clone(&flag);
let worker = std::thread::spawn(move || -> ExampleResult<_> {
    let cancellation = Canceler::new(&worker_flag);
    let capture = capture_file(&absolute_path, preprocessing, cancellation)?;
    let mut read = bytes_reader(&capture.bytes);
    let loaded = loader.load(
        &relative_path,
        capture.file_byte_len,
        capture.mtime,
        &grammar,
        &mut read,
        LoadOptions { preprocessing, cancellation, ..Default::default() },
    )?;
    let mut writes = Vec::new();
    let tree = match loaded {
        LoadResult::Loaded(tree) => tree,
        LoadResult::Miss(miss) => miss.parse(
            &mut loader,
            &mut read,
            ParseOptions { cancellation, ..Default::default() },
            |write| writes.push(write),
        )?,
    };
    Ok((tree, writes))
});

Canceler::new(&flag).cancel(); // normally triggered by a user action
let completion = worker.join().expect("worker panicked");
// The operation may already have completed; cancellation is cooperative.
```

The same token goes into `VerifyOptions { cancellation, ..Default::default() }` for candidate
verification and `PublishOptions { cancellation, ..Default::default() }` for writes. Cancellation during
transformation reports `io::ErrorKind::Interrupted`; load/parse and publication use
their respective cancellation errors. Cancelled parsing does not invoke the write
handler. Cancellation after publication commits cannot undo that publication.

A cancelled write can be explicitly retried with a fresh token:

```rust
let flag = AtomicBool::new(false);
let cancellation = Canceler::new(&flag);
cancellation.cancel();
assert!(matches!(
    write.publish(PublishOptions { cancellation, ..Default::default() }),
    Err(CacheError::Cancelled)
));
let outcome = write.publish(PublishOptions::default())?;
```

## 16. Transaction-backed reads and explicit detachment

Choose the policy when opening this cache. Prefer a single configuration per
project while the repeated-open policy remains undecided.

```rust
let cache = Cache::open(
    root,
    CacheOptions { read: ReadPolicy::PreferTransactionBacked, ..Default::default() },
)?;
let mut loader = cache.loader();
let tree = match lookup(&loader, path, &grammar, &capture, Canceler::default())? {
    LoadResult::Loaded(tree) => tree,
    LoadResult::Miss(miss) => miss.parse(
        &mut loader,
        &mut bytes_reader(&capture.bytes),
        ParseOptions::default(),
        drop,
    )?,
};

if tree.transaction_backed() {
    let alias = tree.clone();
    let detached = tree.detach()?;
    assert!(!detached.transaction_backed());
    drop(tree);
    // alias still pins the original snapshot, including across cache updates.
    drop(alias);
    // detached owns its slab and does not pin the LMDB snapshot.
}
```

This policy is a preference: alignment or reader-admission limits can cause an
owned fallback. Freshly parsed trees are owned. A live backed tree or candidate
remains valid across publication and cleanup, but can delay reuse of LMDB pages.

## 17. Packing variants and byte-oriented clients

```rust
let loaded = loader.load(
    path,
    capture.file_byte_len,
    capture.mtime,
    &grammar,
    &mut bytes_reader(&capture.bytes),
    LoadOptions {
        pack: PackOptions {
            points: false,
            symbol_presence: false,
            repack: true,
            ..Default::default()
        },
        preprocessing: capture.preprocessing,
        ..Default::default()
    },
)?;
```

With `points: false`, point APIs expose byte offsets as columns on row zero.
Disabling symbol presence omits that query index. These two settings identify
distinct cached variants; `repack` and initial capacity only affect fresh packing.
They do not force a cache hit to be repacked or given spare capacity. A miss retains
the selected settings for `parse`.

## 18. Source changes and unsaved buffers

For disk edits after lookup, keep the original immutable snapshot with the miss:

```rust
let original = Arc::clone(&capture.bytes);
// The editor or disk may now advance to another version.
let tree = miss.parse(
    &mut loader,
    &mut bytes_reader(&original),
    ParseOptions::default(),
    |write| writes.push(write),
)?;
// tree and any resulting write describe original, not the newer contents.
```

If the original snapshot was discarded, perform a new capture and lookup. Passing
different contents to the old miss must report an identity mismatch. The proposal
has not yet named that error variant.

Unsaved or arbitrary buffers are not supported by the current disk-associated
lookup contract. Passing `drop` as the write handler does not make false disk
metadata valid. The immediate fallback is ordinary Tree-sitter parsing and packing:

```rust
let mut parser = tree_sitter::Parser::new();
parser.set_language(&language)?;
let mut read = chunk_reader(edited_snapshot.chunks().map(str::as_bytes));
let native = parser.parse_with_options(&mut read, None, None)
    .ok_or_else(|| io::Error::other("parse did not complete"))?;
let prepared = tree_sitter_squatter::Grammar::new(&language)?;
let mut packer = tree_sitter_squatter::TreePacker::new()?;
let tree = packer.pack(&prepared, &native)?;
```

Here `TreePacker` is the proposed rename of `PackContext`. Incremental editing,
non-UTF-8 decoding profiles, and cache reuse for non-disk buffers need separate
API decisions; these examples do not pretend those operations exist.

## 19. Queries that need source text

Structural navigation works directly on a loaded tree. Text predicates also need
the exact transformed source. With contiguous bytes, the current Squatter query
API can use the caller's capture:

```rust
let query = tree_sitter_squatter::Query::new(&language, "(_) @node")?;
let mut cursor = tree_sitter_squatter::QueryCursor::new();
let mut execution = cursor.execute(&query, tree.tree().root_node(), &capture.bytes);
while let Some((matched, index)) = execution.next_capture() {
    let node = matched.captures[index].node;
    let text = &capture.bytes[node.byte_range()];
    // Consume node/text here; capture storage is borrowed from the cursor.
}
if let Some(error) = execution.error() {
    return Err(error.into());
}
```

Do not use this with an unverified preview or a newer edited snapshot. The current
query API takes a flat byte slice, so parsing a rope without flattening does not
yet imply rope-aware text predicates. That query interface is a separate gap.

## 20. Explicit maintenance

These entry points are retained from the existing maintenance API, with `Cache`
owning the shared state. The caller controls scheduling and work budgets.

```rust
let mut pending_sweeps = Vec::new();
if let Some(mut sweep) = cache.sweep_missing() {
    let progress = sweep.step(64, None)?;
    if progress.state == MaintenanceState::More || progress.state == MaintenanceState::Busy {
        pending_sweeps.push(sweep);
    }
}

let mut pending_cleanups = Vec::new();
if let Some(mut cleanup) = cache.maintenance_missing(deleted_path)? {
    let progress = cleanup.step(64, None)?;
    if progress.state == MaintenanceState::More || progress.state == MaintenanceState::Busy {
        pending_cleanups.push(cleanup);
    }
}
let reclaimed = cache.check_stale_readers()?;
```

Keep these queues in application state and schedule later steps for their items.
Dropping a work object stops it; it does not finish cleanup automatically.
Existing maintenance still accepts its old cancellation argument—the proposal
only introduced `Canceler` for the new options structs. Unifying maintenance's
options is not specified yet.

Removing `LoadedFile::maintenance()` also removed the documented client path for
pruning older generations of a still-existing file. A replacement such as
cache-level pruning is undecided. No example calls the previously discussed but
unapproved `cache.prune(path)` method.

## 21. Verify without retaining source text

Use metadata to find a candidate, then verify directly from disk:

```rust
let metadata = SourceFile::open(root.join(path))?.metadata()?;
let candidate = loader.preview(
    path,
    metadata.byte_len,
    metadata.mtime,
    &grammar,
    LoadOptions::default(),
)?;
let confirmed = match candidate {
    Some(candidate) => match candidate.verify_file(VerifyOptions::default())? {
        Verification::Confirmed(tree) => Some(tree),
        Verification::Changed(_) => None,
    },
    None => None,
};
```

This cache-only example declines changed candidates, including alternative exact
hits. A client willing to use those can handle `Changed(LoadResult::Loaded(tree))`.
`verify_file` reopens the request path and checks the actual capture's metadata;
streamable preprocessing feeds XXH3 directly without constructing a source buffer.

If Zed decoding falls back, discard the partial hash and restart through the
buffered decoder. If verification returns a miss, capture source for parsing;
the discarded stream must be read again and match the miss's identity. Use
example 13 when source text is needed anyway. Both paths hash after preprocessing,
so they produce the same source identity. Metadata equality is only a preview
hint, never a substitute for hashing.

The standalone preprocessing crate also supports explicit encodings and Zed's
reload rules. The cache-facing opaque type currently has only `none()` and
`zed()` constructors; examples for explicitly selected cache profiles await
that API extension.

## 22. Metadata, contents, and standalone hashing

```rust
let mut file = SourceFile::open(root.join(path))?;
let observed = file.metadata()?;
let flag = AtomicBool::new(false);
let fingerprint = file.hash(ReadOptions {
    cancellation: Canceler::new(&flag),
    ..Default::default()
})?;
// Later reads start at byte zero and can observe newer file contents.
let contents = file.read(ReadOptions::default())?;
let recomputed = hash_source(
    std::io::Cursor::new(&contents.bytes),
    Canceler::default(),
)?;
assert_eq!(recomputed, contents.fingerprint.source);
```

`observed.byte_len` and `fingerprint.metadata.byte_len` describe raw bytes;
`fingerprint.source.byte_len` describes preprocessed bytes. `hash_source` expects
already-preprocessed input and never applies the default Zed conversion again.
Use `ReadOptions { preprocessing: TextPreprocessing::none(), ..Default::default() }`
for raw bytes. `hash` avoids output storage where preprocessing streams; `read`
always returns owned bytes. Changes detected during either operation return
`WouldBlock`; cancellation returns `Interrupted`.

## Decisions exposed by these examples

- Source validation between `load` and `CacheMiss::parse` currently requires
  another hash, or a future verified-snapshot abstraction.
- Always preparing `PendingWrite` can copy source even when its handler is `drop`.
- Transfer framing must include preprocessing and raw metadata; it is not unchanged
  merely because the method names stay the same.
- `CachedCandidate` must be `Send` for speculative work on a worker thread.
  Threaded examples likewise require `CacheMiss` and `PendingWrite` to be `Send`.
- Query text providers, live-file generation cleanup, and unsaved-buffer reuse
  remain outside the currently specified API.
