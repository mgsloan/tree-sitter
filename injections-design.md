# Injections

Step 3 of 3: [side data](side-data.md) → [generic forests](forests-design.md) →
injections. Assume the first two designs are implemented. This step consumes
their APIs without adding injection policy to core trees or forests.

Decision draft, not implemented API. The previous injection prototype was
discarded. Rust signatures omit routine constructors, serialization, and error
conversions. Prototype formats remain at version 0, without migrations.

The [existing persistence API](crates/persistence/README.md) captures whole-file
raw bytes. This proposal extends it; existing entries do not become eligible for
injected or normalized input. The [Zed investigation](#zed-investigation) records
the source of the compatibility requirements below.

## Ownership and crate boundaries

```text
squatter
  Forest / Tree / SourcePoints / PresenceCache / PointData from steps 1 and 2

injections → squatter / tree-sitter
  Engine owns reusable parsing/discovery scratch
  Injections owns Forest, Manifest, any freshly parsed native trees
  Registry supplies immutable language/query/resolver configuration

persistence → injections
  LoadedFile owns captured source + independently usable host
  optional Injections uses that same capture
  storage/publication policy stays here
```

Implement `tree-squatter-injections` at `crates/injections`, depending on native
Tree-sitter and the Rust core, without persistence, LMDB, or Zed dependencies.
Tools can use it without disk caching. Parse-request and manifest types and
encodings belong here. This design does not change grammar construction or
introduce persistence fingerprinting.

Application language configuration remains distinct from grammar: two language
configurations can use one grammar. The host stays separate from the injection
forest. Discovery, nesting, source ranges, and parse requests live in this crate,
not in core forest descriptors.

## Exact parser requests

These types belong to injections. Fields shown are read-only accessors in the
implementation; checked constructors establish bounds and coordinate consistency.

```rust
pub enum IncludedRanges {
    WholeSource,                  // origin must be zero
    Ranges(Vec<tree_sitter::Range>), // nonempty, origin-relative bytes and points
}

pub struct ParseRequest {
    pub language: LanguageKey,    // application language configuration
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
pub struct LanguageKey { /* application language configuration */ }
pub struct Engine { /* reusable parsing and discovery scratch */ }
pub struct InjectionQuery { /* compiled query and checked injection properties */ }

pub enum HostTree<'tree> {
    Native {
        tree: &'tree tree_sitter::Tree,
        language: &'tree LanguageKey,
    },
    Packed {
        root: Node<'tree>,
        language: &'tree LanguageKey,
    },
}

pub trait Registry {
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

pub struct Manifest { /* captured source/host context and ordered layers */ }
pub struct Injections { /* forest, manifest, optional native trees */ }
pub struct DiscoveryLimits { pub max_depth: u32, pub max_layers: usize }

impl Engine {
    pub fn discover(
        &mut self,
        source: &SourcePoints<'_>,
        host: HostTree<'_>,
        registry: &impl Registry,
        limits: DiscoveryLimits,
        pack_options: PackOptions,
        cancel: Option<&AtomicBool>,
    ) -> Result<Injections, InjectionError>;
}

impl Injections {
    pub fn forest(&self) -> &Forest;
    pub fn manifest(&self) -> &Manifest;
    pub fn layers(&self) -> &[Layer];
    pub fn has_unresolved(&self) -> bool;
    pub fn take_native(&mut self, tree: TreeId) -> Option<tree_sitter::Tree>;

    pub fn set_presence_cache(&mut self, cache: PresenceCache) -> Result<(), SideDataError>;
    pub fn set_point_data(&mut self, points: PointData) -> Result<(), SideDataError>;
    pub fn drop_presence_cache(&mut self, region: RegionId);
    pub fn drop_point_data(&mut self);
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
- Registry contents and resolution stay immutable for the operation.
- Success means discovery finished for every available language. Unresolved
  layers remain visible; their descendants are unknown. Changed availability
  requires rediscovery. No resumable partial manifest.
- Limits abort the operation; they do not truncate successful results. Check
  cancellation during discovery, parsing, and packing.
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

Use the common engine to keep behavior consistent across tools. Aliases,
extension rules, queries, predicates, and available grammars must agree before
outputs can be treated as interchangeable. Compare Zed's incremental
requests/manifests with full-snapshot discovery. Persistent compatibility and
cache-key design are separate work.

The packed host must be a whole host-tree root paired with the registry's host
language/grammar and captured source. Validate that pairing before discovery.
After parsing, use `PackContext::pack_forest` and its input-to-tree mapping to
populate parsed layer states; never infer logical order from physical IDs.
Forward `pack_options` to forest packing so callers choose initial presence and
point sidecars. The engine already has the captured source for explicit point
construction and coordinate conversion. Side-data flags do not affect native
parse requests or core contents. Map cancelled packing to
`InjectionError::Cancelled`.

Discovery must not rely on packed point accessors: a host loaded without point
data returns row-zero coordinates. Query captures supply byte ranges; the engine
explicitly derives parser-request points through `SourcePoints::point`. Use
byte-based selection for discovery and test native/packed equivalence with and
without point data. This explicit source work belongs to discovery, not to a
node accessor or an implicit query fallback.

The set/drop methods on `Injections` delegate to its forest. Callers supply point
data built from the matching forest and captured source. Do not expose
`&mut Forest`: replacing it could invalidate every tree ID in the
manifest. Workers can build side data from `forest()` without mutable access;
set/drop requires exclusive access to `Injections` and preserves the mapping.
Presence changes only
performance; point data changes coordinates and point-bounded query behavior.

## Persistence composition

Keep the existing host loader. Add an optional operation on its captured result;
host-only consumers remain independent of injection data.

```text
source generation
  host tree
  injection blob(s), one forest + manifest per discovery profile
  presence cache(host, region)
  presence cache(injections, region)
  point data(host)
  point data(injections)
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
        options: LoadOptions<'_>, // packing flags, cancellation, and write policy
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

`LoadedFile` retains the captured source and host tree. The registry's host
language must agree with the loaded host grammar. A miss discovers from the same
captured bytes; it never rereads the path. Deferred publication owns everything it
needs, following the existing host write API.

| State | Injection-aware load |
| --- | --- |
| Missing/incompatible injection entry | discover and parse |
| Complete empty manifest | return no injections; skip discovery |
| Manifest with unresolved languages, same profile | return it with `has_unresolved() == true` |
| Changed registry or policy | rediscover; host remains reusable |
| Missing/rejected requested sidecar | build before returning success; fail if construction fails |
| Unrequested presence sidecar | leave absent; ordinary symbol scanning |
| Unrequested point sidecar | leave absent; row-zero points |
| Cancelled/limited discovery | return error; publish no injection entry |

Missing injections mean unknown, not a negative discovery result. An empty result
has a valid manifest and no injected-tree payload. The manifest identifies the
host externally, not through a tree/node ID in the injection forest. Injections
are required syntax data for embedded-language consumers. Presence is disposable
without changing query results. Point data is reconstructible from the captured
source and tree, but removing it changes the tree's point APIs to row-zero access.

Publish forest and manifest atomically, separately from the host. Injection
publication neither rewrites nor requires the continued presence of a host slab;
it refers to the captured host/source context. Late writes must
revalidate source-generation references and skip retired generations. Host writes
never delete injection entries. Sidecar writes cannot resurrect source/artifacts.

Sidecar loading, validation, ownership, and publication follow
[step 1](side-data.md#serialization-and-loading) and
[forest serialization in step 2](forests-design.md#representation-and-serialization).
Host and injection owners have independent side data. Sidecar allocations are
separate from their core slabs; loading/setting/replacing them cannot shift core
columns, descriptors, or tree IDs. Sidecars are never coallocated with the core
or each other, so dropping one immediately releases its storage.

Loading honors the requested side-data flags on hits and misses: load matching
sidecars or construct requested ones before returning success. A core hit alone
does not fulfill a request for point data. When points were not requested, point
access uses the row-zero frame until the caller explicitly sets point data.
Host and injection load policies can differ.
Updating an injection blob rewrites that blob alone; per-injection persistent
segments are deferred.

## Query integration

Use existing per-tree query cursors with source bytes. Adapters select layers
and merge results; grammar iteration is not document order. For Zed, preserve
`(start_byte, Reverse(end_byte), depth)` capture ordering and its existing
tie/match rules. One grammar may span the host and injection allocations.

A logical host-plus-injections collection needs only borrowed views over two
owners. Skip layers outside the requested range, retain per-tree fallback
decisions, and group execution only when application query configurations agree.
No combined allocation or public batch query framework is needed. Byte-based
selection and ordering work without point data. Consumers using document point
bounds or displaying row/column locations must arrange point data for every
participating owner. Passing text to a query does not derive those points.

```rust
let loaded = persistence.load_injections(
    &host, &host_language, &registry, &mut engine, limits, options,
)?;
let mut injections = loaded.injections;

// this consumer requires document points; source uses the host's captured bytes
if !injections.forest().has_points() {
    let points = PointData::build_forest(injections.forest(), &source, None)?;
    injections.set_point_data(points)?;
}

let forest = injections.forest();
// layer order is independent of TreeId
for layer in injections.layers() {
    if let LayerState::Parsed { tree, .. } = &layer.state {
        let root = forest.tree(*tree).unwrap().root_node();
        let points = root.point_range();
        // select the application query and merge its results using layer metadata
    }
}
```

Native trees serve editing and may be evicted by the application for buffers not
receiving edits. Fresh discovery can return both representations; a disk hit only
contains packed syntax. Native restoration and incremental edit tracking stay in
the editor.

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
The final parser requests must include these additions; the original content
captures alone do not describe the input.

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

1. Add checked parse requests and per-request parser state reset.
2. Implement full-snapshot discovery, immutable registry configuration, unresolved layers,
   and deterministic logical/physical mappings.
3. Add independently published injection forest/manifest artifacts bound to the
   host's captured source.
4. Integrate the Zed adapter and compare canonical requests/manifests and query
   results before claiming profile compatibility.

Validate four API scenarios first: host-only loading; combined nested injections
with an unresolved language; side-data attachment followed by queries and eviction;
and a disk hit consumed by an editor needing native trees.

Focused cases drawn from
[Zed's syntax-map tests](</home/mgsloan/proj/zed/crates/language/src/syntax_map/syntax_map_tests.rs>):

- same-language injections under different parents and nested through another
  language; no cross-tree parents, siblings, or matches;
- combined injections inside injections and empty combined ranges; preserve
  Zed's newline inclusion and compare full-discovery outputs;
- an outer range starting before content due to a captured language name;
- unknown language resolution, selectors, comment-triggered injection toggles,
  and different application configurations sharing one grammar;
- nonzero origins and fresh parses after moving first lines or independently
  changing gaps in a combined parse;
- native versus separate packed versus forest traversal and merged queries,
  including bounded ranges, predicates, ties, error recovery, and materialized
  versus explicitly source-derived points over included ranges with gaps;
- invalid parse requests followed by valid requests, ensuring no stale included
  ranges or parser state; unsupported predicates fail explicitly;
- unresolved languages remain visible on success; depth/layer limits and
  cancellation fail without publishing an entry;
- changed language availability requires rediscovery without reparsing the host;
- host-only and injection-aware tools reading each other's entries, distinct
  profiles, complete empty versus missing manifests, and independent eviction;
- late injection publication skips retired generations; eviction of only the host
  slab does not invalidate the retained source/host context;
- side-data attachment/removal preserves the manifest-to-forest mapping and core
  contents; presence preserves results, while points switch coordinate frames and
  can change point-bounded results;
- discovery produces identical requests/manifests with and without host point
  data; consumers requiring document points handle host/injection availability
  independently; exercise requested side-data flags on both misses and core hits;
- cancelled discovery/packing and stale publication, with downstream snapshots
  refreshed after successful representation-only replacement and old readers valid;
  preserve any promised point availability when publishing replacement snapshots;
- canonical shared-engine versus Zed discovery output, including resolver behavior
  and recursion/termination rules;
- raw-byte eligibility versus BOM/newline/encoding transformations;
- fresh native tree retention/eviction versus packed-only disk hits.

Core layout, boundary, malformed-cache, and point-coordinate coverage belongs to
[side-data verification](side-data.md#implementation-and-verification) and
[forest verification](forests-design.md#implementation-and-verification).
Check cancellation during discovery, parsing, and forest packing; never publish
partially built artifacts.

## Deferred work

- Keep one forest plus manifest per profile initially. Defer per-injection
  persistence, selective discovery dependencies, and combined host/injection
  allocations until consumers require them.
- Cross-generation parse reuse is deferred. Included ranges, source newline
  additions, local points, and scanner-observable input all affect parsing.
- Packed coordinates stay absolute; moved trees require rebuilding. Consider
  local coordinates with external placement only after measuring reassembly
  against translated access, source slicing, predicates, and query filtering.
- If relocation is added, uniform displacement can adjust tree-local byte bases,
  but independently moving included ranges cannot use one displacement. Establish
  parse equivalence, check overflow, remap indexes, and rebuild/remap caches for
  the new layout/source. Never mutate a live forest.
- Add injection IPC only when consumers need it, bound to an already available
  host/source generation. Background cache publication remains an outer ownership
  concern as specified in step 1.
