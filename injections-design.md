# Forests, injections, and persistence

Tree-squatter is a prototype. No data has been persisted for ongoing use;
temporary test databases do not create a compatibility obligation. All prototype
format, schema, and profile versions remain at 0. Backward compatibility and
migration support are not wanted yet: change the representation directly and
regenerate temporary caches. Tree-sitter's upstream ABI versions are independent.


Status: implemented prototype, 2026-09-15. This document records the intended
design; the implementation notes below identify current boundaries. See
[the persistence API](crates/persistence/README.md#injections-and-derived-caches)
for usage.

Investigation: `/home/mgsloan/proj/zed`, HEAD
`ce48461eaadd16c65c31f835511ab96bd3b6e746`, including its uncommitted changes.
In particular, packed-tree publication and related tests are local additions.
References below identify the inspected working-tree code, not an upstream
release. No Zed files were changed or Zed tests run for this investigation.

## Decisions

- Squatter supports generic immutable forests, independent of injection policy.
- Trees using the same exact grammar occupy one contiguous grammar region
  within each forest blob. Persist the host tree independently of injections.
- Each tree occupies a contiguous, group-aligned node interval. Syntax edges
  never cross tree boundaries, even for nested injections of the same language.
- All grammars use the fixed-width ID layout. Compressed byte coordinates remain
  in the slab; start/end row-column points are derived cache data.
- Injection discovery, nesting, source ranges, and reuse identities live above
  the slab. An optional injection blob holds its forest and manifest, bound to
  the same captured source as the independently usable host tree. Tools may
  consume or populate either part without changing the host cache key.
- Keep native Tree-sitter and Squatter representations together. The application
  may evict native trees for buffers not receiving edits; incremental edit/reparse
  machinery is outside this design.
- Symbol-presence bitmaps are optional, per grammar region, and owned separately
  from the slab. LMDB stores them separately; their availability never changes
  the forest cache key.
- Store absolute document byte coordinates in the forest. Convert Zed's local
  byte coordinates during packing. Derive absolute start/end points from those
  bytes and the exact source; optionally cache them outside the slab and in a
  separate LMDB database. Point-cache availability never changes the tree key.
  Reconsider local storage with external placement later if measurements justify it.
- Grammar-order scanning and document-order results are separate operations.

The existing [persistence design](tree-squatter-persistence.md) covers whole-file
raw-byte parses only. This document proposes an extension; it does not silently
make existing cache entries eligible for injected or normalized input.

## What Zed actually does

### One syntax tree per layer

[`SyntaxLayerEntry`](</home/mgsloan/proj/zed/crates/language/src/syntax_map.rs:126>)
contains a depth, an anchored outer range, and either:

- a parsed tree, application language, and optional anchored included subranges;
- a pending language name, with no parsed tree yet.

The layer collection is a `SumTree` indexed using depth, source ranges, and
language identity. It is not grouped physically by grammar. Zed does not store
an explicit parent-layer ID in these entries. Its update machinery uses depth,
anchored ranges, and parent parse steps.

A forest must not replace this application index with grammar order. The adapter
needs a mapping from logical layers to forest tree IDs. Application language
configuration can differ even when layers share an exact grammar; it stays in
the layer metadata.

### Discovery and combined injections

[`with_injection_query`](</home/mgsloan/proj/zed/crates/language_core/src/grammar.rs:680>)
recognizes `content`/`injection.content` and `language`/`injection.language`
captures, plus `language`/`injection.language` and `combined`/`injection.combined`
properties. The inspected pattern configuration contains a language and a
combined flag. Do not import the standalone Tree-sitter highlighter's treatment
of `injection.self`, `injection.parent`, or child exclusion into Zed's policy.

[`get_injections`](</home/mgsloan/proj/zed/crates/language/src/syntax_map.rs:1690>)
collects content ranges from query matches. A language can come from a property
or captured text; captured paths can resolve through their final extension.
Resolution uses the language registry. Unavailable languages produce pending
layers and are revisited when the registry version changes.

For a normal injection, the outer layer range encloses the content and, when
present, the captured language name. The parse origin can therefore precede
the first included content byte.

Combined injections collect ranges by resolved `LanguageId` within one parent
layer's discovery call, including across patterns. They use the parent's outer
range as their layer range. This does not combine every occurrence of that
language in the document, and physical grouping in a forest must not change
which ranges constitute one parse.

Zed also creates steps for resolved, statically named combined languages with
no current matches, allowing existing ranges to be removed. An unavailable
language is not equivalent to a confirmed absence of injections.

### Exact parser input and coordinates

Before parsing, Zed subtracts the outer layer's start byte/point from included
ranges. [`parse_text`](</home/mgsloan/proj/zed/crates/language/src/syntax_map.rs:1623>)
provides source chunks beginning at that byte origin and passes the relative
ranges to `set_included_ranges`. The resulting tree uses layer-relative
coordinates. Query access uses `root_node_with_offset` to restore document
coordinates.

An empty included-range list would mean whole-input parsing. Zed instead supplies
one explicit zero-length range when a layer has no included content.

For combined injections,
[`insert_newlines_between_ranges`](</home/mgsloan/proj/zed/crates/language/src/syntax_map.rs:1928>)
extends a range or inserts another range to include an actual source newline
between content on different lines. It does not create synthetic newline bytes.
The final parser ranges, including these additions, belong in parse identity.
Hashing only the original content captures would miss part of the input.

[`Point` arithmetic](</home/mgsloan/proj/zed/crates/rope/src/point.rs:74>) is text
position composition, not componentwise addition. For placement `(byte, row,
column)` and local point `(r, c)`:

```text
absolute byte   = placement.byte + local byte
absolute row    = placement.row + r
absolute column = placement.column + c, if r == 0; otherwise c
```

### Representation lifecycle

Keep both native Tree-sitter trees and packed Squatter query trees when useful.
Native trees serve editing; the application may evict them for buffers not
receiving edits. How native trees are restored and incrementally updated is
application policy, outside this forest/persistence design. Squatter does not
need native incremental parse state in its persisted representation.

### Queries and packed publication

Zed creates a query cursor for each participating layer and deduplicates grammar
references in its result metadata. It filters layers by the requested document
range and applies query-specific options. Capture streams are merged by
`(start_byte, Reverse(end_byte), depth)`; match streams use their first/last
capture extents and depth. See
[`SyntaxMapCaptures`](</home/mgsloan/proj/zed/crates/language/src/syntax_map.rs:1211>)
and the [ordering keys](</home/mgsloan/proj/zed/crates/language/src/syntax_map.rs:1484>).
The forest adapter must preserve these rules, predicates, and within-layer
query ordering, including ties. Grammar-contiguous storage alone cannot emit
this document order.

The local
[`pack_trees`](</home/mgsloan/proj/zed/crates/language/src/syntax_map.rs:352>)
work packs a cloned layer collection and swaps it only after completion and
cancellation checks. Publication checks text/parse/interpolation versions,
registry version, and syntax update count. Its `retain_reparse_data` option is
part of the local runtime experiment, not a capability implied by persisted
Squatter slabs.

Replacing the representation must also refresh downstream cached snapshots
without producing a semantic edit. The checkout's
[publication investigation](</home/mgsloan/proj/zed/packed-snapshot-publication.md>)
identifies this integration concern. Old snapshots must remain valid for their
existing readers.

## Generic forest representation

```text
forest
  grammar A region: [tree 0 groups][tree 1 groups][tree 2 groups]
  grammar B region: [tree 3 groups][tree 4 groups]
```

These are intervals within shared columns, not ordinary child relationships.
An outer JavaScript injection and an inner JavaScript injection may occupy
neighboring intervals while an intervening HTML layer occupies another region.
The host tree is stored separately for independent cache consumption; an optional
assembled in-memory forest can also group it with same-grammar injections.

### Descriptor tables

Proposed logical fields, not a frozen ABI:

| Record | Fields | Meaning |
| --- | --- | --- |
| Forest header | version/configuration, table counts/offsets, group count, column/auxiliary locations | Bounds and interpretation of the allocation |
| Grammar region | `grammar_index: u32`, `first_tree: u32`, `tree_count: u32` | Exact grammar binding and consecutive independent trees |
| Tree | `first_group: u32`, `group_count: u32` | Group-aligned storage interval and traversal boundary |

Grammar-region group bounds follow from its first and last trees. Tree node
bounds follow from group size; the root follows from the final occupied slot
under the current reverse-preorder encoding. No duplicate root or boundary table
is needed unless measurements justify one.

Serialize fields explicitly in little-endian form. Counts/offsets require checked
arithmetic and the format's existing alignment rules. Grammar indices select
caller-supplied exact bindings; memory pointers and Zed's process-local language
IDs are not persistent identities. The owner retains prepared grammar handles.
Persisted grammar fingerprints belong in the surrounding persistence envelope.

Tree IDs are descriptor indices within a snapshot. Physical node IDs identify
slots, including possible group waste; wasted slots do not produce nodes. Neither
ID is stable across rebuilding/reordering the forest. Application layer IDs map
to tree IDs separately. A main/root document tree is selected by the application,
not implicitly by the first tree in grammar order.

Validation establishes that regions partition the tree table and trees partition
the used groups, each grammar binding is valid, and topology stays inside each
tree. Root parent/sibling navigation terminates at tree boundaries. Existing
column, index, and coordinate safety checks still apply. Forest ownership keeps
all views alive; it does not allow an individual view to free shared storage.

### Auxiliary data

Uniform widths do not imply globally meaningful IDs. Symbols and fields remain
grammar-local. Prepared grammar tables and supertype dictionaries can be shared
per exact grammar. Per-node supertype encodings still need that grammar's
interpretation. Grammar-symbol overrides depend on tree contents and require
explicit tree or region scope and index relocation when copied.

### Optional symbol-presence allocation

Provide per-grammar-region symbol-presence bitmaps in a separate allocation,
owned independently of immutable forest columns. For each public symbol, a
bitmap has one bit per physical group in that region. Set bits conservatively
identify groups that may contain the symbol; a clear bit permits skipping.
Define alias/public-symbol mapping consistently with query candidate selection.
Supertype queries must expand or index their requirements consistently too.

The forest works without this cache. It can be loaded and queried immediately,
then acquire a cache loaded from LMDB or built from its symbol columns. Cache
construction and publication are optional and cancellable. A partial bitmap
must never be treated as complete: publish an immutable completed region cache,
with absence represented explicitly rather than by an all-zero bitmap.

Use a runtime cache owner associated with the exact forest snapshot. Attach
completed region entries atomically or under a short lock; a cursor retains the
entry it uses. Do not mutate bitmap words under readers or retain mutable state
inside the slab. Dropping the cache leaves the tree valid and changes only
performance. Host and injection blobs have independent presence-cache owners. Exact attachment/eviction API remains to be selected.

LMDB gets a separate presence database. A proposed logical key is:

```text
owning tree/forest blob identity + region identity + presence-format version
```

The owning blob identity includes source and parse/representation identities,
but no presence flag, presence-format version, or build/load policy. The sidecar
also binds the exact region geometry and grammar interpretation. Forest assembly
must be deterministic for a given entry identity, or identity must additionally
bind its tree ordering/layout; group-indexed bitmaps cannot attach to a differently
ordered forest under the same key. Never key a bitmap by grammar alone.

Validate sidecar identity, dimensions, lengths, and access bounds before use.
Missing, incompatible, or rejected records fall back to ordinary scanning.
This retains persistence's existing safety-only corruption policy; bounds checks
alone cannot prove the semantic correctness of bitmap contents.

Publish presence records independently, including after the forest transaction.
They are disposable derived data, not authoritative references required for a
complete forest/manifest. Cleanup may delete them independently and must tolerate
late sidecar writers without resurrecting retired authoritative entries. Loading
an owned sidecar must not accidentally keep a large LMDB snapshot alive.

This supersedes the existing design's treatment of `symbol_presence` as a
persisted tree-variant option for the new forest format. Grammar-symbol overrides
and other data needed to interpret nodes remain authoritative slab data; only
presence and derived point data move out. The implementation still needs these
splits and a format/schema change.

### Optional start/end point allocation

Remove per-node start/end point columns and their per-group bases from the core
slab. Retain absolute start/end byte offsets. Document points are derived from
those offsets and the exact immutable source, so cached points are an optional
acceleration structure, like symbol presence. They are not another tree variant.

For the current raw-byte source convention, use zero-based rows and byte columns:
row is the number of LF bytes strictly before the offset; column is the offset
minus the start of that row. Offset zero and EOF are valid, including EOF after
a final newline. Do not normalize CRLF/BOM/encoding while building this cache.
The document-point convention is versioned; supporting other input encodings or
coordinate units requires an explicit source/coordinate profile.

A source owner can share a line-start index between the host and all injections.
A point lookup without cached per-node values derives its result from the byte
offset and source/line index. A caller can subsequently build or load a separate
allocation of start/end points indexed by the owning blob's physical slots.
Ignore wasted slots. Whether that allocation uses absolute values or compressed
point columns is an independent cache-format decision; it cannot change core
packing groups or node IDs.

Point APIs must return the same document positions with and without the cache.
The runtime therefore needs a source-aware access context, supplied by the
persistence owner or a generic immutable byte-to-point provider. A source-less
forest remains useful for byte-only queries, but must explicitly require such a
context for point access; it must not silently report `(0, byte_offset)` as a
substitute. Exact signatures remain open. Point-based query restrictions and
returned point attributes use the same resolver, even if a query kernel chooses
to materialize the cache before execution.

Bind a persisted point cache to:

```text
owning tree/forest blob identity + source generation + point-format/profile version
```

The node layout and exact source must both match. Source identity already present
in a blob key need not be duplicated in its physical key encoding. Store points
in a separate LMDB database and allocation; neither cached-point presence nor
its encoding/version enters the host or injection-tree key. The new core format
also removes `points` as a persisted packing option. Parser-supplied included-range
points still belong to the parse request/identity: they can influence parsing,
which is distinct from caching document points for resulting node offsets.

Use the same completed immutable attachment, cancellation, ownership, validation,
and independent publication/cleanup rules as presence caches. Missing or rejected
point caches fall back to source-derived points. Do not expose partially built
entries as complete. Point cache eviction preserves semantics and does not force
node/slab replacement. Persisted point contents retain the existing safety-only
corruption policy; identity/bounds validation is not semantic verification.

After relocation or reassembly, derive points using the new absolute byte offsets
and new source. Old point caches are not reusable merely because syntax is
unchanged. Rebuilding points also handles the first-line column complication of
moving a layer, without changing core node deltas or packing groups. Native
parsers may accept arbitrary supplied point coordinates; this document-coordinate
profile exposes source-derived positions, not arbitrary custom coordinates.

## Injection metadata outside the slab

The injection adapter retains, as needed:

- logical layer identity and its forest tree ID;
- application language/configuration and parsed versus pending state;
- outer discovery range, depth, and placement byte/point;
- final included ranges, distinct from the outer range;
- parse reuse identity and discovery dependencies.

An explicit injection-parent ID is optional adapter metadata. It can express
which discovery step produced a layer and help invalidate descendants. It is
not needed for generic queries or syntax navigation, and Zed's current entries
manage without it. Do not force Zed to adopt a parent graph merely to use a
forest. Combined layers and pending entries must remain representable.

Zed anchors and buffer versions are runtime state. A persisted manifest stores
source-relative ranges and a source generation; the adapter reconstructs anchors
against the confirmed buffer snapshot. Physical tree order need not match layer
order. Several logical consumers may reference one stored tree if their exact
parse inputs agree.

## Placement and reuse

Decision: store absolute document byte coordinates in packed trees and derive
absolute document points from the source, optionally caching them separately.
Zed may keep its existing local-coordinate native parses. Convert byte positions
when packing using the layer origin. Node access, source slicing, text predicates,
query restrictions, and returned captures use document coordinates, with points
resolved through the source-aware context. Do not apply Zed's origin again to
forest nodes. Point conversion does not affect core packing geometry.

For example, a token at local byte 5 in a layer placed at byte 100 is stored at
byte 105. If an equivalent layer moves to byte 200, reassembly shifts that tree's
byte bases by 100. Assembly already copies columns; base adjustment can be a
small additional cost. Keep any origin needed to identify the native parse or
reproduce discovery in the adapter manifest, not as an obligatory node view.

Alternative to reconsider: preserve local coordinates and attach placement at
access time. That can make whole-layer movement metadata-only and permit reuse
of unchanged backing storage. It also requires translated coordinate access,
text predicates, range filtering, and captures throughout the query path. Revisit
it if measurements show relocation/reassembly costs outweigh those costs; it is
not the initial representation or an additional initial format mode.

Group alignment isolates each tree's byte bases. A uniform byte displacement
can update only its start/end byte bases. Row/column changes are handled by
source-derived point lookup or rebuilding the separate point cache. In particular,
a changed origin column does not require changing packed node deltas or groups.
This removes the point-specific repacking cost of the earlier inline-point design.

Disjoint included ranges moving independently cannot use one displacement,
even with local-coordinate storage. Nodes can span gaps. Copying a whole tree preserves relative
subtree spans; other forest/global indexes may need rebuilding or remapping.
Relocation never mutates a live immutable forest. Checked arithmetic rejects
overflow. The adapter establishes equivalent parsing behavior; coordinate
conversion alone does not establish reuse eligibility.

There are two identities:

1. **Parse identity:** exact grammar/runtime, parser input and coordinate frame,
   final ordered included ranges, and persisted representation options.
2. **Discovery identity:** injection queries, their evaluation behavior,
   application language resolution/configuration, and relevant parent context.

If discovery runs again, reuse an individual tree when its parse identity agrees;
query text need not invalidate that tree merely because a different query found
it. Skipping discovery requires validating the manifest's discovery dependencies,
including pending languages becoming available. Zed's registry counter is useful
within a process but is not a durable registry fingerprint.

Start with full captured-source identity and exact parse options. Cross-generation
reuse based on included-byte hashes is a later optimization: preserve boundaries,
newline additions, local points, and all scanner-observable input dependencies.
Do not assume concatenated capture bytes alone completely determine parsing.
Hash final included content directly before introducing a range-hash structure;
a whole-file digest alone does not supply arbitrary substring digests.

## Shared injection behavior across tools

A generic forest alone does not give tools the same parse requests. Shared cache
identity is necessary to reject mismatches, but shared behavior is needed to
actually produce cache hits. Prefer a common injection engine consumed by Zed
and other tools rather than independent implementations of a prose contract.

Implement the shared engine in a dedicated `tree-squatter-injections` workspace
crate at `crates/injections`. `tree-squatter-persistence` depends on it. The
injections crate depends on the native Tree-sitter/Squatter APIs, not persistence,
LMDB, or Zed. Tools may use it directly without enabling disk caching.

```text
tree-squatter-persistence → tree-squatter-injections → Tree-sitter / Squatter
```

Injection parse-request and manifest types/encodings belong in the injections
crate. Persistence supplies storage, source-generation binding, and publication;
Zed supplies its application adapter. Shared identity types needed by both crates
must live below persistence so this dependency stays acyclic.

The shared engine owns:

- query capture/property conventions and their versioned interpretation;
- content-range collection, duplicate handling, outer parse-origin selection,
  and deterministic ordering;
- single versus combined grouping, including grouping scope within each parent;
- Zed-compatible inclusion of source newlines and explicit empty-range handling;
- recursive discovery over an immutable source snapshot, with a common policy
  for termination/resource limits and explicit incomplete outcomes;
- canonical parse-request and discovery-manifest encodings, including unresolved
  language requests, identities, and deterministic forest assembly order.

Tools supply grammar implementations, injection queries, and a resolver through
small interfaces. For cross-tool hits, resolver naming/extension rules, aliases,
query predicates, query bundles, and available grammar versions must agree or
carry distinct identity. Reproduce Zed's resolution behavior through a shared
profile/implementation where feasible; an arbitrary callback is an escape hatch,
not a guarantee of compatibility. Never persist Zed's process-local LanguageId
as the portable grouping or resolution identity.

Keep application registries/loading, GPUI, Rope/Anchor ownership, language UI
configuration, buffer versions, snapshot publication, native-tree eviction,
and incremental edit processing in Zed. The engine operates on immutable bytes
and canonical ranges. Its query/tree access abstraction must permit native and
packed inputs without making discovery semantics depend on the chosen backend.

A first implementation should implement full-snapshot discovery. Zed's existing
incremental machinery may continue independently, but publishing its results
under the shared profile requires conformance to the full-discovery output.
Tests should compare canonical requests/manifests produced by both paths. Do not
move range-splicing/edit tracking merely to obtain an initial shared cache.

Exact grammar/runtime identity is insufficient to validate a saved injection
manifest: discovery-policy and query/resolver dependencies are also required.
A policy change that produces the same parse request can still reuse that
individual tree after rediscovery. It cannot blindly reuse an old manifest.

### Included-range contract

Included ranges describe one parse, not forest membership. An ordinary injection
match may yield several content ranges for one tree; combined injections can
collect ranges across matches within the same parent and resolved language.
Grouping independent trees into a grammar region never combines their parses.

The injections crate owns the final ordered, nonoverlapping parser ranges,
including byte and point endpoints, newline additions, and the parse origin.
Keep range boundaries even when adjacent; scanners can observe them. Preserve
Zed-compatible local parser coordinates, then convert packed byte coordinates to
absolute document offsets and derive document points from the source. Canonical request/manifest encoding specifies the
coordinate frame explicitly and retains enough information to reproduce the
exact native parser request. Validate range bounds against the shared immutable
source and check coordinate conversions before invoking the parser.

Use an explicit whole-source mode distinct from a ranges mode. Native
`set_included_ranges([])` means whole input; a no-content injection instead uses
Zed's explicit zero-length range convention. Invalid ranges must fail the request,
not leave the parser running with a previous request's ranges.

The native parser receives the original source through the selected origin,
not extracted/concatenated snippets. It returns one tree with possible gaps and
nodes spanning those gaps. Squatter accepts that tree without needing a mutable
`set_included_ranges` API or included-range fields in generic forest descriptors.
The injection manifest retains the final ranges for rebuilding and cache identity.
No particular convenience parsing API is frozen here; per-request options avoid
leaking reusable parser configuration between requests.

## Query execution

For a requested grammar, scan its region with one ID interpretation and compiled
query configuration. Candidate-symbol scanning can cross tree boundaries;
structural matching, traversal depth, anchors, and pending match state cannot.
Skip grammars with no query and trees excluded by the adapter's range selection.

Start with existing per-tree matchers behind a region iterator. Then add shared
candidate scanning and region-level presence indexes. Keep fallback decisions
per tree: an error-containing tree or unsupported query option must not disable
fast execution for every unrelated tree in the region.

Use the adapter's placement and layer metadata to merge results in Zed's expected
order. Identical grammar bindings do not guarantee identical application query
configuration. Batch only executions that agree, and measure merge/predicate work
as well as raw symbol scanning. Viewport queries may benefit more from selecting
few trees than from scanning the whole region.

## Persistence composition and cross-tool reuse

Persist three independently consumable kinds of data:

```text
source generation
  host tree                         required for a host-tree hit
  injection blob(s)                  optional, discovery-profile-specific
    injection forest + manifest
  presence cache(host, region)       optional derived data
  presence cache(injections, region) optional derived data
  point cache(host)                  optional derived data
  point cache(injections)            optional derived data
```

The host tree remains an ordinary standalone tree, or a one-tree forest if the
new format requires it. It does not become a different variant when injections
or presence/point caches become available. A tool requesting only the host need not
read injection metadata, load injected grammars, validate injection slabs, or
retain their allocation/LMDB snapshot. A tool using injections first consumes
that same host entry, whether or not an injection blob exists.

| Cache state | Host-only consumer | Injection consumer |
| --- | --- | --- |
| Host only | Use host | Use host; discover/load/parse missing injections |
| Host plus compatible injection blob | Use host | Use both |
| Host plus incompatible injection profile | Use host | Use host; build its own compatible injection blob |
| Host plus completed empty injection manifest | Use host | Skip discovery only if that manifest's profile and dependencies match |

Missing injection data means unknown/not cached, not a negative discovery result.
A complete manifest may explicitly record pending languages; completeness of the
recorded discovery pass does not mean all requested grammars were available.
Preserve unresolved requests and revalidate their resolution dependencies.

### Keys and publication

The host key contains path/source generation, host grammar/runtime identity, and
its core representation options. It contains no injection-enabled bit, discovery
profile, injected grammar set, presence-cache option, or point-cache option.

An injection key extends that host parse/source identity with a versioned
injection profile and exact discovery dependencies. Its envelope records the
injected grammar/runtime bindings and representation identity. Different query
bundles/resolvers can coexist without duplicating the host record. The exact
lookup scheme for dynamic dependency manifests is still open; a known profile
can select a candidate manifest whose full dependencies are then validated.

The injection blob contains only injected trees, grouped by exact grammar, plus
the adapter manifest mapping logical layers to its tree IDs. The manifest names
the host through an external host reference, not a node ID in this blob. Keep
that manifest outside the generic slab even when both are wrapped in one LMDB
value. An empty result has a valid manifest and no injected-tree payload.

One immutable source capture backs both artifacts. Discovering additional layers
must not reread a different file generation. Publish the host independently;
then publish a complete injection forest and its authoritative manifest together
in a later transaction. A host write never deletes or downgrades compatible
injection data. Conversely, injection publication need not rewrite the host.
Missing/malformed/incompatible injection data does not invalidate a host hit.

Late writers must revalidate source/reference state. Cleanup must not leave a
committed authoritative artifact referring to missing source records: either
skip late publication for a retired generation or atomically restore required
records from the captured generation. Live readers retain their immutable owners.
Presence and point caches can be added or removed independently of either
artifact.

Injections are optional to a host-only consumer, but are not merely an acceleration
cache for a consumer needing embedded-language results. Their absence requires
actual discovery/parsing or an explicitly incomplete result. Presence and point caches,
by contrast, affect performance only and never change required query semantics.

### Physical layout and query batching

Separating blobs gives independent loading, ownership, publication, and eviction.
It means a host tree and same-language injections are not necessarily one physical
symbol stream on disk. This is an intentional tradeoff for cross-tool sharing.

Initially, expose a logical forest over the host and injection owners. A grammar
batch may contain multiple physical regions, and query execution switches between
those segments without requiring a combined allocation. Preserve tree boundaries
and document-order result merging as before. If useful, assemble a combined
in-memory forest grouped by grammar; that is an optional optimization, not the
persistence format or a prerequisite for using cached injections.

Presence bitmaps bind their owning blob's exact group layout. Reuse them unchanged
when querying the original segments. A merged/reordered allocation requires a
correct remapping or fresh cache; attaching a bitmap based only on matching
grammar/source identity is insufficient.

### Zed integration

A shared injection engine lets tools populate compatible optional blobs. The
host remains shareable even when tools use different injection policies or none.
Zed keeps native and packed representations as described above; native eviction
and incremental editing remain application policy.

Zed's
[file loader](</home/mgsloan/proj/zed/crates/worktree/src/worktree.rs:7264>)
decodes text, handles BOMs, and normalizes line endings. Participation in the
existing raw-byte cache still requires byte-for-byte equality with captured disk
input. Absolute coordinate conversion does not make normalized buffers eligible. Unsaved
or transformed buffers need a separately specified snapshot-source cache policy.

Build the injection forest by copying reusable tree segments and packing new
trees, then optionally prepare presence and point caches. Updating a combined injection
blob still rewrites that blob, but leaves the host untouched. Per-injection
persistent segments remain a possible later optimization; they are not required
for this initial host/injection split.

## Implementation and verification

1. Finish unconditional fixed-width support and generic forest construction,
   grammar grouping, tree boundaries, ownership, and checked serialization.
2. Convert native local byte coordinates to absolute document offsets during
   packing. Remove point columns from the slab; add source-aware point access
   and optional separately owned point caches. Verify predicates and bounded
   queries. Add optional per-region presence bitmaps and late attachment.
3. Implement shared full-snapshot injection behavior in `crates/injections`,
   add it as a dependency of persistence, and add a Zed adapter; retain logical
   order independently of storage order.
4. Add independently published host and injection artifacts, shared-source and
   discovery-profile validation, pending-language handling, and stale/cancelled
   publication checks. Then investigate cross-generation reuse.
5. Optimize region scanning after correctness; compare with batched independent
   slabs, including assembly, validation, index construction, and result merging.

Focused cases drawn from
[Zed's syntax-map tests](</home/mgsloan/proj/zed/crates/language/src/syntax_map/syntax_map_tests.rs>):

- same-language injections under different parents and nested through another
  language; no cross-tree parents, siblings, or matches;
- combined injections inside injections and empty combined ranges; preserve
  Zed's newline inclusion and compare full-discovery outputs;
- an outer range starting before content due to a captured language name;
- unknown language resolution and comment-triggered injection toggles;
- nonzero row/column origins, moved first lines, and independently changing
  gaps in a combined parse;
- native versus separate packed versus forest traversal and merged query results,
  including bounded ranges, predicates, ties, and error recovery;
- forest rebuild/serialization round trips, changed grammar bindings, malformed
  descriptors, arithmetic overflow, old-reader ownership, and source bounds;
- incomplete/cancelled packing and stale publication, with downstream snapshots
  refreshed after a successful representation-only replacement;
- host-only and injection-aware tools reading each other's entries, adding
  injections without changing host identity, distinct discovery profiles,
  completed empty versus missing manifests, and independent artifact eviction;
- missing, loaded, built, evicted, malformed, and cancelled presence caches;
  identical forest keys and query results with and without sidecars, including
  mismatched region order and concurrent late attachment;
- identical source-derived and cached start/end points, including zero/EOF,
  final newlines, CRLF, multibyte UTF-8 byte columns, empty/missing nodes, and
  injected ranges with gaps; point-range queries agree before/after attachment;
- point-cache loading, eviction, cancellation, stale source/layout rejection,
  relocation, and unchanged core tree keys regardless of point-cache policy;
- shared-engine versus Zed canonical discovery outputs, including different
  resolver availability and query profiles;
- exact raw-byte eligibility versus BOM/newline/encoding transformations.

## Implementation notes

- `crates/squatter` exposes `Forest`, `ForestInput`, grammar regions,
  `SourceCoordinates`, and separate point/presence caches. Forest construction
  groups exact grammar bindings and shares native columns. Tree-local topology
  remains independent; input-to-tree mapping accounts for grouping.
- `crates/injections` owns registry/profile identity, full-snapshot discovery,
  native included-range parsing, pending languages, canonical manifests, and
  absolute packed output. Persistence depends on this crate. Unsupported query
  predicates are configuration errors. Changing discovery semantics requires
  updating the profile identity inputs when needed; its version remains 0 during
  prototyping, independently of the host runtime fingerprint.
- Persistence schema 0 stores host slabs, injection forest/manifests, symbol
  presence, and points independently. Optional caches can be built, loaded,
  cleared, or republished after a hit. They do not affect authoritative keys.
- Packed persistence/forest nodes contain no point columns or inline presence.
  Symbol presence is always separately allocated, including standalone packing.
  Standalone packing currently also offers inline point columns as a layout option;
  it is not an older-format decoder.
- Forest serialization currently writes grouped compact slabs and reassembles
  shared columns on load. It is an owned archive format, not a directly mapped
  shared-column image. Queries use existing per-tree matchers and region-wide
  presence data. Whole-region candidate scanning and source-order result merging
  remain separate work.
- Native injection trees are retained on discovery and absent on cache hits.
  Their order follows parsed manifest layers, and their coordinates remain local
  parser coordinates. Packed output always uses absolute document coordinates.
- Source-less Rust node point access is fallible through `try_start_position`
  and `try_end_position`; infallible access requires attached coordinates or
  inline points. The shared engine reuses one line index across injection
  parsing and packing.
- Host transfer frames are supported. Injection transfer policy currently uses
  in-process deferred publication; no injection IPC frame is defined.
- No Zed code is changed. Registry construction and full-snapshot comparison
  against Zed are the next integration step; editor-owned incremental machinery
  remains there.

## Follow-up considerations

Proceed with implementation; revisit these after the initial integration:

- Source-coordinate ownership and source-less point APIs: share a line index,
  avoid per-access caller plumbing, and measure whether per-node points should
  be eagerly materialized.
- Distinguish storage grouping by exact grammar from execution grouping by
  application query configuration.
- Qualify the shared discovery profile against Zed's full-snapshot output,
  including resolver behavior and recursion/termination rules.
- Measure cache attachment/eviction synchronization; retain immutable cache
  owners across scans rather than paying synchronization per candidate.
- Compare segmented grammar scanning with optional contiguous reassembly,
  including predicates, viewport selection, and result merging.

- Benchmark the compact-tree archive against direct shared-column persistence;
  consider transaction-backed injection ownership only if it pays for its
  additional validation and lifetime machinery.
- Consolidate standalone point packing with source-derived points; no user or
  persisted-format migration is needed.
- Add injection IPC transfer if consumers need it; keep publication bound to an
  already available host/source generation.
- Extend Zed conformance coverage before claiming interchangeable injection
  profiles, particularly comment toggles, language selectors, and nested combined
  parses. Native parsing cancellation exists; cache construction and query calls
  currently observe cancellation between operations rather than inside each scan.
