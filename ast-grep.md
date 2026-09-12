# ast-grep with Squatter and a shared parse cache

Status: implementation specification, 2026-09-11. This document proposes changes;
the integration and cache do not exist yet.

Target checkout: `/home/mgsloan/oss/ast-grep`, inspected at
`fc2b1530db74de49131b725221de98036a552a9f`. Squatter checkout: this repository,
at `bbb0966676d9e6ae2e79f320fb6a5408ff7bcf42`, with local work in progress.
Pin the actual implementation revisions before starting performance comparisons.

## Outcome and scope

Make ast-grep's native CLI search and scan paths consume immutable Squatter trees.
Persist each tree beside its project-relative path under
`.tree-squatter/PATH/VER.squat`, where `VER` concisely identifies the grammar,
Squatter representation, packing flags, and parse options. A hit avoids parsing
and packing the source again, and processes mapping the same entry can share its
file-backed pages. Source changes atomically replace the current variant; identity
changes publish an additional path for the same source generation. When source
contents change, the next writer retires all old-generation variants while existing
readers retain their mappings. Existing ast-grep match semantics, output,
diagnostics, and rewrite results remain the correctness reference.

The cache is expendable and failure must fall back to normal parsing. It contains
parsed source trees, not source text, query results, or compiled rules. Stdin,
unsaved buffers, injected ranges, network filesystems, and caches outside the
project are out of scope for the MVP.

Deliver native CLI UTF-8 host documents first. Keep injections, interactive LSP
editing, N-API UTF-16, WASM, and Python bindings on their existing backend. Verify
rewrite compatibility before enabling cached documents in mutation paths.

## Findings in the current code

Paths in this table are relative to the indicated checkout.

| Location | Finding and required work |
| --- | --- |
| ast-grep `crates/core/src/source.rs` | `Doc: Clone + 'static` has a node associated type implementing `SgNode`. This is the adapter boundary. `children()` requires an `ExactSizeIterator`; edit support is part of `Doc`. |
| ast-grep `crates/core/src/matcher.rs`, `match_tree/` | Matching is generic over `Doc`. Keep its semantics; do not translate ast-grep patterns into Tree-sitter query syntax. `Matcher::potential_kinds()` is a possible later scan optimization. |
| ast-grep `crates/core/src/tree_sitter/mod.rs` | `StrDoc` publicly owns `String`, language, and a native `Tree`. It already reuses a thread-local parser per language. Editing calls `Tree::edit` and reparses incrementally. |
| ast-grep `crates/core/src/tree_sitter/traversal.rs` | `Visitor`, traversal algorithms, and cursors have concrete `StrDoc`/Tree-sitter dependencies despite the generic matcher. These need adaptation too. |
| ast-grep `crates/cli/src/utils/mod.rs` | `read_file`, `filter_file_rule`, and `filter_file_pattern` read source and construct documents. Put explicit cache lookup here, after source/language selection; avoid hidden filesystem I/O in every `StrDoc::new`. |
| ast-grep `crates/cli/src/print/`, `scan.rs`, `run.rs` | Document and match aliases are concrete. Generalize the output/rewrite boundary or use the common CLI document below. Preserve ordering and formatting. |
| ast-grep `crates/language/src/html.rs`, `crates/cli/src/lang/injection.rs`, `crates/dynamic/src/lib.rs` | Injection discovery accepts `Node<StrDoc<L>>`. Discovery must work against packed host trees to avoid reparsing on a hit. Each injection entry is an independent parse, even when languages repeat. |
| ast-grep `crates/cli/src/utils/debug_query.rs` | Debugging uses native nodes directly. Pattern parsing and native debug-tree output can stay native. |
| Squatter `crates/squatter/src/lib.rs`, `traits.rs` | Has packing, serialization, borrowed loading, nodes, cursors, and preorder iteration. `Tree` is `Send + Sync`, but ownership of mappings still needs a wrapper and audit. Existing `Children` is not an exact-size iterator. |
| Squatter `lib/squat/index.c` | Borrowed loading validates every node and the presence index. No copy does not mean lazy loading. |
| Squatter `lib/squat/README.md` | Version-4 slabs are native-endian, have optional point columns, require the exact grammar, and contain no grammar fingerprint. Known field-lookup and seek differences must be addressed for ast-grep. |

### Dependency prerequisite

