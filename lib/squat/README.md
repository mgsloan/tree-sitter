# Squat C runtime

Tree-squatter is a prototype. No data has been persisted for ongoing use;
temporary test databases do not create a compatibility obligation. All prototype
format, schema, and profile versions remain at 0. Backward compatibility and
migration support are not wanted yet: change the representation directly and
regenerate temporary caches. Tree-sitter's upstream ABI versions are independent.

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
SQGrammar *grammar = sq_grammar_new(ts_tree_language(parsed_tree), &error);
if (!grammar) return;
SQTree *packed = sq_tree_pack(grammar, parsed_tree, options, &error);
if (packed) {
  SQNode root = sq_tree_root_node(packed);
  // packed retains its grammar and does not retain parsed_tree
  uint32_t node_count = sq_node_descendant_count(root);
  (void)node_count;
  sq_tree_delete(packed);
}
sq_grammar_delete(grammar);
```

`make -C lib/squat` builds the static library and C comparison executable.
Link the library before mainline Tree-sitter. Public declarations are in
[`include/tree_sitter/squat.h`](include/tree_sitter/squat.h).

Symbol IDs (including grammar symbol overrides), field IDs, supertype
masks/dictionary IDs, and group waste always use 16 bits. Field and supertype
columns are retained even for grammars that do not use them. Coordinates and
boolean columns keep their existing widths. No build flag or Cargo feature is
needed.

Fixed-width packing uses direct halfword stores. Iterators read IDs from the
slab instead of unpacking and caching copies; only coordinates need expansion.
On x86-64, group equality uses SSE2 comparisons and a lane mask, with a portable
scalar implementation elsewhere. These optimizations preserve the slab format.
See [measurements](experiments/fixed-width.md).

Grammar preparation returns
`SQ_ERROR_OVERFLOW` if symbol IDs (including the two error symbols) or field IDs
need more than 16 bits. Supertype dictionaries retain their existing 65,536-entry
limit and return `SQ_ERROR_DICTIONARY_FULL` if exceeded.

Prepare an `SQGrammar` once and retain it between batches. It owns immutable
symbol, supertype, and direct-field lookup tables. `sq_grammar_copy` shares the
handle using atomic reference counting; `sq_grammar_delete` releases it. There is
no global grammar registry or lookup on packing/loading. Native grammar libraries
must remain loaded while any prepared grammar or tree uses them.

Create worker-local scratch with `sq_pack_context_new(&error)` and pack
with `sq_pack_context_pack(context, grammar, parsed_tree, options, &error)`. Separate
contexts can read the same grammar concurrently. A context reuses scratch across
grammars; `sq_pack_context_trim` releases scratch. Output trees retain the shared metadata and
remain valid after context reuse or deletion. One-shot `sq_tree_pack` also takes
a prepared grammar. Slab loaders take that same handle.

Only the costly supertype dictionary is serialized by `sq_grammar_copy_cache`.
`sq_grammar_new_with_cache` restores it, copying directly from the supplied bytes;
other tables are derived from the language. Invalid dictionaries return an error.
The caller may fall back to `sq_grammar_new`. Rust exposes these operations through
`Grammar::new`, `Grammar::from_cache`, `Grammar::cache`, and `Clone`.

`context-check` checks output equality, reuse, trimming, and ownership; setting
`CONTEXT_FAILURES=1` also injects failure at every pack allocation and checks
recovery. Corpus checks run this against each staged grammar.

`make -C lib/squat check` checks column packing and value-preserving growth and
compaction, including fixed-width grammar limits. For grammar comparisons:

```sh
python3 lib/squat/tests/container.py --output build/squat-check --queries
python3 lib/squat/tests/container.py --output build/squat-sanitize --sanitize --queries
```

The runner defaults to `../../code-corpora`, reads its pinned build image ID,
compiles selected local grammar sources, and executes them in offline Podman
containers. `--grammar NAME` is repeatable. Each fresh output directory contains
logs and grammar/tool/image provenance. No source checkout is modified. Missing
grammars and failed comparisons cause a nonzero exit.

The opt-in endian test requires a little-endian host, a native C compiler, Zig
(for cross-compilation), and `qemu-ppc64`. It statically links the same local
grammar into native and emulated big-endian executables; no guest OS or binfmt
registration is needed. It is not part of `make check` or the container tests.
Use `--bits 32` with `qemu-ppc` to also check cross-pointer-width compatibility.

```sh
python3 lib/squat/tests/endian.py \
  --grammar /path/to/tree-sitter-json --symbol tree_sitter_json \
  --source /path/to/sample.json --output /tmp/squat-endian
