# Rust core interfaces

Companion to [rust-core-design.md](rust-core-design.md). This specifies component
boundaries, ownership, and error contracts implemented by `crates/squatter-rust`.
[rust-core-results.md](rust-core-results.md) identifies the immutable baseline
and candidate revisions used by each comparison.

Keep the public Rust API unchanged. The temporary `tree-squatter-rust` package
allows comparison with `tree-squatter`; it does not introduce backend selection
into the production API. A public C facade remains deferred.

Rust signatures below are interface sketches; private helper names can differ.
The native declarations live in [native](crates/squatter-rust/native), with Rust
owners and borrowed views in [native.rs](crates/squatter-rust/src/native.rs).
Existing public signatures remain the contract, including macros and traits.
Packing walks native subtrees or borrowed reductions in Rust and feeds the
encoder directly in reverse preorder.

## Public API carried over

| Surface | Existing contract and candidate implementation |
|---|---|
| [`Grammar`](crates/squatter/src/lib.rs) | `new`, `from_cache`, `language`, `cache`, and shared `Clone`; owns prepared native grammar tables. |
| [`PackContext`, `PackOptions`](crates/squatter/src/lib.rs) | Reusable packing scratch, `pack`, `pack_with_options`, and `trim`; the encoder and slab allocation become Rust. Preserve option fields and defaults. |
| [`Tree`](crates/squatter/src/lib.rs) | Mainline packing/parsing, copied and borrowed loading, safety-checked loading, backed loading, repacking, compact serialization, grammar cache, root/slot access, and storage metadata. |
| [`BorrowedTree`, `BackedTree`, `StableSlab`](crates/squatter/src/lib.rs) | Preserve borrowing, `Deref<Target = Tree>`, stable external storage, descriptor-before-owner destruction, and copying `detach`. |
| [`Parser`, `ParseError`](crates/squatter/src/parser.rs) | `new`, `parse`, `parse_with_options`, `trim`, and `Tree::parse_direct*`; C parses and walks reductions, Rust encodes. Preserve eligibility and syntax-error behavior. |
| [`Node`, `Children`, `Cursor`](crates/squatter/src/lib.rs) | Preserve attributes, names, flags, identity, topology, byte/point seeks, child iteration, typed scans, cursor movement/reset, and lifetime contracts. Implement reads and movement in Rust. |
| [`scan`](crates/squatter/src/scan.rs) | Preserve all public types, aliases, sealed traits, iterator implementations, preorder/postorder and reversal, groups/masks, kind/field/supertype/extra/missing filters, and byte/point range and position selections. Replace the C column-view bridge internally. |
| [`IdSet`, `KindSet`, `KindMatches`](crates/squatter/src/lib.rs) | Preserve construction, membership, reusable `intersection(&self, other: &Self) -> Self`, aliases, and accepted fixed/dynamic scan selections. |
| [`Query`](crates/squatter/src/query.rs) | `new(&Language, &str)`, `pattern_count`, `capture_names`, `general_predicates`, `disable_pattern`, and `disable_capture`. Own native compiled records and Rust preparation. |
| [`QueryCursor`, `QueryExecution`, query results/errors](crates/squatter/src/query.rs) | Preserve optimization, timeout, match-limit, range/depth controls, `execute`, borrowed match/capture streams, removal, cancellation/error reporting, and result fields. Execution becomes Rust. |
| [`traits`](crates/squatter/src/traits.rs) | Preserve `Attributes`, `TreeLike`, `NodeLike`, `CursorLike`, and existing implementations. Candidate definitions are independent of the reference crate. |
| [`Error`, `representation_id`](crates/squatter/src/lib.rs) | Preserve error variants and representation identity semantics. The latter describes slab format/configuration, not implementation or grammar identity. |

Preserve public method signatures, lifetimes, trait bounds, and existing thread
guarantees. Keep `Result` signatures and `Error::Allocation` even though ordinary
Rust allocation failure aborts rather than returning that variant; native paths
may still return it. Removing those signatures or changing `capture_names()` from
`&[String]` to a borrowed-string API would be separate work.

Exact slab bytes and physical slot identities remain comparison requirements.
Preserve existing public representation attributes; no new public C ABI is added.
The candidate and reference have distinct Rust types while both crates exist;
callers can select one crate by dependency alias. Paired benchmarks use local
adapters rather than passing one crate's traits or nodes to the other.