ast-grep currently requests Tree-sitter 0.27 and Rust 1.88. This Squatter workspace
uses the local Tree-sitter 0.28 runtime, whose library requires Rust 1.90.
Tree-sitter declares `links = "tree-sitter"`; do not attempt to link two runtime
versions or cast between incompatible Rust wrapper types.

For the prototype, upgrade ast-grep to the pinned local runtime and Rust 1.90,
with all consumers resolving one Tree-sitter package. Verify with the Cargo
dependency tree and a workspace build before introducing packed documents.
Squatter's packer includes Tree-sitter private headers, so public grammar ABI
compatibility alone is insufficient: its C build must match the linked runtime.

For distribution, make `tree-sitter-squatter` independently consumable: it is
currently `publish = false`, and its build script reaches outside its crate to
`../../lib/squat`, `lib/src`, and `lib/include`. Package the required sources or
provide a pinned supported distribution. Do not commit developer-specific absolute
path dependencies as the release solution. If preserving ast-grep's current MSRV
and runtime is required, port Squatter against 0.27 as a separate compatibility
task instead of assuming a dependency substitution will work.

## Document and node integration

Add an optional Squatter feature and a new native CLI document type, provisionally
`CachedDoc<L>`. Preserve existing public `StrDoc<L>` and its native `tree` field.
The new document owns a source `String`, language, parse context, and a tree enum:

```rust
enum DocumentTree {
    Native(tree_sitter::Tree),
    Packed(Arc<PackedOwner>),
}

enum CachedNode<'a> {
    Native(tree_sitter::Node<'a>),
    Packed(tree_sitter_squatter::Node<'a>),
}
```

These are API sketches, not compilable declarations. Implement `Doc` and `SgNode`
with an enum dispatch that permits per-file native fallback. Keep pattern
compilation native: ast-grep's compiled `Pattern` is already representation
independent. Compare enum overhead before considering more extensive static
specialization.

Required adapter behavior:

- Implement every `SgNode` method, including `field_children`, sibling sequences,
  ancestor order, named leaves, missing/error/extra flags, and exact child counts.
  Do not inherit generic defaults without checking their ordering against the
  current native implementation. Supply an exact-size child iterator with a
  maintained remaining count; avoid repeatedly counting all children.
- Use Squatter's physical-slot traversal for preorder scans. Keep symbol reads
  narrow; do not fetch a bulk coordinate snapshot to answer `kind_id()`.
  Use cursor ancestor stacks for structured traversal where possible. Repeated
  independent `parent()` calls can scan backward and become expensive.
- Node IDs need stability and uniqueness within a document; slots suffice for
  packed nodes. Identity across documents also requires the document identity.
  Do not serialize process pointers or treat equal slots in different trees as
  the same node. Audit existing ID comparisons and `Root::adopt` checks.
- Preserve byte offsets and point semantics; enable Squatter's `points` feature
  initially. ast-grep converts byte columns to character columns using the source.
  Include CRLF, Unicode, and zero-width nodes in differential checks.
- Cursor allocation failure must not silently end iteration or remove matches.
  The existing infallible trait requires either a documented allocation-failure
  policy consistent with the rest of ast-grep or a fallible setup/fallback path.
- Move display-context helpers to a generic UTF-8 document extension. Adapt
  visitor traversal, CLI aliases, printers, and diff generation to the new type.
  Maintain native-only public conveniences as wrappers.

`PackedOwner` owns either an allocated packed tree or the mapping plus the runtime
descriptor that borrows its slab. It must also retain any dynamic grammar library
handle. Add an owning mapped-tree abstraction in the Squatter crate, where raw
descriptor creation and destruction can be encapsulated: destroy the descriptor
before unmapping; nodes borrow the owner; clones share an `Arc`; no public
`BorrowedTree<'static>` or lifetime transmute. Audit thread safety, drop order,
file-handle lifetime on Windows, and all exposed mutation methods.

The initial source `String` is still private to each process and cloned according
to existing document behavior. Sharing tree pages does not share source buffers
or eliminate reading/hashing source. Sharing source snapshots and reducing string
clones are separate optimizations.

### Injections, edits, and compatibility

