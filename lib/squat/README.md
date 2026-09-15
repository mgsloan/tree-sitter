# Squat C runtime

An immutable packed-tree API alongside this checkout's unchanged Tree-sitter
runtime. The implementation follows [design.md](../../design.md): 16-slot
reverse-filled groups, aligned columns, non-straddling packed IDs, physical
subtree spans, supertype masks/dictionary, and optional symbol presence entries.
Streaming queries use an adapted compiler and optimized executor from `../main`;
see [query provenance and scope](QUERY_PROVENANCE.md).

```c
#include <tree_sitter/squat.h>

SQError error;
SQPackOptions options = sq_pack_options_default();
options.repack = true;
SQTree *packed = sq_tree_pack(parsed_tree, options, &error);
if (packed) {
  SQNode root = sq_tree_root_node(packed);
  // packed retains its language and does not retain parsed_tree
  uint32_t node_count = sq_node_descendant_count(root);
  (void)node_count;
  sq_tree_delete(packed);
}
```

`make -C lib/squat` builds the static library and C comparison executable.
Link the library before mainline Tree-sitter. Public declarations are in
[`include/tree_sitter/squat.h`](include/tree_sitter/squat.h).

For batches using one grammar, create an `SQPackContext` with
`sq_pack_context_new(language, &error)` and call
`sq_pack_context_pack(context, parsed_tree, options, &error)`. It retains the
language, grammar lookup tables, and scratch allocations across files. Direct
fields are cached per production, avoiding repeated field-map scans and scratch
clearing in frames and hidden-wrapper descent. One-shot packing keeps its local
field scratch to avoid preparing a whole grammar for a tiny tree. Each call
resets traversal state, including after an error; a tree from another language
is rejected with `SQ_ERROR_LANGUAGE`. Output trees own their storage and remain
valid after context reuse, trimming, or deletion. `sq_pack_context_trim(context)`
releases high-water scratch while keeping grammar tables; finish with
`sq_pack_context_delete(context)`. Use separate contexts for concurrent calls,
and keep native grammar libraries loaded while any context or tree uses them.
The original `sq_tree_pack` remains available for independent conversions.
Historical measurements record the retained cache and the rejected
position-calculation alternatives.

`context-check` checks output equality, reuse, trimming, and ownership; setting
`CONTEXT_FAILURES=1` also injects failure at every pack allocation and checks
recovery. `setup-bench` accepts `SQ_REUSE_CONTEXT=1` and `SQ_BATCH_LOOPS=16` for
paired small-file measurements with sufficient work per timed sample.

`make -C lib/squat check` checks column packing and value-preserving growth and
compaction, including nine-bit lane realignment. For grammar comparisons:

```sh
python3 lib/squat/tests/container.py --output build/squat-check --queries
python3 lib/squat/tests/container.py --output build/squat-sanitize --sanitize --queries
```

The runner defaults to `../../code-corpora`, reads its pinned build image ID,
compiles selected local grammar sources, and executes them in offline Podman
containers. `--grammar NAME` is repeatable. Each fresh output directory contains
logs and grammar/tool/image provenance. No source checkout is modified. Missing
grammars and failed comparisons cause a nonzero exit.

Conversion walks raw subtrees iteratively in reverse preorder. Each frame stages
child positions because multiline point offsets cannot be subtracted. Only the
current group's absolute node attributes are buffered; no full-tree node array
is needed. Parent navigation scans backward, using group span bounds to skip groups; cursors retain an ancestor stack.
The cursor supports first/last child, next sibling, and parent movement. It
retains only an ancestor stack, with no sibling history or decoded-column cache.

Byte-range descendant lookup binary-searches the group start-byte minima, finds
the selected group's qualifying start, then scans end coordinates to find the
deepest enclosing node. Named lookups continue to the nearest named ancestor.
Start-byte bases must retain actual minima for this search; zero-base selection applies only to other columns.
Equal-start empty nodes use the original sibling descent to preserve boundary
behavior. The search allocates nothing and adds no serialized index. Finding the
enclosing end can still scan groups, so the complete lookup is not always logarithmic.

