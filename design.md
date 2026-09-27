# Overview

Tree-squatter is a prototype. No data has been persisted for ongoing use;
temporary test databases do not create a compatibility obligation. All prototype
format, schema, and profile versions remain at 0. Backward compatibility and
migration support are not wanted yet: change the representation directly and
regenerate temporary caches. Tree-sitter's upstream ABI versions are independent.

Tree-squatter provides a compact yet efficient representation for Tree-sitter
trees.

Nodes are listed in reverse-preorder in a contiguous allocation which does not
use pointers. These are divided into groups of 32 nodes. For fields like
`start_byte`, an absolute base is stored for the group, and the nodes store a
single byte offset. If a node's value exceeds what is representable, some slots
are wasted and it gets put in the next group.

# Layout

* `SlabHeader`
* Struct-of-arrays `Group` with `group_capacity`
* Struct-of-arrays `Node` with `slot_capacity`
* Symbol presence bitmaps

# Conversion + compaction

First, memory is allocated to hold all the data for the tree. The initial
allocation is done by estimating `group_capacity` from node count. The mainline
tree is traversed in reverse preorder so that `subtree_size`, `has_error`, etc
can be computed directly without buffering.

If the estimate was too little, it is grown and the data is copied inplace. If
the estimate was too large, it is left that way, but can also be compacted.

`max_subtree_size`, `max_byte`, `max_row`, `min_col`, and `max_col` are computed as it scans. `trailing_waste`, `min_byte`, and `min_row` are known on the last inserted node.

`supertypes` state is inherited on descent.

While the traversal is done in reverse preorder, the physical nodes **could** be
stored in preorder. This was not done because it would cause physical node indices to shift when growing the allocation.

# Slab data

The first word of each slab is little-endian: type in bits 31–24, storage
version in bits 23–16, and flags in bits 15–0. Types are `FF` for trees,
`FE` for symbol presence, `FD` for points, and `FC` for grammar dictionaries.
All storage versions are currently 0.

Tree version 0 uses 32-slot groups, 16-bit span deltas and supertype entries,
and 8-byte column alignment. Display and original grammar IDs use separate compact
domains. Flag bits 4 and 5 independently select one-byte display and grammar IDs
when the corresponding domain fits in 256 slots, including reserved zero and both
error IDs. Otherwise that column uses two-byte IDs. The optional columns,
in storage order, are grammar, extra, error, and missing, with flags in bits
0, 3, 2, and 1 respectively. The grammar
column is omitted when every original ID is recoverable from its display ID.
Presence, point, and grammar dictionary slabs have no flags. Unrecognized
types, versions, or flags are rejected.

```rs
struct SlabHeader {
  /// Little-endian slab type, storage version, and layout flags.
  format_flags: u32,
  group_count: u32,
  /// Actual allocated capacity, including growth beyond the initial estimate.
  group_capacity: u32,
  /// Zero when the grammar uses direct supertype masks instead of a dictionary.
  supertype_dictionary_count: u32,
}

/// A struct of this layout is not used - instead each field is packed into columns.
struct Node {
  /// Whether there is no later visible sibling.
  is_last_child: bool,
  /// "extra" grammar nodes like comments. Not implied by symbol.
  is_extra: bool,
  /// Whether this symbol was inserted as part of error recovery.
  is_missing: bool,
  /// Original node error predicate, including contributions from hidden nodes.
  has_error: bool,

  /// Distance to the subtree's lower physical boundary, including group waste.
  /// Subtract from subtree_size_max to decode the span, then subtract the span
  /// from this node's slot. The next sibling occupies the slot below that boundary.
  subtree_size: u16,
  /// Start byte offset in the input text (add start_byte_base).
  start_byte: u8,
  /// End byte offset in the input text (subtract from end_byte_base).
  end_byte_sub: u16,
  /// Compact public kind after aliasing.
  display_symbol: u8 | u16,
  /// Compact original grammar symbol; omitted when derivable from display_symbol.
  grammar_symbol: u8 | u16,

  /// ID of the field for this node within the parent.
  field_id: u16,

  /// When there are 8 or fewer hidden supertypes, stores a bit mask for which of them occur in the
  /// ancestors. When there are more than 8, all possible combinations are analyzed from the grammar
  /// and given IDs that are used for this field.
  supertypes: u16,
}

/// A struct of this layout is not used - instead each field is packed into columns.
struct Group {
  /// Number of trailing wasted slots, from 0 to 15. Could be a u8.
  trailing_waste: u4,
  subtree_size_max: u32,
  start_byte_base: u32,
  end_byte_base: u32,
}
```

