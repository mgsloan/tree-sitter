# Squat representation

The idea here is to create a compact yet efficient representation for tree-sitter trees that do not require incremental reparse. It all gets allocated into a contiguous slab, and nodes are enumerated in preorder. No pointers are used, so this can also be used as a format for persistence.

To make the representation compact without much access overhead, a statistical
fact about preorder nodes is exploited. In the space of possible values for a
field, their values are often clustered.

So, the idea is to split the nodes into groups. Each squat group stores the absolute base value for each field. This allows most fields to be `u8`. If a node is encountered that has a field that is not representable, it gets put in a different group.

## Slab layout

* `SlabHeader`
* Struct-of-arrays `Group` with `group_capacity`
* Struct-of-arrays `Node` with `slot_capacity`
* Symbol presence bitmaps
* Supertype dictionary
* Sparse field-lookup exceptions (version 2 amendment; see below)

The slab can be directly written during conversion by somewhat overestimating `group_capacity` from node count. This version uses 16 slots per group, so `slot_capacity = 16 * group_capacity` and `slot_count = 16 * group_count`. Counts include partially occupied groups and their wasted slots; capacities also include unused allocation space.

Construction fills the suffix of each column. The active groups occupy the last `group_count` entries of every group column, and active slots occupy the last `slot_count` entries of every node column. Logical group and slot indexes start at zero within these active suffixes. Readers add the corresponding capacity-minus-count offset when accessing a column. The root is at logical slot `Group[0].leading_waste`.

If it runs out of space, the capacities grow geometrically and each column's active suffix is relocated to its new position. There is an option to repack it to save space or for persistence, or if the estimates were off enough that it's worth it. Repacking sets `group_capacity = group_count` and compacts each column individually, preserving logical slot indexes and intra-group waste.

During either growth or compaction, packed values must be repacked if their lane positions within words change. For example, a nine-bit column holds seven values per word, so shifting its active suffix by 16 slots changes its lane alignment. A raw byte copy is sufficient only when the source and destination packing align; otherwise, values must be placed into their new lanes, preserving the unused bits between words' value groups.

## Slab data

Despite the code below being Rust, this will be implemented in C in `lib/squat/`. Mainline Tree-sitter code will be unmodified.

```rs
struct SlabHeader {
    /// Identifies tree-squatter serialization version and flags.
    magic_bits: u8,

    group_count: u32,
    group_capacity: u32,

    /// Offset in the slab where `Group` data starts.
    groups_byte_offset: u32,

    nodes_byte_offset: u32,

    /// Zero when the symbol presence index is absent.
    symbol_presence_byte_offset: u32,

    /// Both zero in direct supertype-mask mode.
    supertype_dictionary_byte_offset: u32,
    supertype_dictionary_count: u32,

    /// Sparse (parent slot, field ID, result slot) u32 triples. Both zero if absent.
    field_exceptions_byte_offset: u32,
    field_exceptions_count: u32,
}

struct Node {
  /// Whether there is no later visible sibling.
  is_last_child: bool,
  /// "extra" grammar nodes like comments. Unfortunately not inferrable from symbol.
  is_extra: bool,
  /// Whether this node or a descendant is an error symbol or is missing.
  has_error: bool,
  /// Whether this symbol was inserted as part of error recovery (and this indicates an error).
  is_missing: bool,

  /// Physical span after this node (add min_subtree_size), including padding
  /// before the next node outside the subtree. Adding this plus one to the
  /// current slot index reaches that node, or slot_count at the end of the tree.
  subtree_size: u8,
  /// Start byte offset in the input text (add min_byte).
  start_byte: u8,
  /// End byte offset in the input text (subtract from max_byte).
  end_byte_sub: u16,
  /// Start row in the input text (add min_row).
  start_row: u8,
  /// End row in the input text (subtract from max_row).
  end_row_sub: u8,
  /// Start col in the input text, in bytes (add min_start_col).
  start_col: u8,
  /// End col in the input text, in bytes (subtract from max_end_col).
  end_col_sub: u8,

  /// Supertypes mask or dictionary index.
  supertypes: u8,

  /// Raw symbol after aliasing; public-symbol mapping happens on read.
  display_symbol: VarBits,

  /// Original grammar symbol before aliasing.
  grammar_symbol: VarBits,

  field: VarBits,
}

struct Group {
  /// Number of leading wasted slots, from 0 to 15. Could be a u8.
  leading_waste: u4,

  min_subtree_size: u32,
  min_byte: u32,
  max_byte: u32,
  min_row: u32,
  max_row: u32,
  min_start_col: u32,
  max_end_col: u32,
}
```

`SlabHeader` is a real struct but `Group` and `Node` are not. Instead the values for each field are stored contiguously (struct-of-arrays style). The header's counts, capacities, offsets, and flags, together with the matching grammar and representation version, determine the layout. Group and node columns appear in the order above. Each column and each slab section starts at an eight-byte boundary; column lengths are computed from their capacities, with trailing alignment padding. Bools and `u4` values are packed into 64-bit words, and `VarBits` uses the word layout described below. The grammar determines symbol/field widths and the supertype count. Region offsets point to the beginnings of the allocated regions, including unused prefixes.