Point-range lookup searches group start-row minima. When the row matches, it
compares the earliest preorder node's start column, since the column base may be
zero or describe a different row. After selecting a candidate, it scans end
coordinates when the subtree root is within 512 groups. Longer distances use
span-based parent traversal, avoiding the large-file regressions of an
unrestricted end scan. Point search and its sibling descent fallback compile
only with `SQ_INCLUDE_POINTS`; byte descent uses integer offsets directly.

Byte search compares the selected group's start deltas with SSE2 when available,
masking out unused lanes and slots outside the subtree. Point search compares its
u16 keys scalarly so it can stop at the first qualifying lane; constructing a full
SIMD mask was slower for the short default groups. Byte end scans reuse group bases
and skip groups whose maximum end is too small. The equal-start boundary walk uses
its subtree root as the bound, avoiding repeated whole-tree checks through the
public preorder API.

`sq_node_attributes`, `sq_cursor_attributes`, and `sq_node_iterator_attributes`
read constant-time bulk snapshots, sharing symbol decoding and metadata reads.
Child, named-child, and descendant counts are separate node APIs. This removes
those members from the C snapshot and Rust `Attributes`; callers must rebuild
and request counts explicitly when needed. Rust nodes, cursors, and iterators
expose `attributes()`:

```rust,ignore
let mut cursor = packed.root_node().walk()?;
let attributes = cursor.attributes();
println!("{}: {}..{}", attributes.kind, attributes.start_byte, attributes.end_byte);
cursor.goto_first_child();
```

The runtime layout has named slab offsets, with no column enum or offset table.
Flags, u8/u16 deltas, and u32/u64 bases have explicit typed reads and writes;
byte positions within native packed words are adjusted on big-endian hosts.
Only variable-width IDs and group waste use the non-straddling bit decoder.
Symbol and field decoders cache their lanes-per-word and masks in the runtime
layout, avoiding repeated grammar-wide arithmetic. These constants add eight
bytes to the runtime tree and do not change serialized slabs.

After the header and per-group waste column, columns are ordered: start byte,
end byte, span, symbol, field, supertype, flag bitmaps (`last`,
`extra`, `error`, `missing`), start point, end point. Each group-base column
immediately precedes its corresponding node-value column, with alignment padding
where needed. The optional symbol-presence index and sparse grammar-symbol
overrides follow the columns.

Subtree-span and start-column bases are zero when every live value in the group
fits in u8; otherwise they use the actual minimum. The packer chooses these bases
after closing the group, preserving group boundaries. End columns keep their
actual maxima and the existing base-minus-delta encoding. Bases are encoding
parameters rather than a general minimum/maximum index: revisit these choices
if actual minimum or maximum column positions, or minimum subtree sizes, become
useful for future operations.

Point rows and columns share one u16 key per node, with the row delta in the
high byte and column delta in the low byte. Their componentwise group bases are
stored symmetrically as one u64 key, with the row in the high word. The payload
still uses four bytes per node and 16 bytes per group before column padding,
while lexicographic point comparisons now use one integer key.

The version-9 serialized header is 16 bytes: a format/flags word, live group
count, allocated group capacity, and supertype-dictionary count. Column and
auxiliary-section offsets are derived from the exact grammar, capacity, and
feature flags. The symbol-presence index has an explicit presence flag. All
previous versions are rejected. Columns start on eight-byte boundaries (64 in
the experimental alignment build); auxiliary sections remain eight-byte aligned.
Slabs are native-endian.

Each newly packed tree has one private allocation: runtime descriptor, supertype metadata,
alignment padding, then the persisted slab. `sq_tree_data` / Rust `as_bytes`
returns only the persisted suffix. Builder growth can relocate this allocation;
public trees and handles are immutable. `sq_tree_repack` returns an independent
colocated compact copy with the same physical slot IDs.