The optional columns end the core slab in the order `grammar_id`,
`extra`, `error`, `missing`. All three flag columns have one bit per physical slot.
Each flag column is omitted when all its values are zero, as recorded by
`SQ_EXTRAS`, `SQ_MISSING`, and
`SQ_ERRORS` in the header. Missing nodes imply the error column is present.
The builder reserves all four optional columns. Finalization removes unused
columns. Columns move in place to their new offsets, including when packing
requests reduced group capacity. The allocation is shrunk when the unused tail
is at least `min(256, allocated_slab_bytes / 2)` bytes. `realloc` may move it.
`Tree::repack_in_place` uses the same path and retains attached side data;
`Tree::repack` returns a separate compact copy.
Smaller tails are excluded from serialization but retained in the allocation.
Points and symbol-presence data use separate sidecar allocations.

`has_error` stores each original visible node's `missing || error_cost > 0`
predicate. This preserves error contributions from omitted hidden nodes without
marking unrelated nodes. `is_error` and `is_missing` also remain exact.

Tree-sitter's hidden nodes are omitted entirely since they are not helpful for
the flat representation without incremental reparse. Their effects are recorded
in `supertypes`, `is_last_child`, and `field`.

`KindId` and `GrammarId` retain Tree-sitter's public and original symbol numbers.
`SquatterKindId` contains only canonical visible public symbols.
`SquatterGrammarId` retains distinct original symbols: visible symbols, hidden
nonterminals exposed by aliases, and conservatively all hidden terminals because
Tree-sitter's alias map covers only nonterminals. Each domain assigns IDs in native
symbol order, reserves zero, and appends `ERROR` and `_ERROR`. Prepared languages
own the bidirectional mappings; compact IDs are scoped to that grammar version.
Queries convert concrete display symbols once after compilation, retaining zero
for wildcards and native IDs for hidden supertype constraints.

The grammar-ID column is first in the optional tail. Each compact display ID has
a default compact grammar ID corresponding to the same native symbol, or zero
for alias-only kinds. Packing tracks whether any emitted grammar ID differs from
this default; finalization omits the column when none do. Reads then recover the
grammar ID through the default lookup. The two compact ID numbers need not match.
For forests, each width is chosen from all participating grammars before packing.
Every present ID column has that constant stride across grammar regions. Omission
of the grammar-ID column is also a slab-wide decision.

Little-endian representation is used on big-endian systems. This is
for simplicity and support for inter-machine communication. Since
big-endian is very rare for host architecture, it is fine for it to
have some performance impacts due to using a non-native
representation.

Persistence still uses `.tree-sitter/big-endian/` on big-endian hosts
because LMDB itself is endian-dependent.

# Symbol presence bitmaps

The optional presence sidecar indexes the same compact display IDs used by node
columns and queries. It reserves one row per compact ID, including zero and both
errors, without rows for omitted hidden or duplicate native symbols.

After the 16-byte header, each row contains `ceil(group_count / 64)` little-endian
`u64` words. A set bit indicates that the group contains a node of that kind.
Unused bits, wasted slots, and unused allocation capacity contribute no membership.


# Compressed points