Introduce a backend-neutral injection hook accepting a generic UTF-8 `Doc`.
Retain the old native hook as a compatibility entry point. Update all bundled and
CLI-configured injection implementations to use the generic implementation for
both document types. Legacy third-party language implementations without this
capability must choose native fallback for the host; never silently omit
injections. The API review must make this capability explicit rather than infer
support from an empty discovery result.

On a packed host hit, rerun injection discovery against that host, then look up
each child tree using its exact included ranges and grammar. Persist included
ranges in the cache envelope/parse context because Squatter does not retain that
metadata itself. Preserve independent entries, original source coordinates, and
multiple disjoint ranges. Reset included ranges when reusing a parser for a
whole document; the current parser cache must not leak injection configuration.

Ordinary CLI rewriting does not require mutating the parse tree: the current
`crates/cli/src/print/interactive_print.rs::apply_rewrite` constructs replacement
source from match ranges, and `rewrite_action` writes that source to the file.
It can use an immutable cached tree throughout. The `do_edit` requirement below
preserves the core library's `Root::edit`/`Root::replace` API for callers that
continue using a document after modifying it; it is not a prerequisite for basic
CLI fixes. The inspected LSP `on_change` path also constructs a fresh document
from the editor's supplied text rather than calling this incremental-edit API.

For `do_edit`, build the new source and tree privately, then swap document state
only after success. A packed document cannot provide Tree-sitter's incremental
parse state. Initially, freshly parse the edited source and transition that
document to the native backend; subsequent edits may be incremental. Original
clones retain their old packed owner. Apply the same included-range semantics as
the existing path, or explicitly select native documents before injected edits.
Never patch a published cache slab. Publish a saved version only from the actual
saved bytes, and preserve ast-grep's file-change checks before applying rewrites.

Squatter's documented field-lookup and seek differences are not acceptable merely
because its own comparison suite labels them expected. Differential ast-grep
tests decide compatibility. Fix the adapter/runtime or gate affected grammars and
operations to native mode before shipping; no silent match differences.

## Project-local persistence

Use the `tree-squatter-persistence` MVP specified in
`tree-squatter-persistence.md`. For a source at `./PATH`, its variants live in
`./.tree-squatter/PATH/`, with the expected entry at `VER.squat` and a stable
`.source` marker shared by all variants. The cache is local to the project; there
is no per-user object store, database, source-reference index, or GC service.
Project traversal must exclude `.tree-squatter`.

ast-grep remains responsible for resolving a file to an `SgLang`, including
configuration and dynamic-grammar policy. Adapt that result to a persistence
`Grammar` containing the exact `tree_sitter::Language` and a 32-byte artifact
fingerprint. Do not put ast-grep's extension registry in the persistence crate.
Generate fingerprints for bundled grammars at build time from `parser.c`, external
scanner sources, and behavior-affecting build inputs. Compute a dynamic grammar's
artifact digest once when loading it. Names, pointer equality, ABI versions, and
language semantic-version metadata are useful diagnostics but insufficient
identity checks.

At the existing path-aware CLI boundary, replace separate read/construct/cache
steps with one call:

```rust
let grammar = persistence_grammar(&lang)?;
let loaded = persistence.load(relative_path, &grammar, parser)?;
```

The call reads source bytes once, hashes that captured buffer, derives `VER`, and
opens that exact path without reading `.source` or enumerating the directory. It
validates and maps the cache when its source and full cache identities match, or
parses/packs and publishes the expected variant. The miss/update path checks
`.source`. It returns the exact source buffer and matching mapped-or-owned tree.
Cache failures return the freshly parsed tree rather than failing the ast-grep
operation.

`VER` is `v1_` plus 26 lowercase unpadded Base32 characters from a domain-separated
128-bit BLAKE3 abbreviation. Its complete input includes the full grammar and
Squatter representation fingerprints, canonical packing flags, and parse options.
The complete identities remain in the envelope, so a token collision is a miss,
not an incorrect hit. Source contents do not enter `VER`; edits replace the same
variant. Host files use persistence initially; injected ranges remain on the native
path until the identity and integration include their parse options and have
semantic parity.

Publication writes a complete temp inside the source cache directory, closes its
writable handle, then takes the per-source update lock. It rechecks the desired
source generation and variant. If `.source` still matches, it publishes
`VER.squat` and keeps every other variant. If source contents changed, it streams
the directory and deletes all direct regular `*.squat` files before atomically
advancing `.source` and publishing. Variant names are opaque during that cleanup,
so an older library can collect a newer library's unanticipated `VER`.

