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

The version-3 serialized header is 32 bytes; earlier versions are rejected. Every section and column starts on an
eight-byte boundary. Slabs are native-endian and need the exact matching grammar;
there is no grammar fingerprint in this version. `sq_tree_from_bytes` copies and
validates layout, topology, coordinate arithmetic, symbols, fields, dictionaries,
presence entries. `sq_tree_repack` returns an independent compact copy, so
existing nodes stay valid. Slab data contains no pointers. The small owning
`SQTree` handle retains the grammar and derived layout metadata outside the slab.

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
physical slots and reads leading waste only at group boundaries. Exhaustion is
permanent. The tree must outlive both the iterator and returned ordinary nodes.

`sq_node_iterator_attributes` and `sq_node_iterator_field_id` read the last yielded
node; they return zeroed attributes / field zero before the first yield and after
exhaustion. Rust exposes `Node::node_iterator(bool)` and a fused `NodeIterator`;
its corresponding accessors return `None` outside a yielded position. The older
allocation-free `Node::preorder()` remains available.

The optional lazy cache stores one group's display symbols, grammar symbols, and
fields as u16 lanes. Fixed-width coordinate/flag columns keep their narrow reads.
A field-only consumer unpacks only fields; a navigation-only consumer never
unpacks anything. Repeated attribute reads reuse the same group. Both modes use
the same attribute construction code as the cursor.

Portable unpacking expands four packed fields into u16 lanes with masks and
shifts. Explicit SSE2 widens 8-bit columns on x86-64; 16-bit columns copy directly
on little-endian machines. Automatic variable-width decoding uses BMI2 PDEP on
supported Intel CPUs, the vendor measured here, and portable SWAR elsewhere.
AVX2 variable shifts and byte shuffles remain available for experiments.
`SQ_UNPACK_KERNEL=1/2/3/4` selects scalar/SWAR/BMI2/AVX2 at build time; unavailable
hardware selections fall back to SWAR. No slab format or cursor API changes.

For the cache experiment, `SQ_ITERATOR_CACHE_ALL=1` also caches coordinate and
flag columns and their group bases. This is an alternative build of the cached
mode, not an additional public API. Counts still use ordinary tree scans. The
default (`0`) caches only IDs; compare both builds before choosing the larger
cache for a workload.