`PointsData` is created during parsing or packing when `PackOptions::points` is
set. It has a separate allocation and can be persisted, dropped, and reattached
for the exact matching core. It cannot be derived from an existing packed tree.

After the 16-byte header, each group stores two `u64` bases, then a column of
`u16` start deltas and a column of `u16` end deltas. Each base packs a row in the
high 32 bits and a column in the low 32 bits. Each delta packs a row difference
in the high byte and a column difference in the low byte. Start components add
to their minima; end components subtract from their maxima. Waste deltas are zero.
This uses 4 bytes per physical slot plus 16 bytes per group.

With points enabled, packing closes a group whenever any row or column range
would exceed 255. Grouping and physical slot IDs can therefore differ between
point-enabled and point-free trees. Missing persisted points require reparsing;
presence caches can still be reconstructed from the matching core.


# C API

This will be offered as an additional API alongside Tree-sitter itself, keeping Tree-sitter in the repo. As much of the Tree-sitter API as possible will be implemented atop this representation.

A `pack_tree` function will be provided which packs a Tree-sitter tree to bytes. The squat parse functions simply call mainline parse and then pack.


# Rust API

The Rust API will export types and functions for this squat representation, along with traits that work with both the mainline and the squat representation. This way code can be written that is specialized to both representations.


# Queries

Queries use the Tree-sitter query language and cursor API over packed trees.
Eligible rooted patterns execute directly over packed columns; other patterns
use the general NFA. Symbol scans, direct plans, and state staging remain enabled
when a cursor has a finite match limit.

A match limit bounds capture-list storage. It does not define which matches must
survive when the limit is exceeded. Execution strategies may discover and evict
matches in different orders, so limited queries need not return mainline's subset.
Every returned match must still satisfy the query, and the cursor reports that
the limit was exceeded.


# Corpus analysis tool

`crates/corpus-analysis` provides a binary of the same name that provides the following commands using mainline Tree-sitter from this repository:

`corpus-analysis sample` creates a representative sample of input files, including some samples that have rare combinations etc.


# Corpus sampling

`corpus-analysis sample` will create the following file lists, which are simply newline separated paths within `code-corpora`.

* `train-tiny` - files with under 10 nodes

* `train-small` - files under 4kb with at least 10 nodes

* `train-normal` - files between 4kb and 100kb

* `train-large` - files larger than 1mb

* `train-unusual` - files containing node patterns or arities not covered by tiny / small / normal.

All of these will also have `test-*` variants.

Omission of files 100kb to 1mb is intentional. The theory is that these files just make things slow without providing much signal.

`train-unusual` is found by looking for the following things:

* Set of `(parent symbol, field id, child symbol)` - include files that have a novel combination.

* Map of `(parent symbol, field id, child symbol)` to the max found child arity. Thresholds of 10, 50, 100 are used.  If the max found is under 10, then finding a file with more than 10 is considered unusual.  Etc etc for 50 and 100.

* Set of encountered missing nodes


# Main test and benchmark

`crates/squatter-bench` tests and benchmarks mainline vs squatter.  It has the following comprehensive benchmarks and comparison tests:

* `query-matches` / `query-captures`: For every query from the Zed extensions and grammars it walks all matches. query-captures additionally walks all captures.

  - TODO: remove this old ref - consider referencing `bench/zed_corpus.py` and `bench/query_matrix.c` in `../tree-squatter`.  Do not just copy stuff - we want this to be a clean well thought out implementation whereas this stuff got messy.

* `walk-forward`: Walks every visible node in preorder and queries every supported attribute listed below

* `cursor-forward`: Walks every visible node using the ordinary cursor and records node identity without reading attributes

* `seek-byte`: Finds the deepest node for a byte. Does this 100 times.

* `seek-point`: Finds the deepest node for a point. Does this 100 times.

* `cold-parse`: Cold parse time.

