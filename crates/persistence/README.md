# tree-squatter-persistence

Initial implementation of [the persistence design](../../tree-squatter-persistence.md).
The database format is a development prototype, not a released compatibility
contract. Built on Pareto commit `7734a5741`.

Uses heed 0.22.1 (pinned, default serialization features disabled), with
`Database<Bytes, Bytes>` and the explicit cache encodings. Cargo.lock pins
lmdb-master-sys 0.2.6; no direct sys-level transaction/cursor calls remain.

```rust,no_run
use std::path::Path;
use tree_squatter_persistence::{GrammarFingerprint, LoadContext, LoadOptions, Options, Persistence};

fn example(language: &tree_sitter::Language, fingerprint: GrammarFingerprint)
    -> Result<(), Box<dyn std::error::Error>>
{
    let cache = Persistence::open(".", Options::default())?;
    let grammar = cache.prepare_grammar(language, fingerprint)?;
    let mut worker = LoadContext::default();
    let result = cache.load_with_context(
        Path::new("src/main.rs"), &grammar, &mut worker, LoadOptions::default(),
    )?;
    println!("{}", result.file.tree().root_node().kind());
    Ok(())
}
```

Grammar providers supply an exact implementation fingerprint through `Grammar`.
Runtime identity is currently a conservative build-time digest of native sources
and build inputs. Squatter exports its actual compiled layout configuration.

Implemented:

- Exact owned disk-byte capture and source hashing; path validation.
- LMDB metadata, source contents, and compact slabs published in one synchronous
  transaction. Source generations and packing/grammar variants coexist.
- Publication compacts used columns directly into heed `put_reserved` storage,
  including envelope and initialized padding, without an intermediate compact
  tree or combined value buffer. Misses retain spare capacity until publication;
  deferred/disabled writes avoid eager compaction. The caller's tree is unchanged.
- Owned cache hits; shared `LoadedFile` values survive publication and cache drops.
- Opt-in `Options::read = ReadPolicy::PreferTransactionBacked` retains an LMDB
  snapshot for aligned cache slabs. Misaligned hits and local reader pressure use
  owned copies. `LoadedFile::transaction_backed` reports the actual storage mode;
  `detach` copies without invalidating aliases. Sources remain owned disk captures.
- Structural safety loading without recomputing auxiliary-index membership or
  checking canonical auxiliary padding; no slab checksum. Node source bounds are
  checked before returning the pair. See [the validator audit](validation.md).
- Parser reset, whole-file ranges, cancellation checks and no partial publication.
- Inline, deferred, and disabled writes. Deferred work retains no transaction.
- `open_existing` avoids foreground cache creation; `WritePolicy::Transfer`
  returns captured publication work even before a cache exists. Bounded,
  same-build transfer decoding validates identity and structural safety before
  publication. Transfer frames are an IPC format, not a durable schema.
- Per-worker `LoadContext` reuses parser and packing scratch across grammar
  changes, including resumable loads. Packing contexts are allocated only on misses.
  Prepared grammars share immutable tables across workers and trees; callers retain
  grammar handles between batches. `LoadContext::trim` releases packing scratch.
- Parse-table-derived supertype dictionaries are stored once per grammar/runtime
  in LMDB. `prepare_grammar` restores them directly from borrowed transaction bytes
  into owned tables, without an intermediate byte buffer or retained transaction.
  Missing/invalid dictionaries fall back to computation. Tree and dictionary
  publication is atomic. Linear symbol and direct-field tables remain process-local.
- Nonblocking writer admission for cooperating processes/threads; map-full,
  unavailable cache, and malformed entries fall back to a freshly parsed pair.
- Linux parse-work ownership with crash-released locks, bounded cancellable waits,
  and resumable `load_step`/`PendingLoad`. Deferred loads retain captured bytes,
  not a parser or transaction; `parse_now` explicitly bypasses contention.
- Optional bounded generation cleanup via `LoadedFile::maintenance`, deleted-path
  discovery via `Persistence::sweep_missing`, and explicit stale-reader checks.
  Cleanup revalidates its target and cancellation rolls back the active batch.
- One process-lifetime environment per directory inode on Unix (canonical path
  elsewhere). No slab temporary files. Retained directory handles anchor Linux
  application-side checks/sidecar access, but heed canonicalizes the environment
  path before LMDB opens it: LMDB file opening is not directory-handle anchored.

Remaining before the full design is implemented:

- Broader validator fuzzing and review of remaining conservative structural
  invariants. The auxiliary semantic checks are now separate from cache loading.
- Tentative and chunked APIs, including transaction-owned cached source views.
- Capacity/age-based eviction policy. Maintenance is caller-driven; a full map
  skips publication instead of automatically cleaning up or resizing.
- Durable canonical fixtures, a real generated grammar fingerprint fixture,
  cancellation/commit fault injection, fuzzing, and platform power-loss testing.
- Complete Windows/macOS and adversarial path-opening validation. Current cache
  directories must be trusted, use local filesystems, and have cooperating writers.
  Parse-work deferral currently bypasses coordination outside Linux.
- Bounded environment-registry retirement and controlled map growth. The first
  opener's map size governs shared instances; handles stay alive until process exit.
- Configurable snapshot admission and reader-age/map-usage diagnostics. Currently
  at most 32 transaction-backed owners are admitted per local environment; clones
  share one slot. Other processes have their own admission counts, so LMDB's
  global reader limit can still force cache fallback. Long-lived snapshots delay
  reuse of retired pages across the entire environment, not just their tree.

Zed may use this raw-byte cache only when its loaded parser input is byte-for-byte
identical to the disk capture. Transformed buffers bypass cache reuse/publication.
Saving CRLF files does not necessarily make them eligible.

Run `cargo test -p tree-squatter-persistence` for lifecycle, codec, cooperation,
and maintenance tests.

Schema version 3 adds the grammar dictionary database. Version 2 caches are
intentionally rejected rather than upgraded in place. The earlier backend
migration probe therefore applies only to retained version 2 builds.