Different grammar or Squatter identities are incompatible, but they are not stale:
each remains useful to the corresponding tool. Normal hits and new variants for
unchanged source use direct path operations and do not enumerate the directory.
An optional future age or size policy may reclaim abandoned same-source variants;
the MVP does not infer abandonment from the existence of another `VER`.

Existing Unix mappings retain unlinked inodes and can fault cold pages after
replacement or stale-variant deletion. Windows readers use delete sharing; when
replacement is blocked, ast-grep keeps the fresh owned tree and retries naturally
on a later update. If deleting an old generation is blocked, it leaves `.source`
unchanged and postpones publication of the new generation. Readers never take the
writer lock, and envelope validation prevents a mismatched source/tree pair.

The first checked implementation verifies the payload checksum and invokes
Squatter's checked borrowed loader. This scans the slab, so it avoids reparsing but
does not yet deliver selective page-in on open. Demand-paged safe loading remains
a later Squatter milestone.

## I/O and query optimization

Default to demand mapping without whole-file prefaulting. Add a best-effort
`prefetch_ranges` abstraction: Linux/macOS `madvise(MADV_WILLNEED)` and Windows
`PrefetchVirtualMemory`. Query actual OS page size and map/view granularity;
neither is the slab's 64-byte alignment. Advice failures affect performance only.
[Apple madvise](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/madvise.2.html),
[Windows prefetch](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-prefetchvirtualmemory).

Do not prefetch every column. Profile symbols, span bases/deltas, waste metadata,
fields, and position reads separately. Squatter's physical preorder is descending,
so test its actual access pattern rather than assuming forward sequential
readahead. Compare no advice, bounded prefetch, and explicit batched reads if
cold scans stall. Do not increase every small column's alignment to a page without
measuring the storage cost across many small trees.

After semantic parity, use `potential_kinds()` to accelerate candidate enumeration
with symbol columns/presence information. Filtering may skip candidate nodes, but
must still visit descendants unless their absence is proven. Preserve public-vs-
grammar symbol mapping, aliases, errors, wildcard behavior, result order, nested
matches, and non-reentrant visitor semantics. Squatter's existing Tree-sitter
query executor is not automatically used by ast-grep's matcher.

## User controls and observability

Proposed native CLI controls, subject to existing argument conventions:

- `--tree-backend native|squatter`, initially defaulting to native.
- `--parse-cache off|project`, initially opt-in; backend and persistence remain
  independently selectable for benchmarking and fallback diagnosis.
- A project-root override if ast-grep's existing root discovery is insufficient.
  The cache directory is always `<root>/.tree-squatter` in the MVP.
- Debug statistics for hit/miss reasons. Stdin and unsaved buffers may use
  in-memory Squatter but never publish persistent objects.

Record hits/misses by reason, source read/hash time, parse/pack time, validation
time, map time, matching time, fallback reason, writer-lock wait, publication
failures, stale-variant deletions/failures, and temp cleanup. Keep these off normal
match output.
Do not call a validated-open run a lazy-open benchmark or equate summed RSS with
physical memory used across processes.

## Work packages and acceptance criteria

| Order | Deliverable | Acceptance gate |
| --- | --- | --- |
| 1 | Align runtime/MSRV; package Squatter dependency; optional feature skeleton | One native runtime; existing ast-grep workspace checks pass; feature-disabled native behavior remains available. |
| 2 | `CachedDoc`/node adapter, ownership wrapper, generic visitors/printers | Native versus freshly packed trees produce identical matches, captures, ordering, positions, JSON/text output, and fixes in the supported corpus. No mapping lifetime escapes. |
| 3 | `tree-squatter-persistence`, cache identities, `.source`, envelope, and single `load` operation | A repeated load directly maps `.tree-squatter/PATH/VER.squat` without parsing or directory enumeration; grammar/flag variants coexist for unchanged source; source changes retire all old variants; malformed entries fall back. |
| 4 | CLI language-registry and parser-pool integration | ast-grep resolves the grammar, passes its existing parser to `load`, excludes `.tree-squatter`, and preserves native behavior when disabled. |
| 5 | Per-source writer locking, atomic publication, and generation cleanup | Active readers access cold old pages after Unix replacement/deletion; source changes collect unfamiliar variants by suffix; blocked Windows operations fall back without disrupting readers. |
| 6 | Edit fallback and controlled rollout | CLI fixes never modify cached slabs; semantic parity remains; benchmark real workloads before changing defaults. |
| 7 | Later demand-paged loader and scan optimizations | Report the validation contract; selective scans reduce actual I/O without changing matches or result order. |