## Shared native conventions

All exported candidate C symbols start with `sq_native_`, including renamed
tree-feller symbols. The private header lives under the candidate's `native/`
directory. It uses Tree-sitter's `TSLanguage` and `TSTree` only as opaque pointers;
Rust never reads their private layouts. There is one Tree-sitter runtime.

Shared records use fixed-width integers and explicit flag words. Rust mirrors
them with `#[repr(C)]`; check sizes, alignments, offsets, and mask values together.
The following Rust notation stands for concrete C pointer/count structs for
each element type, not a generic C ABI:

```rust
#[repr(C)]
struct NativeSlice<T> { data: *const T, length: u32 }
#[repr(C)]
struct NativeMutSlice<T> { data: *mut T, length: u32 }
#[repr(C)]
struct NativeRange { offset: u32, length: u32 }
#[repr(C)]
struct NativePoint { row: u32, column: u32 }

type NativeStatus = i32;
// 0 = success; 1..=7 retain the existing Error discriminants

#[repr(C)]
struct NativeParseError {
    code: NativeStatus,
    byte: u32,
    point: NativePoint,
    message: [u8; 512],
}

#[repr(C)]
struct NativeQueryError { kind: u32, byte: u32 }
```

`NativeQueryError.kind` retains the query compiler's syntax, node-type, field,
capture, structure, and language categories; it is separate from `NativeStatus`.
Parse diagnostics are NUL-terminated within `message` and copied into the
existing owned Rust error type. Query diagnostics retain byte offsets into the
original source.

For every native call:

- Constructors return null on failure and initialize their error output. A
  successful returned handle immediately enters a Rust `Drop` guard.
- Other fallible functions return a status. Their output is readable only on
  success, except for an explicitly documented initialized prefix.
- Pointer/count pairs describe initialized, contiguous elements in one allocation.
  Empty arrays may have null pointers; the Rust adapter returns `&[]` without
  passing null to `slice::from_raw_parts`. Byte lengths must fit Rust slice limits.
- Safe wrappers reject source lengths and coordinates that do not fit native
  fields before conversion; they never silently truncate them.
- Rust owns caller-provided buffers; C only fills the supplied capacity. Native
  allocations remain native allocations and are released by native destructors.
- Retained view descriptors are raw pointers/counts. Safe slices borrow their
  owner when accessed; no fabricated `'static` references or self-borrowing owner.
- No unwind crosses C. This interface needs no Rust callback during query execution.
- Compiler output is trusted in release. Full record validation is debug-only,
  with invariants also checked in boundary tests. This does not weaken validation
  of externally supplied slab or grammar-cache bytes.

## Grammar interface

The native handle owns its language reference, prepared metadata, and lazily
published direct-parser tables. Retain/release operations preserve shared grammar
ownership without duplicating tables. Lazy preparation remains retryable after
failure and safe when separate parsers initialize concurrently.

```c
typedef struct SQNativeGrammar SQNativeGrammar;

SQNativeGrammar *sq_native_grammar_new(
    const TSLanguage *language, int32_t *error);
SQNativeGrammar *sq_native_grammar_new_cached(
    const TSLanguage *language, const uint8_t *bytes, size_t length,
    int32_t *error);
void sq_native_grammar_retain(SQNativeGrammar *grammar);
void sq_native_grammar_delete(SQNativeGrammar *grammar);
void sq_native_grammar_view(
    const SQNativeGrammar *grammar, SQNativeGrammarView *out);
uint32_t sq_native_grammar_cache_size(const SQNativeGrammar *grammar);
int32_t sq_native_grammar_copy_cache(
    const SQNativeGrammar *grammar, uint8_t *destination, size_t length);
```

`SQNativeGrammarView` contains these immutable views and scalars:

