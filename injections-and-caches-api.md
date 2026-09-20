# Injections and caches: proposed API

Decision draft, not implemented API. The previous injection prototype was
discarded; this document specifies intended behavior, not existing APIs or disk
formats. Rust signatures omit routine constructors, serialization, and error
conversions.

No data has been persisted for ongoing use. Temporary test databases create no
compatibility obligation. Prototype format, schema, and profile versions remain
at 0; change them directly and regenerate caches, without migrations. Tree-sitter's
upstream ABI versions are independent.

The [existing persistence API](crates/persistence/README.md) covers whole-file
raw-byte parses. This proposal extends it; existing entries do not become eligible
for injected or normalized input. Supporting [Zed investigation](#zed-investigation)
and [conformance cases](#implementation-and-verification) are included below.

## Decisions

- Separate generic immutable forests, injection policy, and optional caches.
  Keep independent host loading and exclusive cache mutation.
- Start with tree iteration; keep batching, result merging, and application query
  configuration outside the forest API.
- Fingerprint the entire immutable registry/profile for discovery invalidation.
  Accept extra misses rather than tracking selective discovery dependencies.
- Expose unresolved layers explicitly. Cancellation and resource exhaustion fail
  the operation rather than publishing a partial manifest.
- Require a source for point access, even when a point cache is attached.
- Separate parse identity, stored representation, and the exact layout addressed
  by a derived cache. Parse identity excludes packing options.
- Defer relocation, per-injection persistence, combined host/injection allocation,
  and background cache publication.

## Ownership and crate boundaries

```text
squatter
  Forest owns packed columns, grammar handles, optional derived caches
  TreeView / Node / Cursor borrow Forest
  SourcePoints borrows immutable bytes; owns one line-start index

injections → squatter / tree-sitter
  Engine owns reusable parsing/discovery scratch
  Injections owns Forest, Manifest, any freshly parsed native trees
  Registry supplies immutable language/query/resolver configuration

persistence → injections
  LoadedFile owns captured source + independently usable host
  optional Injections uses that same capture
  storage/publication policy stays here
```

Move the existing fingerprinted grammar binding below persistence, into squatter;
re-export it from persistence. Application language identity remains distinct
from exact grammar identity: two language configurations can use one grammar.

Implement `tree-squatter-injections` at `crates/injections`, depending on native
Tree-sitter and Squatter, without persistence, LMDB, or Zed dependencies. Tools
can use it without disk caching. Parse-request and manifest types and canonical
encodings belong there; shared identity types must live below persistence.

## Forests

```rust
pub struct Forest { /* slab, retained grammars, optional caches */ }
pub struct TreeId(u32);       // local to one forest
pub struct RegionId(u32);     // local to one forest
pub struct TreeView<'forest> { /* borrowed descriptor */ }
pub struct GrammarRegion<'forest> { /* borrowed descriptor */ }

pub struct PackInput<'tree> {
    pub grammar: &'tree Grammar,
    pub tree: &'tree tree_sitter::Tree,
    pub byte_origin: usize,
}

impl PackContext {
    pub fn pack_forest(
        &mut self,
        inputs: &[PackInput<'_>],
    ) -> Result<(Forest, Vec<TreeId>), Error>;
}

impl Forest {
    pub fn tree(&self, id: TreeId) -> Option<TreeView<'_>>;
    pub fn regions(&self) -> impl Iterator<Item = GrammarRegion<'_>>;
}

impl<'forest> GrammarRegion<'forest> {
    pub fn id(&self) -> RegionId;
    pub fn grammar(&self) -> &'forest Grammar;
    pub fn trees(&self) -> impl Iterator<Item = TreeView<'forest>>;
}

impl<'forest> TreeView<'forest> {
    pub fn id(&self) -> TreeId;
    pub fn root_node(&self) -> Node<'forest>;
}
```

- The returned vector maps input order to physical tree IDs. Empty input is valid.
- One contiguous region per exact grammar; each tree has a group-aligned interval.
  Nodes never navigate or match across tree boundaries.
- Packing adds `byte_origin` with checked arithmetic. Packed nodes expose absolute
  document bytes. Grammar grouping does not combine parser inputs.
- IDs are invalid across rebuilds. An ID alone does not identify its owner.
- A standalone `Tree` uses the same storage/borrow rules as a one-tree forest;
  retain its convenience API. No public slab descriptor tables are needed.

### Internal representation

All grammars use the fixed-width ID layout. Keep compressed absolute byte
coordinates in shared slab columns; points and presence bitmaps are separate.

```text
forest
  grammar A region: [tree 0 groups][tree 1 groups][tree 2 groups]
  grammar B region: [tree 3 groups][tree 4 groups]
```

These intervals are not child relationships. Nested injections of one grammar
can be adjacent even when an intervening layer uses another grammar. The host
stays in its own allocation; the application selects the document root rather
than assuming it is the first tree in grammar order.

Proposed internal descriptor fields, not a public API or frozen ABI:

| Record | Fields | Meaning |
| --- | --- | --- |
| Forest header | version/configuration, table counts/offsets, group count, column/auxiliary locations | Bounds and interpretation of the allocation |
| Grammar region | `grammar_index: u32`, `first_tree: u32`, `tree_count: u32` | Exact grammar binding and consecutive independent trees |
| Tree | `first_group: u32`, `group_count: u32` | Group-aligned storage interval and traversal boundary |

Region group bounds follow from its first and last trees. Tree node bounds follow
from group size; under reverse-preorder encoding the root is the final occupied
slot. Avoid redundant root/boundary tables unless measurements justify them.
Physical node IDs address slots, including group waste; wasted slots are not nodes.

Serialize fields explicitly in little-endian form, with checked count/offset
arithmetic and the format's alignment rules. Grammar indices select exact
bindings retained by the owner; pointers and process-local language IDs are not
persistent identities. Grammar fingerprints belong in the persistence envelope.

Validation establishes that regions partition the tree table, trees partition
used groups, grammar bindings are valid, and topology stays within each tree.
Root parent/sibling navigation terminates at its tree boundary. Column, index,
and coordinate bounds checks still apply. Views cannot free shared storage.

Symbols and fields remain grammar-local despite uniform widths. Prepared grammar
tables and supertype dictionaries may be shared per exact grammar; per-node
supertype encodings require that grammar's interpretation. Grammar-symbol overrides
depend on tree contents and require explicit tree/region scope and index relocation
when copied. They remain authoritative slab data, not optional caches.

## Source and points

```rust
pub struct SourceId { /* byte length + content digest + coordinate profile */ }
pub struct SourcePoints<'source> { /* bytes, SourceId, line starts */ }

impl<'source> SourcePoints<'source> {
    pub fn new(bytes: &'source [u8]) -> Result<Self, Error>;
    pub fn id(&self) -> SourceId;
    pub fn bytes(&self) -> &'source [u8];
    pub fn point(&self, byte: usize) -> Result<Point, Error>;
}

impl Node<'_> {
    pub fn byte_range(self) -> Range<usize>;
    pub fn point_range(self, source: &SourcePoints<'_>)
        -> Result<Range<Point>, Error>;
}
```

One source index serves the host and every injection. Points use zero-based rows
and byte columns, with LF as the line separator; zero and EOF are valid offsets.
No decoding or newline normalization occurs here.

More precisely, row is the number of LF bytes strictly before the offset; column
is the offset minus that row's start. EOF after a final newline starts an empty
row. Other encodings or coordinate units require an explicit source/coordinate
profile. Native parsers can accept custom point coordinates; this profile exposes
source-derived document points instead.

Generic forests do not prove which source produced them. The caller supplies the
matching source; persistence and injection APIs enforce that pairing. Point lookup
checks bounds and uses cached points only when their source identity matches.

Remove `PackOptions::points` and the `(0, byte_offset)` fallback. Point navigation,
point query bounds, and `NodeLike::attributes` must also receive the source context
or split into byte-only and source-dependent operations. Do not leave an implicit
source-less path through an existing trait.

Remove per-node point columns and their per-group bases from the core slab.
Parser-request points still belong to parse identity because they can influence
parsing; cached document points are derived from the resulting node bytes.

## Derived caches

```rust
pub struct PresenceCache { /* one complete grammar region */ }
pub struct PointCache { /* one complete forest + source */ }

impl PresenceCache {
    pub fn build(forest: &Forest, region: RegionId, cancel: Option<&AtomicBool>)
        -> Result<Self, Error>;
}

impl PointCache {
    pub fn build(forest: &Forest, source: &SourcePoints<'_>, cancel: Option<&AtomicBool>)
        -> Result<Self, Error>;
}

impl Forest {
    pub fn attach_presence(&mut self, cache: PresenceCache) -> Result<(), Error>;
    pub fn attach_points(&mut self, cache: PointCache) -> Result<(), Error>;
    pub fn remove_presence(&mut self, region: RegionId) -> Option<PresenceCache>;
    pub fn remove_points(&mut self) -> Option<PointCache>;
}
```

Build/load a complete owned allocation, then attach it. Attachment validates its
target and replaces the previous cache only on success. Dropping a failed or
cancelled build has no effect on the forest. Serialization/load uses these same
owned types; there is no separate live/persisted cache interface.

| Cache | Binding | Missing-cache behavior |
| --- | --- | --- |
| Presence | exact slab layout + grammar bindings + region + cache format | ordinary symbol scanning |
| Points | exact slab layout + source identity + point format | source line-index lookup |

Use a digest of serialized core slab bytes and ordered grammar fingerprints as
the layout identity for persisted sidecars. Compute it once when needed, never
per node. It includes representation identity, excludes derived caches, and avoids
assuming identical parse keys imply identical physical layouts.

Presence must conservatively cover public symbols, aliases, and supertype query
requirements. An absent region cache is distinct from an all-zero bitmap.

Each public-symbol bitmap has one bit per physical group in its grammar region.
A clear bit permits skipping; a set bit only indicates a possible match. Trees
borrow it with a region-relative group offset. Construct caches from symbol
columns using the same public-symbol interpretation as query candidate selection.

Point caches address the forest's physical slots, ignoring waste. Absolute or
compressed point storage is a cache-format choice; it cannot change slab groups
or node IDs. Validate sidecar identity, dimensions, lengths, and access bounds
before attachment. Reordering/rebuilding a forest with a different layout
invalidates caches bound to the old layout; unchanged syntax alone does not make
old point caches reusable.

Cache allocations have one owner; views borrow them. `&mut Forest` excludes active
views during attachment/removal. No lazy writes, locks, or per-view reference
counts. Application snapshot sharing may retain owners externally. The current
`LoadedFile` sharing must not expose cache mutation through a shared `Arc<Tree>`;
attach before sharing or create a new owner for publication.

The C API requires caller-enforced exclusion for attachment/removal. Release
existing views/cursors first, then create new borrowers after mutation. Immutable
readers can run concurrently with ordinary cache reads. Removing a cache detaches
its allocation without changing slab bytes, IDs, or query results. Host and
injection forests own their caches independently; source access must remain valid
for every point lookup. LMDB publication/cleanup does not mutate live owners.

## Exact parser requests

These types belong to injections. Fields shown are read-only accessors in the
implementation; checked constructors establish bounds and coordinate consistency.

```rust
pub enum IncludedRanges {
    WholeSource,                  // origin must be zero
    Ranges(Vec<tree_sitter::Range>), // nonempty, origin-relative bytes and points
}

pub struct ParseRequest {
    pub language: LanguageKey,    // portable application configuration identity
    pub origin: usize,           // absolute byte offset into the captured source
    pub included: IncludedRanges,
}

impl Engine {
    pub fn parse(
        &mut self,
        source: &SourcePoints<'_>,
        grammar: &Grammar,
        request: &ParseRequest,
        cancel: Option<&AtomicBool>,
    ) -> Result<tree_sitter::Tree, InjectionError>;
}
```

The parser reads original source from `origin`, including gaps. Never concatenate
included snippets. Derive local points by subtracting the origin as a text
position, not componentwise. Reject invalid requests before parsing; reset all
per-request parser state so failures cannot reuse previous included ranges.

`Ranges` preserves ordered, nonoverlapping boundaries, even when adjacent. An
empty-content injection uses one explicit zero-length range. An empty vector is
invalid because Tree-sitter interprets it as whole-input parsing. Final requests
include any source newlines added by combined-injection policy.

One ordinary injection can contain several ranges; nodes may span the gaps.
Scanners can observe range boundaries, so canonical encoding must retain the
coordinate frame, origin, and every final byte/point endpoint. Included ranges
belong in the manifest, not generic forest descriptors; Squatter needs no mutable
`set_included_ranges` API. For example, local byte 5 at origin 100 packs as byte
105. Adapters must not apply the origin again during node access or source slicing.

## Discovery

```rust
pub trait Registry {
    fn fingerprint(&self) -> RegistryFingerprint;
    fn language(&self, key: &LanguageKey) -> Option<LanguageConfig<'_>>;
    fn resolve(&self, selector: &str) -> Option<LanguageKey>;
}

pub struct LanguageConfig<'registry> {
    pub grammar: &'registry Grammar,
    pub injections: Option<&'registry InjectionQuery>,
}

pub enum LayerState {
    Parsed { request: ParseRequest, tree: TreeId },
    Unresolved { selector: String },
}

pub struct Layer {
    pub depth: u32,
    pub outer_range: Range<usize>,
    pub state: LayerState,
}

pub struct Manifest { /* source, host identity, profile, ordered layers */ }
pub struct Injections { /* forest, manifest, optional native trees */ }
pub struct DiscoveryLimits { pub max_depth: u32, pub max_layers: usize }

impl Engine {
    pub fn discover(
        &mut self,
        source: &SourcePoints<'_>,
        host: HostTree<'_>,       // native or packed, with its LanguageKey
        registry: &impl Registry,
        limits: DiscoveryLimits,
        cancel: Option<&AtomicBool>,
    ) -> Result<Injections, InjectionError>;
}

impl Injections {
    pub fn forest(&self) -> &Forest;
    pub fn forest_mut(&mut self) -> &mut Forest;
    pub fn manifest(&self) -> &Manifest;
    pub fn layers(&self) -> &[Layer];
    pub fn has_unresolved(&self) -> bool;
    pub fn take_native(&mut self, tree: TreeId) -> Option<tree_sitter::Tree>;
}

pub enum InjectionError {
    Cancelled,
    LimitExceeded,
    InvalidRequest,
    UnsupportedPredicate,
    ParseFailed,
    Pack(Error),
    // ordinary allocation/query/grammar errors omitted
}
```

- Implement one shared full-snapshot policy first, with Zed-compatible capture,
  selector, combined-range, newline, and empty-content behavior. The engine owns
  policy; the registry supplies configuration. Unsupported predicates fail.
- Combine by resolved application language within each parent discovery call.
  Same grammar, different parents/configurations does not imply one parse.
- Registry contents and resolution stay immutable for the operation. Its portable
  fingerprint covers language keys, availability, aliases/extension rules, exact
  grammars, query text, and predicate/resolver behavior. No process-local IDs.
- Success means discovery finished for every available language. Unresolved
  layers remain visible; their descendants are unknown. Changed availability
  changes the fingerprint and forces rediscovery. No resumable partial manifest.
- Limits abort the operation; they do not truncate successful results. Thus they
  are execution controls rather than cache identity. Check cancellation during
  discovery, parsing, and packing.
- Logical layer order and tie order are deterministic, independent of grammar
  packing order. The host is external to the injection forest. Parent tracking
  may remain engine-private; adapters need not adopt a public parent graph.
- Fresh parses retain native trees for the caller to take or drop. Disk hits
  contain packed trees only. Native restoration/editing stays application policy.

The shared engine also owns duplicate handling, outer-origin selection, canonical
request/manifest encoding, and deterministic forest assembly. Native and packed
host inputs must produce identical discovery semantics. Application registries,
loading, anchors, buffer versions, UI configuration, and incremental edit handling
remain in the adapter.

The manifest stays outside the generic slab. It maps logical layers to forest
tree IDs and retains application language, parsed/unresolved state, depth, outer
ranges, origin, and final included ranges as applicable. Several logical layers
may reference one tree when exact parse requests agree. Persist source-relative
ranges bound to the captured generation; reconstruct Zed anchors against the
confirmed buffer snapshot. Parent tracking must not force an adapter to adopt a
public parent graph.

Cross-tool cache hits require shared behavior, not just compatible identities.
Use the common engine and registry/profile contract. An arbitrary resolver does
not establish compatibility; aliases, extension rules, queries, predicates, and
available grammars must agree. Zed's process-local registry counter is not a
portable fingerprint. Its incremental results can use the shared profile only
after canonical requests/manifests agree with full-snapshot discovery.

## Persistence composition

Keep the existing host loader. Add an optional operation on its captured result;
do not add injections or cache flags to the host variant key.

```text
source generation
  host tree
  injection blob(s), one forest + manifest per discovery profile
  presence cache(host, region)
  presence cache(injections, region)
  point cache(host)
  point cache(injections)
```

Host-only consumers need not read injection metadata, load injected grammars,
validate injection slabs, or retain their allocations/LMDB snapshots. Missing,
malformed, or incompatible injection data does not invalidate a host hit.
Different discovery profiles can coexist without duplicating the host record.

```rust
impl Persistence {
    pub fn load_injections(
        &self,
        host: &LoadedFile,
        host_language: &LanguageKey,
        registry: &impl Registry,
        engine: &mut Engine,
        limits: DiscoveryLimits,
        options: LoadOptions<'_>, // existing cancellation and write policy
    ) -> Result<InjectionLoad, InjectionError>;
}

pub struct InjectionLoad {
    pub injections: Injections,
    pub cache_hit: bool,
    pub pending_write: Option<PendingInjectionWrite>,
}

impl PendingInjectionWrite {
    pub fn publish(&self) -> Result<WriteOutcome, CacheError>;
}
```

`LoadedFile` must retain its validated source/host identity. The registry's host
language must agree with the loaded host grammar. A miss discovers from the same
captured bytes; it never rereads the path. Deferred publication owns everything it
needs, following the existing host write API.

```text
parse identity       = source identity + exact grammar/runtime
                       + origin + final ordered ranges + parse options
host entry           = path/source generation + host parse identity
                       + core representation identity
discovery profile    = engine policy identity + registry fingerprint
injection entry      = host entry + host LanguageKey + discovery profile
                       + injection representation identity
presence sidecar      = layout identity + region + presence format
point sidecar         = layout identity + source identity + point format
```

Application language selects discovery configuration; it need not distinguish
individual parse reuse when grammar and actual parser requests agree. Initial
reuse is within the exact captured source. No substring-hash reuse contract.

A discovery-policy/query change may still produce identical parse requests;
individual parses can be reused after rediscovery when their identities agree.
It cannot justify reusing a manifest under a changed discovery profile.

| State | Injection-aware load |
| --- | --- |
| Missing/incompatible injection entry | discover and parse |
| Complete empty manifest | return no injections; skip discovery |
| Manifest with unresolved languages, same profile | return it with `has_unresolved() == true` |
| Changed registry or policy | rediscover; host remains reusable |
| Missing/rejected sidecar | use the normal fallback |
| Cancelled/limited discovery | return error; publish no injection entry |

Missing injections mean unknown, not a negative discovery result. An empty result
has a valid manifest and no injected-tree payload. The manifest identifies the
host externally, not through a tree/node ID in the injection forest. Injections
are required syntax data for embedded-language consumers; only presence and point
caches can be discarded without changing query semantics.

Publish forest and manifest atomically, separately from the host. Injection
publication neither rewrites nor requires the continued presence of a host slab;
it binds the host parse identity and retained source record. Late writes must
revalidate source-generation references and skip retired generations. Host writes
never delete injection entries. Sidecar writes cannot resurrect source/artifacts.

Sidecar persistence is explicit read/build/write work using the cache types above;
loading a host or injections does not implicitly materialize every cache. Loaded
sidecars own their bytes and do not retain an LMDB read transaction. Preserve the
existing safety-only validation policy; identity/bounds checks do not prove cache
contents semantically correct.

Store presence and points in separate LMDB databases. Their formats and build/load
policies do not affect host or injection keys. This removes `symbol_presence` and
`points` from persisted tree-variant options for this format.
Sidecars can be published after their forest and deleted independently; cleanup
must tolerate late writers while preserving source-generation references for
authoritative artifacts. Updating an injection blob rewrites that blob alone.

## Query boundary and decisions to settle

Use existing per-tree query cursors, extended with `SourcePoints` for point access.
Adapters select layers and merge results; grammar iteration is not document order.
For Zed, preserve `(start_byte, Reverse(end_byte), depth)` capture ordering and its
existing tie/match rules. Keep fallback decisions per tree. A logical host-plus-
injections collection needs only borrowed views over two owners initially.

One grammar may span the host and injection allocations. Skip unqueried grammars
and layers outside the requested range. Structural matching, traversal depth,
anchors, and pending-match state stay within each tree; an error-containing tree
or unsupported query option must not disable fast execution for unrelated trees.
Equal grammar bindings do not imply equal application query configurations.

| Decision | Recommendation | Cost |
| --- | --- | --- |
| Discovery invalidation | whole registry/profile fingerprint | unrelated registry changes miss |
| Point API | explicit shared source context | changes node/query/trait signatures |
| Cache mutation | exclusive owner access | background publication needs a new owner |
| Persisted cache binding | digest of exact slab layout | one hash when storing/using sidecars |
| Incomplete discovery | unresolved layers on success; limits/cancellation error | no partial progress reuse |
| Initial injection storage | one forest + manifest per profile | any injection change rewrites the blob |
| Coordinates | absolute packed bytes, source-derived points | moved trees require rebuilding |

Before implementation, validate the API against four cases: host-only loading;
combined nested injections with an unresolved language; cache attachment followed
by queries and eviction; a disk hit consumed by an editor needing native trees.
Use the [conformance cases below](#implementation-and-verification) during
implementation. Do not expand this interface for relocation, selective dependency
validation, or batch scheduling until those cases require it.

## Zed investigation

Investigation recorded 2026-09-16: `/home/mgsloan/proj/zed`, HEAD
`ce48461eaadd16c65c31f835511ab96bd3b6e746`, including its uncommitted changes.
In particular, packed-tree publication and related tests are local additions.
References below identify the inspected working-tree code, not an upstream
release. No Zed files were changed or Zed tests run for this investigation.

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

### Raw-byte cache eligibility

Zed's
[file loader](</home/mgsloan/proj/zed/crates/worktree/src/worktree.rs:7264>)
decodes text, handles BOMs, and normalizes line endings. Participation in the
existing raw-byte cache still requires byte-for-byte equality with captured disk
input. Absolute coordinate conversion does not make normalized buffers eligible.
Unsaved or transformed buffers need a separately specified snapshot-source cache
policy.

## Implementation and verification

1. Implement fixed-width forests, grammar grouping, tree boundaries, ownership,
   checked serialization, and input-to-tree mapping.
2. Pack absolute bytes, remove slab point columns, and add explicit source-aware
   point access throughout node/query/trait APIs. Add separately owned presence
   and point caches with exclusive attachment.
3. Implement full-snapshot discovery in `crates/injections`, preserving logical
   order separately from grammar order, and add the persistence dependency.
4. Add independent host/injection publication, registry/profile validation,
   unresolved-language handling, and stale/cancelled publication checks.
5. Integrate the Zed adapter and compare canonical requests/manifests and query
   results before claiming profile compatibility.

Choose cancellation checkpoints within parsing, packing, cache construction, and
query scans; no partially built result may become visible. Validate the four API
scenarios above before implementation, then cover these cases as features land.

Focused cases drawn from
[Zed's syntax-map tests](</home/mgsloan/proj/zed/crates/language/src/syntax_map/syntax_map_tests.rs>):

- same-language injections under different parents and nested through another
  language; no cross-tree parents, siblings, or matches;
- combined injections inside injections and empty combined ranges; preserve
  Zed's newline inclusion and compare full-discovery outputs;
- an outer range starting before content due to a captured language name;
- unknown language resolution and comment-triggered injection toggles;
- nonzero row/column origins and fresh parses after moving first lines or
  independently changing gaps in a combined parse;
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
  mismatched region order, late attachment after releasing views, and concurrent
  immutable readers;
- identical source-derived and cached start/end points, including zero/EOF,
  final newlines, CRLF, multibyte UTF-8 byte columns, empty/missing nodes, and
  injected ranges with gaps; point-range queries agree before/after attachment;
- point-cache loading, eviction, cancellation, stale source/layout rejection,
  rebuilt placements, and unchanged core tree keys regardless of point-cache policy;
- shared-engine versus Zed canonical discovery outputs, including different
  resolver availability and query profiles;
- exact raw-byte eligibility versus BOM/newline/encoding transformations.

Also verify the API contract:

- unrelated registry changes invalidate injection entries but preserve host hits;
- unresolved languages are successful visible outcomes; depth/layer limits and
  cancellation fail without publishing an entry;
- identical parse identities with different physical layouts cannot share
  sidecars; failed attachment leaves an existing cache intact;
- point access always requires source context, including when cached; source
  mismatch cannot use cached points, and invalid byte offsets fail;
- rejected parser requests cannot leave included ranges or other parser state
  from the previous request active;
- late injection publication skips retired generations, while eviction of only
  the host slab does not invalidate a retained source/host parse identity;
- fresh discovery retains native trees for callers; disk hits contain packed
  trees only and leave native restoration to the application.

## Deferred work

These are measurement or integration questions, not initial API requirements:

- Measure eager per-node point materialization against line-index lookup, keeping
  explicit source access and exclusive cache ownership.
- Compare per-tree/segmented scans with shared candidate scanning and contiguous
  reassembly. Batch only matching query configurations; include viewport selection,
  predicates, result merging, assembly, validation, and index construction costs.
  Candidate scans may cross tree boundaries; structural matches may not.
- Compare a compact-tree archive with direct shared-column persistence. Consider
  transaction-backed injection ownership only if it justifies the validation and
  lifetime machinery.
- Revisit local coordinates with external placement only if moving/reassembling
  trees costs more than translated node access and query filtering. Initial
  packed coordinates remain absolute, and moved trees require rebuilding.
- If relocation is added, a uniform byte displacement can adjust tree-local group
  bases, but independently moving included ranges cannot use one displacement.
  Never mutate a live forest; check overflow, establish parse equivalence, remap
  indexes, and rebuild or correctly remap caches for the new layout/source.
- Cross-generation reuse must preserve range boundaries, source newline additions,
  local points, and scanner-observable input. Concatenated included bytes are
  insufficient. Start with direct final-content hashing before adding range-hash
  structures; a whole-file digest does not provide substring digests.
- Add per-injection persistence, selective discovery dependencies, background
  cache publication, or injection IPC only when consumers need them. IPC
  publication must bind an already available host/source generation.