The comparison contract covers freshly parsed mainline trees and their packed equivalents, including parses of mutated source text. Supported attributes are public symbol/type, grammar symbol/type, start/end bytes and stored points, named/extra/missing/error/has-error flags, `has_changes` (false for these fresh trees), child and named-child counts, and logical descendant counts. Point-free trees instead expose byte offsets as columns on row zero. The contract also compares parent/child/sibling relationships, child field IDs/names, and named-child navigation. Nodes are identified across representations by their visible preorder ordinal, not their pointer or physical slot. Seek results use the same identity, including null results.

Without a finite match limit, query comparisons cover match/capture order,
pattern and capture IDs, and captured-node identities, including field, anchor,
and supertype semantics. Any text predicates use the same source bytes in both
runs. Limited queries validate returned matches but do not compare their subset
or order with mainline. The contract excludes parse-state and next-parse-state
accessors, incremental edit/reparse behavior, and `node_string` equality while
exact unexpected-character rendering is deferred. These exclusions apply to the
walk and cold-parse comparisons as well.

It takes the following CLI arguments:

* Positional arguments specify which tests/benchmarks to run and which samplings to use. Filepaths are also accepted instead of samplings. If no benchmark is specified, all are run.  If no samplings, it uses train-tiny and train-small.

* `--all` specifies sampling from all files - typically paired with `--count`.

* `--count N` specifies how many files to randomly select from the samplings. Default is the whole sampling.

* `--repeat N` specifies the number of times to repeat a test, to reduce the noisiness of the benchmark.  The median is used.

* `--mutate` specifies randomly performing deletes / inserts / moves before parse to introduce parse errors.

* `--seed N` specifies the seed used for the random samplings and mutations. The seed is not used fragile-ey via a stateful RNG used everywhere.  Instead each random selection of a sampling gets its own RNG based on a hash of the sampling name with the seed. Same for file mutations - hash of relative path with the seed.

* `--short-circuit` exits early on failure.

* `--output NAME` specifies a name to use for the output files. `bench-outputs/NAME-files.jsonl` will contain per-file timings. `NAME-languages.jsonl` contain per-language statistics (0th 50th 90th 95th 99th 100th percentiles). `NAME-aggregate.jsonl` will contain those same percentiles but across everything.

  - These stats include wall-clock millis, CPU runtime millis, instruction counts, and caching statistics (misses, hits, etc).

  - Per-file paired comparison ratios of all those stats should also be computed.  Percentile stats should be made within these ratios as well.

  - Performance counters are skipped if unavailable. Cloud VMs should be configured to make them available.

  - `NAME-run.json` contains the effective seed, code-corpora/tool/grammar/query identities, machine/build information, and planned/completed/failed counts. --repeat should repeat the same selected and mutated inputs. A short-circuited run should flush its measurements and explicitly mark summaries as partial.

The benchmark does the following:

1. Parse batches of N from the selected files. Always record cold parse timings, including conversion for squatter. If `cold-parse` is specified, additionally walk both results in parallel and compare them using the supported-attribute contract above. That's the test part - it is not timed.

2. The mainline bench is then performed on all N.  Then the same with squatter. This order alternates each batch.  Results are collected so they can be equality compared at the end. This result collection must have equal cost for both.

3. The results are compared. The first detected mismatch for a test type is recorded for reporting later (subsequent ones just increment a counter). If `--short-circuit` is set, stop at the first detected mismatch and flush the partial results.

The failure count is accumulated, but only the details of the first failure are reported in the output, along with the timings.  It exits with failure if there are any.


# Future work

* Representation of which char in unexpected char nodes

* ABI compatible drop-in

* Make persistence work across BE vs LE?

* Sampling was download weighted, but removed.  Consider

  > Within each of these, the number of files selected will be chosen to be roughly proportional the the number of downloads for the most popular language extension for that language in Zed. This may not be possible in all cases - when it greatly deviates it just does its best and warns about which languages are not proportionately represented.