| Field | Representation and purpose |
|---|---|
| `language` | Retained `const TSLanguage *`; language identity and public `Language` cloning. |
| `symbol_count`, `field_count` | Grammar-plus-alias count excluding builtin errors, and field count excluding field zero. |
| `public_symbols` | `NativeSlice<u16>` mapping grammar IDs to the prepared display IDs. |
| `symbol_flags` | `NativeSlice<u8>` with named/visible/supertype masks `1/2/4`; no `TSSymbolMetadata` bitfield view. |
| `symbol_names`, `field_names` | Views of pointer/length UTF-8 names retained by the handle/language; field zero denotes no field. |
| `symbol_encoding` | Fixed-width encoding mode, shift, separate-column flag, and views of the existing grammar-ID, default-code, variant-count, default-selector, and grammar-selector tables. |
| `supertypes`, `supertype_indexes` | `NativeSlice<u16>` preserving ordering and the existing zero/one-based index conventions. |
| `supertype_masks` | `NativeSlice<u64>` plus dictionary count and words per mask; native-endian grammar data, not persisted slab bytes. |
| Packing tables | Direct fields by production/structural child, alias sequences and stride, and immutable supertype hash buckets/capacity. |

The symbol encoding table follows [symbols.c](lib/squat/symbols.c): local codes,
global selectors, or literal byte IDs. It must retain the current decoding and
alias behavior; the interface does not require expanding it into a larger lookup
table. Production and alias tables remain owned by the native handle and are
exposed through its immutable view for Rust packing.

Grammar symbol metadata/name/public-map tables include two additional entries:
builtin error at `symbol_count` and error-repeat at `symbol_count + 1`. Rust
accessors translate public builtin IDs to these indexes. Field-name tables
include field zero. Compiled query steps retain their existing symbol and wildcard
conventions. Supertype codes are direct masks for at most eight supertypes and
dictionary indexes otherwise.

The Rust `Grammar` uses these internal operations:

```rust
impl Grammar {
    fn tables(&self) -> GrammarTables<'_>;
    fn language_identity(&self) -> LanguageIdentity;
}

impl<'grammar> GrammarTables<'grammar> {
    fn encode_symbol(&self, display: u16, original: u16) -> u16;
    fn decode_symbol(&self, code: u16, separate_original: Option<u16>) -> (u16, u16);
    fn symbol_name(&self, symbol: u16) -> &'grammar str;
    fn field_name(&self, field: u16) -> Option<&'grammar str>;
    fn symbol_flags(&self, symbol: u16) -> u8;
    fn has_supertype(&self, code: u16, supertype: u16) -> bool;
}
```

`GrammarTables<'grammar>` is a borrowed view of the listed data. The accessors
are Rust reads, not C calls, and returned names may borrow for `'grammar`.
`LanguageIdentity` is an opaque identity token whose use requires an owner that
retains the language; equality does not compare `Grammar` allocation addresses.
Cache loading copies its input; cache writing initializes exactly the requested
size. Dictionary serialization remains native because its representation belongs
to grammar preparation. Slab serialization belongs to Rust.

## Tree input and direct parsing

`PackContext` owns reusable Rust frame, position, mask, and presence-index
scratch. The mainline walker borrows `tree_sitter::Tree`; its root wrapper ties
private subtree pointers to that borrow. The packing traversal module is the
only Rust code that interprets the private subtree representation.

Bindgen generates the subtree layouts, unions, and bitfield accessors from the
resolved dependency's headers for the selected target. This requires libclang;
the development shell supplies it. Cross builds also need matching target system
headers, which bindgen accepts through `BINDGEN_EXTRA_CLANG_ARGS`.

The inline/heap tag must be checked before choosing a union member. Nonterminal
metadata is read only with a nonzero child count. Heap fields use raw accesses:
a shared reference to the entire header would also cover the reference count,
which other tree owners can update concurrently.

The walker visits children last-to-first and emits the parent directly into the
Rust encoder. Frames retain the subtree's physical starting boundary, including
waste introduced by descendants. Hidden nodes propagate fields, aliases, sibling
state, and supertypes without producing output. Disabling points skips position
calculation. `InputNode` is an internal encoder argument, not an FFI record or
retained batch.

A walk guard clears every scratch vector's logical length on success, error, or
unwind. Retained capacity never owns input pointers. A completed packed tree owns
its slab and grammar independently of the packer or input. `trim` releases all
retained traversal and presence scratch.

The direct parser keeps its C-owned reduction arena. Its private bridge exposes
that arena after a successful parse:

```c
const SQReduction *sq_native_parser_reductions(
    const SQParser *parser, uint32_t *count, uint32_t *root);
```

`SQReduction` and the Rust `#[repr(C)] Reduction` share child/sibling indexes,
byte and point bounds, original symbol, alias, field, extra/visible flags, and
visible descendant count. Absent links use `UINT32_MAX`; child links already run
right to left. Hidden reductions have at least one child with visible output.