### Required tests

Use ast-grep's current workspace tests as the reference, extended with backend
differential tests. Cover patterns and metavariables, repeated captures,
relational rules (`inside`, `has`, `precedes`, `follows`), fields, nth-child,
strictness, anonymous/extra/missing/error nodes, empty siblings, nested matches,
Unicode/CRLF positions, malformed source, embedded languages, and rewrites.
Compare diagnostics and outputs, not just match counts. Audit all native-node
escape hatches used in production paths. Exercise deep and wide trees to detect
quadratic ancestor/child traversal.

Use deterministic subprocess barriers for lifecycle tests, not sleep-based race
tests. Required scenarios:

1. Reader A maps `.tree-squatter/PATH/VER.squat`, writer B changes the source and
   replaces that pathname, and reader A accesses previously untouched coordinate
   pages and finishes with its original source/tree pair.
2. A reader races replacement between open and mmap and sees a complete old entry,
   complete new entry, or a miss—never partial bytes or the wrong source/tree pair.
3. Concurrent writers for the same source generation but different `VER`s retain
   both complete variants. Writers for different captured source generations do
   not interleave cleanup and publication; a later load repairs a stale winner.
4. Crash during temp write leaves the old entry. Crash between generation cleanup,
   `.source` replacement, and variant publication leaves a recoverable state and
   releases the OS-owned writer lock.
5. Windows cooperative readers permit replacement where supported. A foreign or
   mapped handle that blocks replacement leaves the old entry intact while the
   writer returns its fresh owned tree and retries on a later load.
6. Source changes with unchanged size/mtime retire every old-source variant.
   Grammar parser or scanner changes without name/version changes,
   point/endian/slab changes, packing-flag changes, and a different language
   selected for unchanged source derive another `VER`; both variants remain usable.
7. Corrupt/truncated envelope and payload, overflow/alignment checks, full disk,
   unwritable cache, cache-path symlinks, missing source, and unsupported language
   never suppress matches or return a mismatched tree.
8. `Doc` clones and cross-thread use retain mappings and grammars correctly;
   editing one clone leaves the other's tree/text unchanged. Run native ownership
   tests under sanitizers and Rust lifetime/compile-fail tests for exposed APIs.
9. Project traversal excludes `.tree-squatter`; nested, extensionless, non-UTF-8,
   and already-`.squat` source names map to distinct expected cache directories.
10. A hit opens its exact variant without reading `.source` or scanning the
    directory. Adding another variant for unchanged source checks `.source` but
    also avoids a scan. A source change deletes every direct regular `*.squat`,
    including arbitrary future `VER` syntax, while ignoring subdirectories and
    non-`.squat` files. A Windows delete failure leaves `.source` unchanged and
    postpones new-generation publication.

### Performance evidence before default enablement

Measure native parse+match, uncached pack+match, checked mmap hit+match, and
explicitly labeled trusted/lazy hit+match. Separate cold filesystem cache from
warm OS cache and warm same-process ownership; opening a new process is not a
cold-cache test. Use real `sg run` and `sg scan` workloads, tiny and large files,
many files, selective and broad patterns, injections, dynamic grammars, and
parallel tools. Compare against the existing thread-local native parser cache.

Record elapsed/CPU time, source bytes read and hashed, tree bytes read, faults,
allocation peaks, packed disk size, metadata overhead, and cleanup latency.
For simultaneous tools measure PSS/shared/private memory where available; do not
sum RSS and report it as unique physical RAM. Measure source-buffer duplication
separately. Cache misses include conversion and write costs and may be slower;
quantify the reuse needed to amortize them. Candidate filtering and mmap must
earn their defaults independently.

Release gate: all compatibility and lifecycle tests pass on Linux, macOS, and
Windows; ordinary supported hits avoid native parsing; no correctness dependency
on cache availability; old readers survive cache replacement; and recorded
end-to-end measurements justify whichever defaults are selected. Numeric speedup
targets should follow those measurements, not precede them.