`corpus-analysis memory-pareto` was used to determine that `u16` should be used
for `end_byte_sub`. This results in `~13.6B/node` whereas `u8` was `15.6B/node`.
After that choice, it also determined that `16` slots per group is better than
`32`, which was `14.3B/node`.

FIXME: include up-to-date memory-pareto info here

Note that the fields for `Node` are not actually grouped. There is one contiguous interval of bytes that has all `display_symbol` data.

Symbol and field ids use the grammar's required width, with a minimum of two
bits so that SWAR tricks can be used. A nine-bit column holds seven values per
word, wasting one bit per word. Not spanning multiple words allows bitwise tricks to be much faster.

Builtin error symbols are remapped to the two values immediately after the
grammar's real symbol range, then decoded at the API boundary.

Tree-sitter's hidden nodes are omitted entirely since they are not helpful for the flat representation without incremental reparse. Their effects are recorded in `supertypes`, `is_last_child`, and `field`.

Field lookup can traverse an aliased wrapper and return a grandchild even though
the cursor assigns the field to the wrapper. Version 2 adds a sparse exception
section for these cases, sorted by parent slot then field ID. A result slot of
`u32::MAX` means null. Ordinary lookups still use the node's field column. These
exceptions are computed bottom-up during conversion; the header is now 40 bytes.

Public symbol is mapped from raw display symbol at read time.

`is_named` is looked up based on the raw `display_symbol`.

EXPERIMENT: store grammar_symbol in a sparse index (only used for aliases). Fast to know from grammar if a display symbol might have a different grammar symbol.

EXPERIMENT: try field interspersal

EXPERIMENT: Make things align on cache lines etc

EXPERIMENT: Try different node counts.

## Supertypes

Since hidden nodes are omitted, supertype information is needed.  There are two modes for this, distinguished by a flag in the SlabHeader:

1. Stored directly in the `supertypes: u8`, when there are 8 or less potential supertypes.

2. An index into a dictionary of bitmaps where each bitmap has N bits where N is the supertypes count. This requires building up the dictionary as it goes. Each entry occupies `ceil(N / 64)` 64-bit words, with unused high bits zeroed. Its location and entry count are stored in `SlabHeader`, so its byte length is `supertype_dictionary_count * ceil(N / 64) * 8`. The dictionary is staged separately and appended after grouping is complete.

FIXME: For now if there are more than 256 dictionary entries it will crash.

EXPERIMENT: Make supertypes a VarBits representation. Allows omitting it when there are none.

## Symbol presence bitmaps

After the `Node`s comes an index of which public display symbols are present in a given group. This is only present if there are more than 32 groups. The builder applies public-symbol mapping to each raw `display_symbol` before indexing it, so different raw IDs with the same public ID contribute to the same entry. Queries use this public ID directly; the node columns retain raw IDs.

Let `P` be the grammar's symbol count plus alias count plus the two remapped builtin error symbols. The index reserves an entry for each ID in this range, including IDs not used by the public map. Builtin error IDs use the same compact remapping as the node columns. First is a mode bitmap of `P` bits, rounded up to whole 64-bit words. A 0 bit indicates the symbol is rare and uses an occurrence list. A 1 bit indicates that a per-group bitmap is used.

Let `G` be `group_count` rounded up to the nearest multiple of 32. After the mode bitmap are `P` entries in public-ID order, each occupying `G / 8` bytes. This determines the index's total byte length from the grammar and header.

When the symbol has a `0` bit, its entry is a sorted sequence of `u32` logical slot indexes where the symbol appears. This mode is used only when all occurrences fit in the entry; 0xFFFFFFFF fills unused parts of the sequence.

When the symbol has a `1` then its entry is a bitmap where a `1` indicates that the corresponding logical group has a node with that symbol. Bits beyond `group_count` are zero. Both modes omit wasted slots and unused allocation space. The index is built after grouping fixes the logical slot indexes.

EXPERIMENT: try different thresholds for symbol bitmaps

## Conversion algorithm

The mainline tree is walked in reverse preorder: descend through children right-to-left and emit each parent after its children, filling slab groups from right to left. It walks nodes until one has a field that doesn't fit or until the group is full. It saves node references for the current group, retaining `Subtree` handles for inline leaves. Group deltas are encoded only once the group's bases are final.

The traversal stack tracks absolute byte/point positions, inherited fields and supertype masks, sibling status, and subtree-end boundaries. The current group's scratch entries retain the conversion-derived values needed at encoding time alongside the node references; these values cannot all be recovered from a node reference alone. Intrinsic attributes can be reread from the mainline nodes when the group closes.