```rust
impl NativeParser {
    fn new(grammar: &Grammar) -> Result<Self, ParseError>;
    fn parse(&mut self, source: &[u8]) -> Result<Reductions<'_>, ParseError>;
    fn trim(&mut self);
}

impl Reductions<'_> {
    fn grammar(&self) -> &Grammar;
    fn nodes(&self) -> (&[Reduction], u32);
}
```

`Reductions` exclusively borrows the parser and calls `parser_clear` on drop.
The returned slice cannot outlive that guard. Rust walks the reductions and
calls the same encoder without a native traversal session or expanded event
arena. The public `Parser` owns native parser state and a `PackContext`.
Syntax failures clear logical state; `trim` is allowed only while idle.

## Compiled-query interface

C compiles directly into shared fixed-width records. Rust owns the native handle
and reads those records without a copy or per-step C call. Compilation includes
grammar analysis; scan/presence/direct-plan preparation belongs to Rust.

```rust
#[repr(C)]
struct StepRecord {
    symbol: u16,
    supertype_symbol: u16,
    field: u16,
    capture_ids: [u16; 3],
    depth: u16,
    alternative_index: u16,
    negated_field_list_id: u16,
    flags: u16,
}

#[repr(C)]
struct PatternEntry {
    step_index: u16,
    pattern_index: u16,
    presence_requirement: u16,
    flags: u16,
}

#[repr(C)]
struct PatternRecord {
    steps: NativeRange,
    predicate_steps: NativeRange,
    start_byte: u32,
    end_byte: u32,
    flags: u16,
}

#[repr(C)]
struct PredicateStep { kind: u32, value_id: u32 }

#[repr(C)]
struct NativeStringTable {
    bytes: NativeSlice<u8>,
    entries: NativeSlice<NativeRange>,
}
```

The planned step flags are:

| Bit | Meaning | Writer |
|---|---|---|
| 0 | Named | C compiler |
| 1 | Immediate | C compiler |
| 2 | Last child | C compiler |
| 3 | Pass through | C compiler |
| 4 | Dead end | C compiler |
| 5 | Inside alternation | C compiler |
| 6 | Contains captures | C compiler |
| 7 | Root pattern guaranteed | C compiler |
| 8 | Parent pattern guaranteed | C compiler |
| 9 | Missing | C compiler |
| 10 | Alternative is skip | C compiler |
| 11 | Local | Rust preparation |
| 12–15 | Reserved, zero | Initialization |

Use named masks in both languages. This retains the current 20-byte step size
on native x86_64. `PatternEntry.flags` bit zero means rooted; its
`presence_requirement` is zero for none and otherwise one plus a Rust requirement
index. `PatternRecord.flags` bit zero means non-local. Other bits start zero.
Predicate kind values remain done/capture/string (0/1/2); capture quantifiers
remain bytes with the existing `TSQuantifier` values. Preserve the existing
sentinels for missing alternatives/captures, pattern completion, and negated-field
lists rather than redefining their meaning during extraction.

```rust
#[repr(C)]
struct NativeQueryView {
    language: *const TSLanguage,
    symbol_count: u32,
    public_symbols: NativeSlice<u16>,
    steps: NativeSlice<StepRecord>,
    pattern_entries: NativeSlice<PatternEntry>,
    patterns: NativeSlice<PatternRecord>,
    predicate_steps: NativeSlice<PredicateStep>,
    capture_names: NativeStringTable,
    predicate_values: NativeStringTable,
    capture_quantifiers: NativeSlice<NativeSlice<u8>>,
    negated_fields: NativeSlice<u16>,
    rootless_repeat_symbols: NativeSlice<u16>,
    wildcard_root_pattern_count: u32,
}

#[repr(C)]
struct NativeQueryEdit {
    steps: NativeMutSlice<StepRecord>,
    pattern_entries: NativeMutSlice<PatternEntry>,
}
```

The language snapshot supplies the grammar-plus-alias count and public-symbol
map needed for query preparation without constructing a packed `Grammar`. This
map covers the ordinary grammar/alias IDs; query preparation handles builtin
errors explicitly. Nested quantifier views
are descriptors prepared once; the quantifier bytes remain in their native
arrays. C `Array(T)` headers/capacities never become Rust slice layouts. Temporary
analysis graphs, parser strings, and step-offset scratch are released once no
compiler diagnostics require them. Per-pattern source offsets remain in the
final records. Account for retained array capacity and view descriptors.

