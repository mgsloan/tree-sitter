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

`sq_cursor_attributes` reads a bulk attribute snapshot. Rust exposes it through
`Node::walk()` and `Cursor::attributes()`:

```rust,ignore
let mut cursor = packed.root_node().walk()?;
let attributes = cursor.attributes();
println!("{}: {}..{}", attributes.kind, attributes.start_byte, attributes.end_byte);
cursor.goto_first_child();
```

Packed reads are inline: flags, 8-bit and 16-bit node columns, and 32-bit group
bases use specialized loads on little-endian hosts. Variable-width IDs use the
non-straddling lane decoder. Big-endian hosts retain native-word extraction.
This does not change the serialized layout.

The version-4 serialized header is 16 bytes: a format/flags word, live group
count, allocated group capacity, and supertype-dictionary count. Column and
auxiliary-section offsets are derived from the exact grammar, capacity, and
feature flags. The symbol-presence index has an explicit presence flag. All
previous versions are rejected. Columns start on eight-byte boundaries (64 in
the experimental alignment build); auxiliary sections remain eight-byte aligned.
Slabs are native-endian.

Newly packed trees use one allocation: runtime descriptor, supertype metadata,
alignment padding, then the persisted slab. `sq_tree_data` / Rust `as_bytes`
returns only the persisted suffix. Builder growth can relocate this allocation;
public trees and handles are immutable. `sq_tree_repack` returns an independent
colocated compact copy with the same physical slot IDs.

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
The [comparison with ../main](experiments/field-lookup-review.md) documents the
upstream inconsistency and the removed version-2 compatibility mechanism.

Known mainline seek differences are counted but ignored by default, as requested
by the human. Use `--strict-seeks` for the container runner or `SQ_STRICT_SEEKS=1`
for the C executable to investigate them. The fixture `tests/fixtures/hidden-seek.css`
is a minimal valid-input repro. No hidden-node or seek-barrier index is stored.

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
packed; child and descendant counts still use ordinary tree scans.

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

`SQ_ITERATOR_CACHE_ALL=2` is the default absolute-coordinate cache. Historical
build modes `0` (IDs only) and `1` (u16 deltas and flags) remain available for
reproducing earlier experiments. The public boolean constructor still selects
cached or uncached operation. No slab format or cursor API changes.

`SQ_ITERATOR_UNPACK_SLOTS=32/64/128` widens the iterator cache independently of
`SQ_GROUP_SIZE`. With the default 16-slot slab groups, these windows decode ahead
across 2/4/8 groups without changing serialization or node addresses. Every group
uses its own bases during reconstruction, and the final window stops at the last
live group. The default unpack window remains one group.

See the [absolute-coordinate cache benchmark](experiments/iterator-absolute-results-2026-09-09.md)
for cached/uncached comparisons at all four window sizes.

## Memory benchmark

The [measured memory comparison](experiments/memory-results-2026-09-10.md) covers
mainline and Squatter, default/compact packing, and point-enabled/byte-only builds.
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

The [column-addressing investigation](experiments/column-addressing-results-2026-09-10.md)
records the earlier pointer/bias investigation. The subsequent version-4 format
uses the smaller header and reverse preorder, which eliminates index bias entirely.

The [version-4 storage results](experiments/storage-v4-results-2026-09-10.md)
compare the new layout with version 3 on the two-vCPU cloud VM, including both
point modes, cached/uncached walks, queries, compact packing, and retained memory.


## Optional row/column positions

Points are enabled by default. A byte-only C build removes the four row/column
node columns, their four group bases, packing constraints and temporary point
positions, iterator cache entries, and query cursor point ranges:

```sh
make -C lib/squat BUILD=../../build/squat-byte-only \
  CFLAGS="-O2 -DSQ_INCLUDE_POINTS=0" check all
```

Compile callers with the same `SQ_INCLUDE_POINTS` value. With points disabled,
point getters, point-range seeks, query point-range setters, point column IDs,
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

The 16-byte version-4 header records point support in `format_flags`. Each build
rejects the other mode before interpreting columns. Regenerate version-3 slabs
and slabs from another point mode; they are incompatible with this format.

[Validation and compiled allocation sizes](experiments/optional-points-validation-2026-09-10.json)
cover both modes, API omission, sanitizers, and original/mutated corpus checks.