Up to eight supertypes use direct byte masks without a dictionary lookup. Larger
supertype sets use a deterministic dictionary derived from the compiled grammar,
shared by trees and contexts for that language. IDs are sorted by mask, independent
of conversion order. The dictionary selects 8- or 16-bit indexes upfront and is
not stored in each slab. Loading derives it from the matching grammar and checks
the header count/width. Grammar analysis returns `SQ_ERROR_DICTIONARY_FULL` if its
conservative mask set exceeds 65,536 entries. Retaining a packing context keeps
the cache warm even when no trees remain; trimming keeps this immutable metadata.
Analysis distinguishes nonterminal extras from ordinary recursive gotos and only
explores hidden definitions reachable from supertypes or hidden extras. Unary
productions avoid building the full predecessor graph. Version 9 rejects older
slabs because the tighter analysis can change dictionary IDs.
Historical mask-analysis benchmarks record the dictionary-size, initialization,
conversion, and memory effects.

Physical columns are filled from the beginning in reverse preorder. Nodes use
direct physical slot indexes, and preorder traversal walks toward lower slots.
There is no capacity-minus-count lookup or cache. Growth and compaction copy
used packed words without changing lane phase. Iterator caches decode physical
windows normally and consume them in descending order. Query plans translate
slots to ascending preorder positions only where ordered scan intervals need it.

