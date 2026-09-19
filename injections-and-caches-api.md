# Injections and caches: proposed API

Decision draft, not implemented API. Rust signatures omit routine constructors,
serialization, and error conversions. Prototype formats remain at version 0;
change them directly, without migrations.

## Critique of `injections-design.md`

- Keep the separation of packed syntax, injection policy, and optional caches.
  Keep independent host loading and exclusive cache mutation.
- The Zed investigation is useful evidence, but obscures the public contract.
  Keep it in that document rather than repeating it in API documentation.
- Grammar regions need not become a query framework. Start with tree iteration;
  keep batching, result merging, and application query configuration separate.
- Dynamic discovery dependencies make lookup unnecessarily difficult initially.
  Fingerprint the entire immutable registry/profile. Accept extra misses.
- A successful pass with unresolved languages is not complete syntax coverage.
  Expose unresolved layers explicitly; cancellation and resource exhaustion fail
  the operation rather than publishing a partial manifest.
- Optional points are an API change, not just a storage change. Require a source
  for point access, even when a point cache happens to be attached.
- Parse identity should exclude packing options. Distinguish a parse request,
  a stored representation, and the exact layout addressed by a derived cache.
- Defer relocation, per-injection persistence, combined host/injection allocation,
  and background cache publication. None is needed for the initial interface.

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

Generic forests do not prove which source produced them. The caller supplies the
matching source; persistence and injection APIs enforce that pairing. Point lookup
checks bounds and uses cached points only when their source identity matches.

Remove `PackOptions::points` and the `(0, byte_offset)` fallback. Point navigation,
point query bounds, and `NodeLike::attributes` must also receive the source context
or split into byte-only and source-dependent operations. Do not leave an implicit
source-less path through an existing trait.

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

Cache allocations have one owner; views borrow them. `&mut Forest` excludes active
views during attachment/removal. No lazy writes, locks, or per-view reference
counts. Application snapshot sharing may retain owners externally. The current
`LoadedFile` sharing must not expose cache mutation through a shared `Arc<Tree>`;
attach before sharing or create a new owner for publication.

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

## Persistence composition

Keep the existing host loader. Add an optional operation on its captured result;
do not add injections or cache flags to the host variant key.

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

| State | Injection-aware load |
| --- | --- |
| Missing/incompatible injection entry | discover and parse |
| Complete empty manifest | return no injections; skip discovery |
| Manifest with unresolved languages, same profile | return it with `has_unresolved() == true` |
| Changed registry or policy | rediscover; host remains reusable |
| Missing/rejected sidecar | use the normal fallback |
| Cancelled/limited discovery | return error; publish no injection entry |

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

## Query boundary and decisions to settle

Use existing per-tree query cursors, extended with `SourcePoints` for point access.
Adapters select layers and merge results; grammar iteration is not document order.
For Zed, preserve `(start_byte, Reverse(end_byte), depth)` capture ordering and its
existing tie/match rules. Keep fallback decisions per tree. A logical host-plus-
injections collection needs only borrowed views over two owners initially.

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
Use the larger design's conformance cases during implementation. Do not expand
this interface for relocation, selective dependency validation, or batch scheduling
until those cases require it.