```

The output directory must be new. Each executable writes slabs, checks its own
slabs, then reads the other executable's slabs. The probe requires identical
bytes and compares every live node's attributes and topology through copied and
borrowed loaders. Sixteen packing variants cover capacity hints, compaction,
points, and presence-index options. Use a source exceeding 32 groups to exercise
the presence index, and additional grammars/sources for grammar IDs and
grammar overrides. Both cross-endian directions run even if one fails.

Conversion walks raw subtrees iteratively in reverse preorder. Each frame stages
child positions because multiline point offsets cannot be subtracted. Only the
current group's absolute node attributes are buffered; no full-tree node array
is needed. Parent navigation scans backward, using group span bounds to skip groups; cursors retain an ancestor stack.
The cursor supports first/last child, next/previous sibling, parent movement,
and first-child seeking by byte or point. Seeking returns the child index and
leaves the cursor unchanged on failure. Previous-sibling movement and seeking
can scan siblings. `sq_cursor_reset` changes the root while retaining allocated
ancestor storage; it can switch trees. The cursor retains no sibling history or
decoded-column cache.

## Rust traversal APIs

`NodeLike` exposes backend-native `preorder()`, `node_iterator(unpack_cache)`,
`descendants_matching_kinds(&KindSet)`, and child iterators. These use static
dispatch; generic callers do not need to select a representation per node.

`KindSet` is a reusable set of public kind IDs for one language. Filtered scans
include the root, stay inside its subtree, preserve preorder, and deduplicate
requested IDs. Filtering uses a linear preorder scan with constant-time
membership checks.

Individual node getters avoid constructing a full attribute snapshot.
`NodeIteratorLike` exposes the last yielded node's kind, byte range, and full
attributes. Kind and field IDs are read directly from fixed-width storage.
The packed iterator lazily decodes requested coordinates and retains them across
a cache window. Read through the iterator to use that cache; returned
nodes are independent handles. Reads return `None` before iteration and after
exhaustion. The cache hint has no effect on mainline traversal.

`children()`, `named_children()`, and `children_by_field_id()` do not require an
exact count. Field zero yields no children. `has_children()` avoids counting;
`has_named_children()` stops at the first named child. Packed counts, indexed
child access, and parent access can scan, so prefer child iteration and cursors
when visiting many nodes. `CursorLike` also exposes reset and range seeking.


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
unrestricted end scan. Point-free trees use the sibling descent fallback;
byte descent uses integer offsets directly.

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
multi-byte values are byte-swapped on big-endian hosts.
Symbol, field, supertype, and waste columns use direct u16 accesses.

After the header and per-group waste column, columns are ordered: start byte,
end byte, span, symbol, field, supertype, flag bitmaps (`last`,
`extra`, `error`, `missing`), start point, end point. Each group-base column
immediately precedes its corresponding node-value column, with alignment padding
where needed. The optional symbol-presence index and sparse grammar-symbol
overrides follow the columns.

Subtree-span bases are zero when every live value in the group fits in u8;
otherwise they use the actual minimum. Start-column bases retain their actual
minima, and end columns keep their actual maxima and the existing
base-minus-delta encoding. The packer chooses bases after closing the group, so
the choices do not affect group boundaries. Revisit span or coordinate base
selection if zeroing or retaining tighter bounds becomes useful to another
operation.

Point rows and columns share one u16 key per node, with the row delta in the
high byte and column delta in the low byte. Their componentwise group bases are
stored symmetrically as one u64 key, with the row in the high word. The payload
still uses four bytes per node and 16 bytes per group before column padding,
while lexicographic point comparisons now use one integer key.

The serialized header is 16 bytes: a format/flags word, live group
count, allocated group capacity, and supertype-dictionary count. Column and
auxiliary-section offsets are derived from the exact grammar, capacity, and
feature flags. The symbol-presence index has an explicit presence flag. Columns start on eight-byte boundaries (64 in
the experimental alignment build); auxiliary sections remain eight-byte aligned.
Slabs and grammar caches are little-endian on all hosts, including 32-bit hosts.

Each newly packed tree has one private allocation: runtime descriptor, alignment
padding, then the persisted slab. Grammar tables are shared through its retained
prepared handle. `sq_tree_data` / Rust `as_bytes`
returns only the persisted suffix. Builder growth can relocate this allocation;
public trees and handles are immutable. `sq_tree_repack` returns an independent
colocated compact copy with the same physical slot IDs.

Up to eight supertypes use direct byte masks without a dictionary lookup. Larger
supertype sets use a deterministic dictionary derived from the compiled grammar,
shared by trees and contexts using the same prepared grammar. IDs are sorted by mask, independent
of conversion order. The dictionary selects 8- or 16-bit indexes upfront and is
not stored in each slab. Loading uses the prepared dictionary and checks the
header count/width. Grammar analysis returns `SQ_ERROR_DICTIONARY_FULL` if its
conservative mask set exceeds 65,536 entries. Retaining the grammar handle keeps
its tables available even when no trees or contexts remain.
Analysis distinguishes nonterminal extras from ordinary recursive gotos and only
explores hidden definitions reachable from supertypes or hidden extras. Unary
productions avoid building the full predecessor graph.
Historical mask-analysis benchmarks record the dictionary-size, initialization,
conversion, and memory effects.

Physical columns are filled from the beginning in reverse preorder. Nodes use
direct physical slot indexes, and preorder traversal walks toward lower slots.
There is no capacity-minus-count lookup or cache. Growth and compaction copy
used packed words without changing lane phase. Iterator caches decode physical
windows normally and consume them in descending order. Query plans translate
slots to ascending preorder positions only where ordered scan intervals need it.

`sq_tree_from_bytes` copies arbitrary-alignment input into a single colocated
tree allocation and validates topology, coordinates, symbols, fields,
dictionary, and presence entries. `sq_tree_from_bytes_borrowed` validates an
externally owned, immutable, aligned buffer without copying it; deletion frees
only the runtime prefix. The caller keeps that buffer alive until all uses of
the borrowed tree finish. Rust's `Tree::from_bytes_borrowed` returns
`BorrowedTree<'a>`, tying that lifetime to the input slice and exposing read-only
tree APIs through `Deref`.
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
Historical comparisons document the upstream inconsistency.

