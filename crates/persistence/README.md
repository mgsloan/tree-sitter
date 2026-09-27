# tree-squatter-persistence

Tree-squatter is a prototype. No data has been persisted for ongoing use;
temporary test databases do not create a compatibility obligation. All prototype
format, schema, and profile versions remain at 0. Backward compatibility and
migration support are not wanted yet: change the representation directly and
regenerate temporary caches. Tree-sitter's upstream ABI versions are independent.

The database format is a development prototype, not a released compatibility
contract. Built on Pareto commit `7734a5741`.

Uses heed 0.22.1 (pinned, default serialization features disabled), with
`Database<Bytes, Bytes>` and the explicit cache encodings. Cargo.lock pins
lmdb-master-sys 0.2.6; no direct sys-level transaction/cursor calls remain.

Cache files are `.tree-sitter/squat.mdb`, `.tree-sitter/squat.mdb-lock`, and
`.tree-sitter/squat.coop-lock` under the project root. Big-endian hosts
use `.tree-sitter/big-endian/` because LMDB is endian-dependent.

```rust,no_run
use std::path::Path;
use tree_squatter_persistence::{LoadContext, LoadOptions, Options, Persistence};

fn example(tree_sitter_language: &tree_sitter::Language)
    -> Result<(), Box<dyn std::error::Error>>
{
    let cache = Persistence::open(".", Options::default())?;
    let language = cache.prepare_language(tree_sitter_language, "json")?;
    let mut worker = LoadContext::default();
    let result = cache.load_with_context(
        Path::new("src/main.rs"), &language, &mut worker, LoadOptions::default(),
    )?;
    println!("{}", result.file.tree().root_node().kind());
    Ok(())
}
```

`LanguageIdentity` holds the language's name, optional version, and an XXH3
hash of its generated tables and identity values. The name argument supplies a
fallback for ABI < 15 grammars. The hash does not cover
native lexer or external scanner code, so clients must invalidate the cache when
those implementations change.
`LanguageIdentity::new_with_version` and `Persistence::prepare_language_with_version`
also accept a fallback version for grammars without embedded metadata.
Runtime identity is currently a conservative build-time digest of native sources
and build inputs. Squatter exports its actual compiled layout configuration.

Implemented:

- Exact owned disk-byte capture and source hashing; path validation.
- LMDB metadata, source contents, and compact slabs published in one synchronous
  transaction. Presence and point sidecars use separate databases and can be
  published later. Source generations and grammar variants coexist.
- `LoadedFile::evict_sidecar` deletes presence or points independently, preserving
  the core, other sidecar, and existing readers. Subsequent loads or publishers
  can rebuild evicted presence data; missing requested points require reparsing.
- `LoadOptions::pack` selects side data on both cache hits and misses. The simple
  `load` method uses the side-data defaults in `Options`. Points affect grouping,
  so point-enabled and point-free trees use separate cache variants.
- Publication compacts used columns directly into heed `put_reserved` storage,
  including envelope and initialized padding, without an intermediate compact
  tree or combined value buffer. Misses retain spare capacity until publication;
  deferred/disabled writes avoid eager compaction. The caller's tree is unchanged.
- Owned cache hits; shared `LoadedFile` values survive publication and cache drops.
- Opt-in `Options::read = ReadPolicy::PreferTransactionBacked` retains an LMDB
  snapshot for aligned cache slabs. Misaligned hits and local reader pressure use
  owned copies. `LoadedFile::transaction_backed` reports the actual storage mode;
  `detach` copies without invalidating aliases. Sources remain owned disk captures.
- Structural safety loading of the core, with cheap sidecar dimension checks
  and point-delta overflow checks, plus debug content checks; no slab checksum. Node source bounds are checked
  before returning the pair. See [the validator audit](validation.md).
- Parser reset, whole-file ranges, cancellation checks and no partial publication.
- Inline, deferred, and disabled writes. Deferred work retains no transaction.
- `open_existing` avoids foreground cache creation; `WritePolicy::Transfer`
  returns captured publication work even before a cache exists. Bounded,
  same-build transfer decoding validates identity and structural safety before
  publication. Frames carry compressed points from the original packing; presence
  can be rebuilt from the transferred core. Transfer frames are an IPC format,
  not a durable schema.
- Per-worker `LoadContext` reuses parser and packing scratch across grammar
  changes, including resumable loads. Packing contexts are allocated only on misses.
  Prepared grammars share immutable tables across workers and trees; callers retain
  grammar handles between batches. `LoadContext::trim` releases packing scratch.
- Parse-table-derived supertype dictionaries are stored once per grammar/runtime
  in LMDB. `prepare_language` restores them directly from borrowed transaction bytes
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
  Cleanup revalidates its target before each batch.
  Missing-file cleanup retains a per-path retirement marker to reject deferred
  writers captured before cleanup, including across delete/recreate cycles.
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
- Durable canonical fixtures, a real generated grammar hash fixture,
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

The current schema includes the grammar dictionary database. Schema and transfer
format versions are 0; no migration is provided.