```c
typedef struct SQNativeQuery SQNativeQuery;

SQNativeQuery *sq_native_query_new(
    const TSLanguage *language, const uint8_t *source, uint32_t length,
    SQNativeQueryError *error);
void sq_native_query_delete(SQNativeQuery *query);
void sq_native_query_view(const SQNativeQuery *query, SQNativeQueryView *out);
void sq_native_query_edit(SQNativeQuery *query, SQNativeQueryEdit *out);
void sq_native_query_disable_pattern(SQNativeQuery *query, uint32_t pattern);
void sq_native_query_disable_capture(
    SQNativeQuery *query, const uint8_t *name, uint32_t length);
```

The disable operations change compiler records only. They do not build C execution
plans. Capture removal edits IDs in place, preserving descriptors and plans.
Pattern removal compacts the entry array; Rust refreshes its view, masks disabled
direct roots, reindexes entries, and updates range eligibility. Existing root scan
filters remain conservative. No slices may remain live across native mutation.

```rust
struct CompiledQuery { /* unique native handle and cached descriptors */ }

impl CompiledQuery {
    fn new(language: &Language, source: &str) -> Result<Self, QueryError>;
    fn view(&self) -> CompiledQueryView<'_>;
    fn edit(&mut self) -> CompiledQueryEdit<'_>;
    fn disable_pattern(&mut self, pattern: u32);
    fn disable_capture(&mut self, name: &str);
    fn language_identity(&self) -> LanguageIdentity;
    // cfg(debug_assertions)
    fn validate(&self);
}
```

`CompiledQueryView` exposes the shared records as borrowed slices, string-table
accessors, and `capture_quantifiers(pattern) -> &[u8]`. `CompiledQueryEdit`
exposes disjoint mutable step/entry slices and read-only remaining metadata.
Only Rust preparation uses mutable views, under exclusive ownership, to fill
its designated fields. C cannot access them during that borrow. Compilation and
native disabling retain responsibility for compiler-owned fields.

`Drop` calls `sq_native_query_delete`, including when later Rust predicate
preparation fails. Do not construct a `Vec` from native allocations. Native
query buffers and the retained language survive all executions borrowing the
query. A finished query does not borrow its source string. Audit `Send`/`Sync`
with these rules; execution introduces no lazy mutation.

## Rust storage and packing

The storage implementation owns layout calculation, aligned allocation, column
encoding/decoding, validation, compaction, and presence indexes. No native
function allocates a candidate packed tree or interprets its slab.

```rust
enum Validation { Full, SafetyOnly }

impl SlabLayout {
    fn calculate(
        grammar: GrammarTables<'_>, capacity: u32, features: SlabFeatures,
    ) -> Result<Self, Error>;
}

fn validate_slab<'bytes>(
    grammar: GrammarTables<'_>, bytes: &'bytes [u8], validation: Validation,
) -> Result<ValidatedSlab<'bytes>, Error>;

fn load_owned(grammar: &Grammar, bytes: &[u8], validation: Validation)
    -> Result<Tree, Error>;
fn load_borrowed<'bytes>(
    grammar: &Grammar, bytes: &'bytes [u8], validation: Validation,
) -> Result<BorrowedTree<'bytes>, Error>;
fn load_backed(grammar: &Grammar, owner: impl StableSlab)
    -> Result<BackedTree, Error>;

impl Tree {
    fn data(&self) -> &TreeData;
    fn columns(&self) -> Columns<'_>;
    fn language_identity(&self) -> LanguageIdentity;
}

impl SlabBuilder {
    fn new(grammar: &Grammar, options: PackOptions) -> Result<Self, Error>;
    fn reserve_groups(&mut self, capacity: u32) -> Result<(), Error>;
    fn finish(self, scratch: &mut PackingScratch) -> Result<Tree, Error>;
}

impl PackingScratch {
    fn clear(&mut self);
    fn trim(&mut self);
}
```

`SlabFeatures` describes points, wide supertypes, and optional stored columns;
the builder tracks which optional columns are actually needed. Group size,
alignment, and version come from the build configuration, not new public options.
`SlabLayout` contains the offsets and decoder constants currently in
[`SQLayout`](lib/squat/internal.h), represented in Rust.