`min_subtree_size`, `max_byte`, `max_row`, `min_col`, and `max_col` are computed as it scans. `leading_waste`, `min_byte`, and `min_row` are known on the last inserted node. Each candidate is checked against the resulting extrema for the whole group. If it fails, the accepted group is closed and the candidate is retried in a new group to the left, recomputing its physical span after inserting padding.

For a node at logical slot `i`, let `end` be the slot of the first node outside its subtree, or `slot_count` for a subtree reaching the end of the tree. Its decoded physical span is `end - i - 1`. This includes all intervening waste, including padding immediately before `end`. A leaf can therefore have a nonzero span. To find a first child, advance to the next occupied slot, skipping group-leading waste, and check that it is less than `end`. The API's `descendant_count` counts occupied slots in `[i, end)`, including the node itself as mainline does. Subtree jumps land directly on `end` and need no padding normalization.

During construction, positions and subtree boundaries are recorded as distances from the right edge of the active tree, so growing the slab or adding groups to the left does not invalidate them. Once a node is placed, later padding is inserted before it and cannot change its span. Final logical indexes are derived once `slot_count` is known.

`has_error` state is maintained bottom up, including errors and missing nodes under omitted hidden nodes. Conversion can also read the equivalent mainline summary, `ts_subtree_error_cost() > 0`.

`supertypes` state is inherited on descent and saved for emission on ascent. A visible node receives the incoming mask; its children start a fresh mask, whereas a hidden node's children inherit the incoming mask. In either case, add the current node's own supertype bit, if any, to its children's mask. Resolved fields and sibling status likewise survive omitted hidden nodes.

EXPERIMENT: try buffering the absolute values instead of writing down pointers

EXPERIMENT: try eagerly filling without checking if the fields fit to reduce branching. Optimistically figure out the full group values, and then see if the nodes fit. Candidate placements remain provisional until accepted. If a split is required, recompute affected slot positions and physical subtree spans, including those of buffered ancestors, before encoding or committing traversal summaries that depend on placement.


# C API

This will be offered as an additional API alongside Tree-sitter itself, keeping Tree-sitter in the repo. As much of the Tree-sitter API as possible will be implemented atop this representation.

A `pack_tree` function will be provided which packs a Tree-sitter tree to bytes. The squat parse functions simply call mainline parse and then pack.


# Rust API

The Rust API will export types and functions for this squat representation, along with traits that work with both the mainline and the squat representation. This way code can be written that is specialized to both representations.


# Corpus analysis tool

`crates/corpus-analysis` provides a binary of the same name that provides the following commands using mainline Tree-sitter from this repository:

`corpus-analysis sample` creates a representative sample of input files, including some samples that have rare combinations etc.

`corpus-analysis memory-pareto` explores the tradeoff space of memory layout choices.


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

* TODO: Consider more things that could actually reveal real bugs etc


# Main test and benchmark

`crates/squatter-bench` tests and benchmarks mainline vs squatter.  It has the following comprehensive benchmarks and comparison tests:

* `query-matches` / `query-captures`: For every query from the Zed extensions and grammars it walks all matches. query-captures additionally walks all captures.

  - TODO: remove this old ref - consider referencing `bench/zed_corpus.py` and `bench/query_matrix.c` in `../tree-squatter`.  Do not just copy stuff - we want this to be a clean well thought out implementation whereas this stuff got messy.

* `walk-forward`: Walks every visible node in preorder and queries every supported attribute listed below

* `walk-backward`: Walks every visible node in reverse preorder and queries every supported attribute listed below

* `seek-byte`: Finds the deepest node for a byte. Does this 100 times.

* `seek-point`: Finds the deepest node for a point. Does this 100 times.

* `cold-parse`: Cold parse time.

The comparison contract covers freshly parsed mainline trees and their packed equivalents, including parses of mutated source text. Supported attributes are public symbol/type, grammar symbol/type, start/end bytes and points, named/extra/missing/error/has-error flags, `has_changes` (false for these fresh trees), child and named-child counts, and logical descendant counts. It also compares parent/child/sibling relationships, child field IDs/names, and named-child navigation. Nodes are identified across representations by their visible preorder ordinal, not their pointer or physical slot. Seek results use the same identity, including null results.

Query comparisons cover match/capture order, pattern and capture IDs, and captured-node identities, including field, anchor, and supertype semantics. Any text predicates use the same source bytes in both runs. The contract excludes parse-state and next-parse-state accessors, incremental edit/reparse behavior, and `node_string` equality while exact unexpected-character rendering is deferred. These exclusions apply to the walk and cold-parse comparisons as well.

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

* How to compute magic value - is it a hash of representation version + grammar metadata?

* Sampling was download weighted, but removed.  Consider

  > Within each of these, the number of files selected will be chosen to be roughly proportional the the number of downloads for the most popular language extension for that language in Zed. This may not be possible in all cases - when it greatly deviates it just does its best and warns about which languages are not proportionately represented.