The known mainline difference in `tests/fixtures/hidden-seek.css` is counted but
ignored by `squatter-bench` unless `--strict-seeks` is used. Other differences
fail. No hidden-node or seek-barrier index is stored.

`tests/seek.c` compares byte seeks and, when enabled, point seeks exactly with the
previous sibling-descent algorithms, independently of mainline differences.
Build it with `make -C lib/squat ../../build/squat/seek-check`, then run
`build/squat/seek-check GRAMMAR_LIBRARY GRAMMAR_SYMBOL SOURCE_LIST`, where
`SOURCE_LIST` contains one source path per line. It checks named/unnamed ranges,
subtree roots, boundaries, and malformed variants of every input.

Query declarations are in [`squat_query.h`](include/tree_sitter/squat_query.h).
Compile once with `sq_query_new`, execute with `sq_query_cursor_exec`, and advance
with `sq_query_cursor_next_match` or `sq_query_cursor_next_capture`. Capture arrays
are borrowed until the next cursor mutation. The query, tree, and callback payload
must outlive execution. C exposes text predicates as metadata; the Rust wrapper
evaluates equality, regex, and membership predicates against supplied source bytes.

Capture iteration returns provisional snapshots. A state may gain
captures or be discarded by longest-match filtering; different states may emit
the same capture. Event order, snapshot contents, and duplicate counts need not match mainline.
Use match iteration for completed, longest matches.

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

The optional lazy cache stores absolute coordinates; IDs are read directly
from the slab. Coordinate decoding
widens unsigned byte/u16 deltas directly from the slab and adds or subtracts a
broadcast group base with AVX2 (eight lanes) or SSE2 (four lanes) on x86-64.
Other platforms use a portable scalar implementation. Single-bit flags remain
packed. Bulk snapshots exclude child and descendant counts; their explicit node
APIs still use ordinary tree scans.

AVX2 and scalar group decoding skip addition for zero bases. SSE2 and per-node
scalar reads retain their arithmetic paths. Subtraction always retains its
base-minus-delta semantics, including when the base is zero.

Field-only and navigation-only consumers do not unpack coordinates. Repeated
attribute reads reuse the same window. Ordinary node handles do not use the
iterator's cache. `SQ_COORDINATE_KERNEL=0/1/2/4` selects automatic, scalar, SSE2,
or AVX2 coordinate reconstruction, with a supported fallback.

`SQ_ITERATOR_CACHE_ALL=2` is the default absolute-coordinate cache. Build mode `0`
(no coordinate cache) remains available for reproducing earlier experiments. The old
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
default/compact packing, and stored/synthetic point configurations.
It measures live allocations from the implementation, rather than estimating
storage from public nodes. Retained sizes include the tree object and auxiliary
allocations; the parser is released first. Construction peaks are separate.

On Linux/glibc with GNU-compatible linker wrapping, build and run:

```sh
make -C lib/squat BUILD=../../build/squat-memory CFLAGS="-O3 -g" \
  ../../build/squat-memory/memory-bench
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

Points are stored by default. Set `SQPackOptions.points` to false to omit the two
point node columns and their two group bases for an individual tree. Point APIs
then treat its input as one long line: every row is zero and every column is the
corresponding byte offset. `sq_tree_has_points` distinguishes stored source
positions from these synthetic positions.

The 16-byte header records point storage in `format_flags`, and every build loads
either layout.

Historical validation covers both modes, sanitizers, and
original/mutated corpus checks.


## Named columns and bulk equality

Named equality functions replace the old `SQColumn` selector. For example,
`sq_tree_group_field_equal(tree, group, value)` replaces
`sq_tree_group_equal(tree, group, SQ_COLUMN_FIELD, value)`. Each previously
exposed encoded column has its own function, including byte deltas and point keys,
supertypes, raw display symbols, and grammar symbols. These are exact
physical-lane masks, with the same SWAR kernel and encoded-value semantics.

Iterator caches likewise use named lane arrays. A field-only request fills only
fields; a snapshot fills the remaining named attributes once per unpack window.
Two booleans track those states. There is no column-bitmask/ctz dispatch. Byte
coordinates retain absolute u32 SIMD reconstruction. Point fill expands each
u16 key, combines it with one packed group base, and caches the absolute point
as one u64 value.

Historical named-column comparisons record cloud timings and byte-for-byte
compatibility with the storage commit.