`sq_tree_from_bytes` copies arbitrary-alignment input into a separate owned
payload and validates topology, coordinates, symbols, fields, dictionaries, and
presence entries. `sq_tree_from_bytes_borrowed` validates an externally owned,
immutable, aligned buffer without copying it; deletion frees only the runtime
prefix. The caller keeps that buffer alive until all uses of the borrowed tree
finish. Rust's `Tree::from_bytes_borrowed` returns `BorrowedTree<'a>`, tying that
lifetime to the input slice and exposing read-only tree APIs through `Deref`.
There is no grammar fingerprint; callers must supply the exact matching grammar.
Index validation reads the external columns in place, using per-symbol counters
and bitmap popcounts instead of constructing a temporary copy of the slab.

Empty nodes need care: mainline's `next_sibling` skips siblings ending at the
current node's end byte. Child enumeration and cursors include them. Squat's
public sibling accessor reproduces that behavior, while
`sq_node_next_sibling_including_empty` supports structural iteration.

Excluded APIs: incremental editing/reparsing, parse states, exact
unexpected-character S-expressions, and preservation of included-range metadata.
Included ranges still affect the packed node coordinates. Allocation, layout
overflow, and more than 256 distinct supertype masks report errors.

Field lookup returns the first visible child carrying the requested field;
ERROR parents have no lookup fields. Mainline's API can disagree with its own
visible-child cursor, notably when inheritance crosses an alias-visible wrapper.
Tests count that as an expected field mismatch only if squat agrees with the
mainline cursor's direct-child result. Other field mismatches still fail. No
exception table or conversion bookkeeping is retained for these cases.
Negated-field queries consequently follow visible-child fields too. The C query
test counts attributable differences for its simple `(_ !field) @parent` probes;
other query comparisons remain strict.
Historical comparisons document the upstream inconsistency and the removed
version-2 compatibility mechanism.

Known mainline seek differences are counted but ignored by default, as requested
by the human. Use `--strict-seeks` for the container runner or `SQ_STRICT_SEEKS=1`
for the C executable to investigate them. The fixture `tests/fixtures/hidden-seek.css`
is a minimal valid-input repro. No hidden-node or seek-barrier index is stored.

`tests/seek.c` compares byte seeks and, when enabled, point seeks exactly with the
previous sibling-descent algorithms, independently of those mainline differences. Build it with
`make -C lib/squat ../../build/squat/seek-check`, then run
`build/squat/seek-check GRAMMAR_LIBRARY GRAMMAR_SYMBOL SOURCE_LIST`, where
`SOURCE_LIST` contains one source path per line. It checks named/unnamed ranges,
subtree roots, boundaries, and malformed variants of every input.

Query declarations are in [`squat_query.h`](include/tree_sitter/squat_query.h).
Compile once with `sq_query_new`, execute with `sq_query_cursor_exec`, and advance
with `sq_query_cursor_next_match` or `sq_query_cursor_next_capture`. Capture arrays
are borrowed until the next cursor mutation. The query, tree, and callback payload
must outlive execution. C exposes text predicates as metadata; the Rust wrapper
evaluates equality, regex, and membership predicates against supplied source bytes.

Root filtering combines exact masked SWAR comparisons with the optional symbol
presence index. Mandatory symbol/field requirements use intersected group masks.
Local and anchored-child plans share the NFA's ordered capture coordinator; other
patterns use the NFA. `sq_query_cursor_set_optimized(false)` disables scan/plan
shortcuts for differential checks.

Bounded byte/point ranges with branching or rootless patterns report
`SQ_QUERY_UNSUPPORTED_RANGE` on advancement. Always inspect
`sq_query_cursor_error` after iteration. Ordinary unrestricted queries and simple
rooted ranges are supported. This limitation is independent of ignored seek
comparisons. Cancellation callbacks terminate execution, but their exact cadence
depends on the representation.

## Preorder node iterator

`SQNodeIterator` walks a root and its descendants in preorder, including empty
nodes. Construct it with `sq_node_iterator_new(root, unpack_cache)`, consume nodes
with `sq_node_iterator_next`, and release it with `sq_node_iterator_delete`.
The iterator owns no tree and keeps no ancestor stack. It advances consecutive
physical slots in descending order and reads trailing waste only at group boundaries. Exhaustion is
permanent. The tree must outlive both the iterator and returned ordinary nodes.

`sq_node_iterator_attributes` and `sq_node_iterator_field_id` read the last yielded
node; they return zeroed attributes / field zero before the first yield and after
exhaustion. Rust exposes `Node::node_iterator(bool)` and a fused `NodeIterator`;
its corresponding accessors return `None` outside a yielded position. The older
allocation-free `Node::preorder()` remains available.

The optional lazy cache stores display symbols, grammar symbols, and fields as
u16 lanes, plus six absolute coordinate columns as u32 lanes. Coordinate decoding
widens unsigned byte/u16 deltas directly from the slab and adds or subtracts a
broadcast group base with AVX2 (eight lanes) or SSE2 (four lanes) on x86-64.
Other platforms use a portable scalar implementation. Single-bit flags remain
packed. Bulk snapshots exclude child and descendant counts; their explicit node
APIs still use ordinary tree scans.

AVX2 and scalar group decoding skip addition for zero bases. SSE2 and per-node
scalar reads retain their arithmetic paths. Subtraction always retains its
base-minus-delta semantics, including when the base is zero.

A field-only consumer unpacks only fields; a navigation-only consumer never
unpacks anything. Repeated attribute reads reuse the same window. Returned
ordinary node handles do not use the iterator's cache.

Portable ID unpacking expands four packed fields into u16 lanes with masks and
shifts. Automatic variable-width decoding uses BMI2 PDEP on supported Intel
CPUs, the vendor measured here, and portable SWAR elsewhere. AVX2 variable shifts
and byte shuffles remain available for experiments. `SQ_UNPACK_KERNEL=1/2/3/4`
selects scalar/SWAR/BMI2/AVX2 at build time; unavailable hardware selections fall
back to SWAR. `SQ_COORDINATE_KERNEL=0/1/2/4` independently selects automatic,
scalar, SSE2, or AVX2 coordinate reconstruction, with a supported fallback.

`SQ_ITERATOR_CACHE_ALL=2` is the default absolute-coordinate cache. Build mode `0`
(IDs only) remains available for reproducing earlier experiments. The old
delta-cache mode `1` has been removed. The public boolean constructor still selects
cached or uncached operation. No slab format or cursor API changes.

`SQ_ITERATOR_UNPACK_SLOTS=32/64/128` widens the iterator cache independently of
`SQ_GROUP_SIZE`. With the default 16-slot slab groups, these windows decode ahead
across 2/4/8 groups without changing serialization or node addresses. Every group
uses its own bases during reconstruction, and the final window stops at the last
live group. The default unpack window remains one group.

Historical absolute-coordinate cache benchmarks compare cached and uncached
operation at all four window sizes.

## Memory benchmark

The historical measured memory comparison covers mainline and Squatter,
default/compact packing, and point-enabled/byte-only builds.
It measures live allocations from the implementation, rather than estimating
storage from public nodes. Retained sizes include the tree object and auxiliary
allocations; the parser is released first. Construction peaks are separate.

On Linux/glibc with GNU-compatible linker wrapping, build and run:

```sh
make -C lib/squat BUILD=../../build/squat-memory/points \
  CFLAGS="-O3 -g -DSQ_INCLUDE_POINTS=1" \
  ../../build/squat-memory/points/memory-bench