`TreeData` holds the retained grammar, layout, slab address/length, and storage
mode. Owned trees colocate it with aligned payload. Borrowed trees allocate only
the descriptor. `Tree` remains a small owner of a stable descriptor; moving the
Rust wrapper does not relocate the descriptor. Growth may relocate a builder,
so only offsets survive growth. Finalization consumes the builder and publishes
an immutable tree.

`ValidatedSlab` carries a borrowed byte slice and validated layout; it does not
extend storage lifetime or establish alignment by itself. Borrowed/backed loading
checks alignment before exposing direct reads. Owned loading copies into aligned
storage. Both validation policies establish bounds, topology, and coordinate
safety; full validation additionally checks auxiliary membership and canonical
contents. Neither proves agreement with an external source or grammar identity.

`finish` handles optional-column removal, requested repacking, and presence-index
construction using retained scratch. Traversal frames retain physical subtree
boundaries through descendant emission; the encoder recomputes spans when a
group closure adds waste. Walk guards clear transient scratch on every exit.

The existing public `compact_size`, `copy_compact_into`, and `repack` are also the
internal serialization interface; another serializer abstraction is unnecessary.
`copy_compact_into` accepts exactly sized uninitialized, potentially unaligned
destination bytes and initializes all bytes on success without an intermediate
slab. Format/overflow errors return `Error`; aligned allocation failure follows
ordinary Rust allocation behavior.

## Rust traversal and scan kernels

Public `Node`, `Children`, and `Cursor` methods are the traversal interface. Their
implementations operate on `TreeData` and `Columns`, with no native accessor
calls. Cursor ancestry remains cursor-owned; point-free fallback, empty siblings,
inherited fields, and failed-movement behavior retain their existing contracts.

The additional shared kernel interface is:

```rust
struct SlotInterval { start: u32, end: u32 } // half-open physical slots
struct PreorderPosition(u32);              // ascending logical position

impl<'tree> Columns<'tree> {
    fn group(self, index: u32) -> GroupRef<'tree>;
    fn node_at_slot(self, slot: u32) -> Option<Node<'tree>>;
    fn subtree_slots(self, root: Node<'tree>) -> SlotInterval;
    fn to_position(self, slot: u32) -> PreorderPosition;
    fn slot_for_position(self, position: PreorderPosition) -> Option<u32>;
    fn group_has_symbol(self, group: u32, symbol: u16) -> bool;
}

impl<'tree> Preorder<'tree> {
    fn bounded(root: Node<'tree>, interval: SlotInterval) -> Self;
}

impl<'tree, S: GroupScan<'tree>> Scan<'tree, S> {
    fn matching_slots(self) -> MatchingSlots<S>;
}

fn seek_byte(root: Node<'_>, start: u32, end: u32, named: bool) -> Option<Node<'_>>;
fn seek_point(
    root: Node<'_>, start: NativePoint, end: NativePoint, named: bool,
) -> Option<Node<'_>>;
```

`Columns` borrows the slab, layout, grammar metadata, optional points, and
presence index directly. It replaces `sq_tree_scan_columns` and
`sq_tree_scan_point_layout`; the candidate also derives symbol-index offsets
without calling `sq_tree_scan_symbol_index`. Group masks always clear waste and
interval-excluded lanes. A bounded preorder clips to the root's subtree and
retains progress so repeated next-hit searches do not restart scanning. Position
conversion retains the current reverse-slot mapping, including waste; consumers
skip non-node slots.

Retain the existing `GroupScan`, `Predicate::retain_matches`, ordered slot
consumers, fixed/dynamic ID predicates, and coordinate relation kernels from
[`scan.rs`](crates/squatter/src/scan.rs). Expose needed helpers within the crate,
without new public scan methods or a requirement that specialized query loops
construct a public pipeline. Scalar/SIMD dispatch and set-size specialization
remain behind these interfaces.

`SymbolIndex` retains offsets into the existing slab. Its group searches respect
the remaining subtree/range interval in both directions; sparse entries can also
return exact slot masks. Dense selections keep flat traversal. Byte-subtree
rejection runs before bitmap jumps so an index cannot skip a useful ancestor.
Indexed fixed predicates initialize one posting cursor per target; indexed dynamic
predicates initialize one for a singleton or a bounded prefix of sixteen otherwise.
Flat predicates do not initialize cursor payloads. Index jumps and exact masks
share these hints.
Each seek verifies its cursor and bounds local probing before binary search, so
clipping, reversal, and skipped groups need no reset. `next_group` and
`retain_indexed` borrow predicates mutably through composition; column-only
`retain_matches` remains immutable.

