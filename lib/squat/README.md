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
Backward sibling cursor movement caches u32 sibling slots in open cursor frames.

`SQCursor` remains the default. `SQCachedCursor` has the same navigation methods
under the `sq_cached_cursor_*` prefix and lazily decodes whole groups of accessed
columns. It follows the batched size/extent cache in `../main`'s
`lib/src/squatter/api_packed.c`, adapted to this slab's fixed-width columns.
Each column retains one group; there is no shared mutable tree cache. On x86-64
with 16-slot groups, cursor handles occupy 32 and 1,088 bytes respectively,
excluding the identical ancestor/sibling storage. Allocation is per cursor;
column decoding happens on first access, not at allocation.

Both types offer a bulk attribute snapshot (`sq_cursor_attributes` /
`sq_cached_cursor_attributes`). Returned `SQNode` handles are ordinary nodes:
accessing their attributes directly bypasses the cursor cache. Child and
descendant counts still use ordinary node scans to avoid evicting cached columns.
The serialized format and query engine's default cursor are unchanged.

Rust exposes `Node::walk()` and `Node::walk_cached()`, returning `Cursor` and
`CachedCursor`. Both implement `CursorLike`, including `attributes()`:

```rust,ignore
let mut cursor = packed.root_node().walk_cached()?;
let attributes = cursor.attributes();
println!("{}: {}..{}", attributes.kind, attributes.start_byte, attributes.end_byte);
cursor.goto_first_child();
```

See [cursor measurements](experiments/README.md#cursor-caching) for workloads
where decoding reuse helps and where its overhead outweighs the savings.

The version-2 serialized header is 40 bytes. Every section and column starts on an
eight-byte boundary. Slabs are native-endian and need the exact matching grammar;
there is no grammar fingerprint in this version. `sq_tree_from_bytes` copies and
validates layout, topology, coordinate arithmetic, symbols, fields, dictionaries,
presence entries, and sparse field-lookup exceptions. `sq_tree_repack` returns an independent compact copy, so
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

Some grammars inherit field lookups through nodes made visible by aliasing. The
lookup can return a grandchild, which differs from the nearest field assigned to
a node during cursor traversal. A sparse section after the supertype dictionary
stores these exceptions as `(parent slot, field ID, result slot)` u32 triples,
sorted by parent and field. `UINT32_MAX` represents a null result. The two final
header words locate/count this section; both are zero when it is absent.
The [comparison with ../main](experiments/field-lookup-review.md) reproduces the
case its packed engine misses and explains the storage tradeoff.

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