make -C lib/squat BUILD=../../build/squat-memory/bytes \
  CFLAGS="-O3 -g -DSQ_INCLUDE_POINTS=0" \
  ../../build/squat-memory/bytes/memory-bench
python3 lib/squat/experiments/memory.py --output build/squat-memory/results.json
```

The runner uses the saved iterator corpus manifest and grammar bundles. It checks
their hashes, prepares the same seed-42 mutations, compares both point modes, and
requires two identical measurements per input. For a single file, invoke either
`memory-bench GRAMMAR_LIBRARY GRAMMAR_SYMBOL SOURCE` directly.

Requested bytes and glibc usable bytes are both recorded. Neither is process RSS:
allocator metadata, free arenas, source text, shared grammar mappings, and the
out-of-band allocation tracker are excluded. Link wrapping covers runtime and
Squatter objects, including serialized scanner states owned by trees. Direct libc
allocations inside prebuilt grammar scanners are not intercepted; these scanners
are destroyed with the parser before retained tree measurements. Reported peaks
therefore cover runtime/Squatter allocations, not every construction allocation.

The historical column-addressing investigation records the earlier pointer/bias
work. The subsequent version-4 format
uses the smaller header and reverse preorder, which eliminates index bias entirely.

Historical version-4 storage results compare the new layout with version 3 on
the two-vCPU cloud VM, including both
point modes, cached/uncached walks, queries, compact packing, and retained memory.


## Optional point positions

Points are enabled by default. A byte-only C build removes the two point
node columns, their two group bases, packing constraints and temporary point
positions, iterator cache entries, and query cursor point ranges:

```sh
make -C lib/squat BUILD=../../build/squat-byte-only \
  CFLAGS="-O2 -DSQ_INCLUDE_POINTS=0" check all
```

Compile callers with the same `SQ_INCLUDE_POINTS` value. With points disabled,
point getters, point-range seeks, query point-range setters, point column equality functions,
and point snapshot members are **absent** from the C API. Byte getters, seeks,
and query ranges retain their existing behavior. The SIMD cache reconstructs
only the two byte-coordinate columns.

Rust exposes the same choice as a default-enabled `points` Cargo feature:

```sh
cargo build -p tree-sitter-squatter --no-default-features
cargo build --release -p squatter-bench --no-default-features
```

Dependent crates can use `tree-sitter-squatter` with `default-features = false`.
Cargo features are additive: all dependents must leave `points` disabled for a
byte-only library. `HAS_POINT_POSITIONS` reports the linked Rust library setting.
Point methods and attribute fields are omitted from Rust as well. Configure Rust
through Cargo features; a generated C assertion prevents incompatible CFLAGS
from silently changing the FFI snapshot layout.

The 16-byte version-9 header records point support in `format_flags`. Each build
rejects the other mode before interpreting columns. Regenerate older slabs and
slabs from another point mode; they are incompatible with this format.

Historical validation covers both modes, API omission, sanitizers, and
original/mutated corpus checks.


## Named columns and bulk equality

Named equality functions replace the old `SQColumn` selector. For example,
`sq_tree_group_field_equal(tree, group, value)` replaces
`sq_tree_group_equal(tree, group, SQ_COLUMN_FIELD, value)`. Each previously
exposed encoded column has its own function, including byte deltas and point keys,
supertypes, raw display symbols, and grammar symbols. Point functions remain
absent from byte-only builds. These are exact physical-lane masks, with the same
SWAR kernel and encoded-value semantics.

Iterator caches likewise use named lane arrays. A field-only request fills only
fields; a snapshot fills the remaining named attributes once per unpack window.
Two booleans track those states. There is no column-bitmask/ctz dispatch. Byte
coordinates retain absolute u32 SIMD reconstruction. Point fill expands each
u16 key, combines it with one packed group base, and caches the absolute point
as one u64 value.

Historical named-column comparisons record cloud timings and byte-for-byte
compatibility with the storage commit.