The sealed `Predicate` protocol includes `flat(&self) -> impl Predicate` and
`into_flat(self) -> impl Predicate`. They retain comparison state and subtree
bounds without mutable index cursors. `And` converts both children; a shared
predicate reference forwards comparisons and bounds, without enabling index
traversal. `count_flat(self, source: Preorder<'_>) -> usize` selects specialized
singleton, two-ID, and four-ID kernels before the group loop when applicable.
`Filtered::count` forwards its predicate directly, so an unnecessary `Identity`
wrapper cannot hide this dispatch. Fixed-size flat views copy the encoded IDs
to keep their loop state independent of the mutable source.

`Predicate::prepare` prepares encoded IDs and optional index traversal together
for public scans. Query consumers use the narrower internal operation:

```rust
impl<const N: usize> FixedKindIds<N> {
    fn prepare_columns(&mut self, group: &GroupRef<'_>);
}
```

It encodes targets without reading index entries or enabling group jumps.
`retain_matches` then consumes the caller's candidate mask. Query root and
descendant-presence experiments retain their own progress, budgets, and index
policy. Neither preparation path copies columns or changes slab ownership.

`seek_byte` and `seek_point` preserve the indexed singular lookup algorithm and
its structural tie-breaking. Coordinate kernels can accelerate candidate-group
inspection; an all-node containment scan is not a replacement for this API.
Child/sibling searches retain subtree jumps and parent checks. Their mask-based
alternatives, direct-plan fusion, and postorder scratch changes remain independent
experiments under the [design's semantic constraints](rust-core-design.md#semantic-constraints).

## Rust query preparation and execution

The public `Query` owns a `CompiledQuery`, Rust execution plans, and existing
capture-name/text-predicate/general-predicate metadata. Construction completes
all preparation before returning. Native compiled buffers may be edited during
preparation; they become read-only for the duration of shared query access.

```rust
impl Query {
    fn language_identity(&self) -> LanguageIdentity;
}

impl QueryPlans {
    fn prepare(compiled: &mut CompiledQuery) -> Self;
}

struct PresenceRequirement { symbol: u16, field: u16 }
enum PresenceResult { Present, Absent, Unknown }

fn descendant_presence(
    node: Node<'_>, requirement: PresenceRequirement,
    cache: &mut PresenceCache, budget: u32,
) -> PresenceResult;
```

`QueryPlans` owns pattern-map indexes, symbol/root filters, presence requirements,
and eligible direct execution plans. Preparation also sets inline local-step and
presence-index fields. It derives needs-fields, needs-supertypes, and repeated-
capture summaries once. Text-predicate construction remains a separate fallible
part of `Query::new`; failed preparation drops the native owner.

Presence checks exclude the root, require kind and field on the same descendant,
and retain the current 256-position budget, cache, error bypass, and cooldown.
Budget exhaustion produces `Unknown`, which cannot reject a match. Cancellation
is checked by the enclosing executor between bounded checks and during longer
root scans.

The existing public signatures also define the execution boundary:

```rust
impl QueryCursor {
    pub fn execute<'cursor, 'query, 'tree, 'text>(
        &'cursor mut self, query: &'query Query, node: Node<'tree>,
        source: &'text [u8],
    ) -> QueryExecution<'cursor, 'query, 'tree, 'text>;
}

impl<'tree> QueryExecution<'_, '_, 'tree, '_> {
    pub fn next_match(&mut self) -> Option<QueryMatch<'_, 'tree>>;
    pub fn next_capture(&mut self) -> Option<(QueryMatch<'_, 'tree>, usize)>;
    pub fn remove_match(&mut self, id: u32);
    pub fn error(&self) -> Option<QueryExecutionError>;
    pub fn did_cancel(&self) -> bool;
}
```

`QueryCursor` owns reusable state/capture pools, pending and finished queues,
ordering/deduplication scratch, and presence caches. `QueryExecution` owns the
active borrows, traversal/scan position, and timeout progress. Keep those borrows
out of reusable cursor storage, or clear any private raw descriptors before they
can be accessed after execution ends. Capture slices borrow the current execution
advance; nodes inside them borrow the tree. Starting another execution resets
logical state while reusing allocations, including after an error or cancellation.

Use `Query::language_identity()` and the tree's identity before execution.
Preserve existing range eligibility, provisional captures, longest-match rules,
match-limit behavior, and text-predicate filtering. Optimized and general paths
share the result coordinator and capture pool. Root skipping is legal only with
no active NFA states and requires restoring ancestry. Native C is absent from
advancement, timeout checks, text predicates, and result coordination.

Concrete NFA-state, capture-list, and plan structs remain Rust-private. Their
sizes, inline capacities, pooling, and scratch reuse must be compared with the
reference. The interface does not prescribe a heap allocation per state or an
owned `Vec` for each returned match.

## Ownership summary

| Owner or guard | Retains/borrows | Release and concurrency |
|---|---|---|
| `Grammar` | Native handle and language | Shared retain/release; immutable tables, synchronized lazy parser preparation; preserve `Send + Sync`. |
| `CompiledQuery` / `Query` | Native compiled arrays/language and Rust plans | Unique native deletion on drop; mutation requires `&mut`; preserve shared read-only `Send + Sync`. |
| `Tree` | Stable Rust descriptor, owned slab, grammar | Rust deallocation; immutable `Send + Sync`. |
| `BorrowedTree` | Descriptor, grammar, external byte borrow | Frees descriptor, never caller bytes; all nodes bounded by wrapper lifetime. |
| `BackedTree` | Descriptor and `StableSlab` owner | Destroys descriptor before external owner; `detach` copies. |
| `TreeImport` | Exclusive traversal, shared grammar and mainline tree | Drop ends traversal; no retention in output tree. |
| `Reductions` / `ReductionImport` | Exclusive parser session and traversal | End traversal before clearing reductions; parser arena retained for reuse. |
| `PackContext` / `Parser` | Native workers and Rust packing scratch | Preserve movable exclusive-use workers and `trim`; outputs outlive workers. |
| `Node`, scan groups, scans, `Cursor` | Tree borrow and traversal scratch as needed | Preserve current lifetimes and trait behavior; no ownership transfer through node handles. |
| `QueryCursor` | Reusable Rust execution state | Preserve `Send` and exclusive execution access; no implicit `Sync` promise. |
| `QueryExecution` | Cursor, query, tree, and text with separate lifetimes | Results borrow advancement; drop ends access to active inputs; cursor scratch reusable. |

## Comparison, build, and persistence interfaces

The comparison harness is the only component selecting a backend. Keep its
adapters local, statically dispatched, and outside timed inner loops. Cover:

- Grammar preparation/cache loading; mainline conversion and both parser paths.
- Owned/borrowed/backed loading, compact copying, and cross-loading slabs.
- Attributes, traversal, seeks, all typed scan consumers, and query streams.
- Complete query construction, disabling/rebuilding, and destruction separately.
- Reference, Rust original-algorithm control, and one scan experiment at a time.

Preserve all eight benchmark workloads and their timing scope. Build both paired
and separately linked executables. Results record revision/configuration, elapsed
and CPU time, consumed results, allocations, peak scratch, retained capacities,
slab/runtime bytes, and optional counters. Native retained-byte accounting can
use a benchmark-only `sq_native_*` inspection hook or allocator instrumentation;
it is not part of the production Rust API and must not perturb timed runs.

Generate shared build constants for slab group size/alignment and native record
definitions/masks, or mechanically check their mirrors. The native build uses
headers from the exact resolved Tree-sitter dependency. Check one runtime and
disjoint candidate/reference symbols. No candidate C static/shared-library facade
or second Tree-sitter build is needed.

`representation_id()` remains a Rust-computed format/configuration value matching
the reference for matching slabs. Persistence's broader build fingerprint must
include Rust sources, native adapter, tree-feller, configuration, and Tree-sitter
identity. Its consumers keep the public loading/serialization interfaces above.

Before promotion, exercise unchanged public call sites against both crate aliases,
including compile-fail lifetime cases and existing trait guarantees. Check native
layout and flag agreement, debug compiler validation, mutation/view invalidation,
cleanup after preparation failure, and reference/candidate slab cross-loading.
Runtime implementation and performance acceptance follow the main design.
