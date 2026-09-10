# Squat representation

The idea here is to create a compact yet efficient representation for tree-sitter trees that do not require incremental reparse. Newly packed trees use one allocation containing runtime metadata followed by a contiguous persisted slab. Physical nodes are stored in reverse preorder; traversal APIs still enumerate preorder. The persisted portion contains no pointers.

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

The slab can be directly written during conversion by somewhat overestimating `group_capacity` from node count. This version uses 16 slots per group, so `slot_capacity = 16 * group_capacity` and `slot_count = 16 * group_count`. Counts include partially occupied groups and their wasted slots; capacities also include unused allocation space.

Construction appends to the prefix of every column in reverse preorder. Active
groups occupy indexes `0..group_count`, and physical node slots occupy
`0..slot_count`. Unused lanes are at the high end of each group, followed by
unused group capacity. The root is the highest occupied physical slot:
`slot_count - Group[group_count - 1].trailing_waste - 1`.

Node handles contain direct physical slots. Preorder walks toward decreasing
slots and skips trailing group waste. Reads use the column offset and physical
index directly; there is no `group_capacity - group_count` cache or adjustment.
Ordered query plans translate physical slots to ascending preorder positions at
the scan boundary. Iterator caches unpack physical windows in ascending order,
then consume their lanes in reverse as preorder advances.

Growth and compaction recompute column locations but preserve physical indexes.
Because each column starts with its first physical lane, packed-word phase is
unchanged even for nine-bit IDs. Used words can be copied directly. Repacking
sets capacity to count and returns an independent tree with identical slot IDs.

The runtime prefix contains `SQTree`, its supertype list, and alignment padding.
The persisted header begins immediately afterward. Internal builder operations
update their tree pointer when growing this combined allocation; public trees
and their node handles never move. The copying loader instead owns a separate
runtime prefix and payload. The borrowed loader owns only its runtime prefix and
retains the caller's immutable, aligned payload without copying or freeing it.

## Optional point positions

Row and column storage is optional at compile time (`SQ_INCLUDE_POINTS=0` in C,
or disabling the default `points` Cargo feature). Byte-only builds omit the four
row/column columns, their group bases, packing constraints, cache lanes, and
point APIs and snapshot members. Byte positions and byte-range APIs remain.
The layout below describes the default build with points enabled.

The 16-byte version-4 header has a format/flags word. Readers reject other
versions, point modes, group sizes, alignments, and unknown flags. Old slabs
must be regenerated.

## Slab data

Despite the code below being Rust, this will be implemented in C in `lib/squat/`. Mainline Tree-sitter code will be unmodified.

```rs
struct SlabHeader {
    /// Native-endian format/version, point/group/alignment flags, optional index.
    format_flags: u32,
    group_count: u32,
    /// Actual allocated capacity, including growth beyond the initial estimate.
    group_capacity: u32,
    /// Zero when the grammar uses direct supertype masks instead of a dictionary.
    supertype_dictionary_count: u32,
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

  /// Distance to the subtree's lower physical boundary, including group waste.
  /// Add min_subtree_size to decode the span, then subtract it from this node's
  /// slot. The next sibling, when present, occupies the slot below that boundary.
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
  /// Number of trailing wasted slots, from 0 to 15. Could be a u8.
  trailing_waste: u4,

  min_subtree_size: u32,
  min_byte: u32,
  max_byte: u32,
  min_row: u32,
  max_row: u32,
  min_start_col: u32,
  max_end_col: u32,
}
```

`SlabHeader` is a real struct but `Group` and `Node` are not. Instead the values for each field are stored contiguously (struct-of-arrays style). The header's counts, capacity, and flags, together with the matching grammar and representation version, determine the layout. Group and node columns appear in the order above. Each column and each slab section starts at an eight-byte boundary; column lengths are computed from their capacities, with trailing alignment padding. Bools and `u4` values are packed into 64-bit words, and `VarBits` uses the word layout described below. The grammar determines symbol/field widths and the supertype count. Derived column offsets point to their first physical entry; unused capacity follows the active entries.

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

Field lookup uses the first visible child carrying the requested field, with no
fields on ERROR parents. Mainline's lookup API can instead inherit through an
alias-visible wrapper and return a grandchild whose field is absent from the
parent's visible children. Tests count these as expected mismatches only when
squat agrees with mainline's visible-child cursor. Other field mismatches fail.
The sparse field-exception section remains removed. Version 4 uses a 16-byte
header and reverse-preorder physical slots; the loader rejects earlier formats.

Public symbol is mapped from raw display symbol at read time.

`is_named` is looked up based on the raw `display_symbol`.

EXPERIMENT: store grammar_symbol in a sparse index (only used for aliases). Fast to know from grammar if a display symbol might have a different grammar symbol.

EXPERIMENT: try field interspersal

EXPERIMENT: Make things align on cache lines etc

EXPERIMENT: Try different node counts.

## Supertypes

Since hidden nodes are omitted, supertype information is needed. There are two modes, determined by the matching grammar's supertype count:

1. Stored directly in the `supertypes: u8`, when there are 8 or less potential supertypes.

2. An index into a dictionary of bitmaps where each bitmap has N bits where N is the supertypes count. This requires building up the dictionary as it goes. Each entry occupies `ceil(N / 64)` 64-bit words, with unused high bits zeroed. Its entry count is stored in `SlabHeader`, so its byte length is `supertype_dictionary_count * ceil(N / 64) * 8`. Its location is derived: immediately after the columns and optional symbol-presence index. The dictionary is staged separately and appended after grouping is complete.

Packing returns `SQ_ERROR_DICTIONARY_FULL` if more than 256 dictionary entries are needed.

EXPERIMENT: Make supertypes a VarBits representation. Allows omitting it when there are none.

## Symbol presence bitmaps

After the `Node`s comes an index of which public display symbols are present in a given group. This is only present if there are more than 32 groups. The builder applies public-symbol mapping to each raw `display_symbol` before indexing it, so different raw IDs with the same public ID contribute to the same entry. Queries use this public ID directly; the node columns retain raw IDs.

Let `P` be the grammar's symbol count plus alias count plus the two remapped builtin error symbols. The index reserves an entry for each ID in this range, including IDs not used by the public map. Builtin error IDs use the same compact remapping as the node columns. First is a mode bitmap of `P` bits, rounded up to whole 64-bit words. A 0 bit indicates the symbol is rare and uses an occurrence list. A 1 bit indicates that a per-group bitmap is used.

Let `G` be `group_count` rounded up to the nearest multiple of 32. After the mode bitmap are `P` entries in public-ID order, each occupying `G / 8` bytes. This determines the index's total byte length from the grammar and header.

When the symbol has a `0` bit, its entry is a descending sequence of `u32` physical slot indexes where the symbol appears, following preorder. This mode is used only when all occurrences fit in the entry; 0xFFFFFFFF fills unused parts of the sequence.

When the symbol has a `1` then its entry is a bitmap where a `1` indicates that the corresponding physical group has a node with that symbol. Bits beyond `group_count` are zero. Both modes omit wasted slots and unused allocation space. The index is built after grouping fixes the physical slot indexes.

EXPERIMENT: try different thresholds for symbol bitmaps

## Conversion algorithm

The mainline tree is walked in reverse preorder: descend through children right-to-left and emit each parent after its children, appending physical slab groups from left to right. It walks nodes until one has a field that doesn't fit or until the group is full. It saves node references for the current group, retaining `Subtree` handles for inline leaves. Group deltas are encoded only once the group's bases are final.

The traversal stack tracks absolute byte/point positions, inherited fields and supertype masks, sibling status, and lower subtree boundaries. The current group's scratch entries retain the conversion-derived values needed at encoding time; these values cannot all be recovered from a node reference alone.

`min_subtree_size`, `max_byte`, `max_row`, `min_col`, and `max_col` are computed as it scans. `trailing_waste`, `min_byte`, and `min_row` are known on the last inserted node. Each candidate is checked against the resulting extrema for the whole group. If it fails, the accepted group is closed and the candidate is retried in a new group at the end, recomputing its physical span after inserting padding.

For a node at physical slot `i`, its decoded span is the distance to its lower
subtree boundary: `first = i - (group_span_base + node_span_delta)`. The subtree
occupies the valid slots in `[first, i]`. This interval includes intervening
trailing group waste; even a leaf can have a nonzero physical span. The first
child is the next occupied lower slot, provided it is at least `first`.
`descendant_count` counts occupied slots in the interval, including the node.
The next sibling is at `first - 1`, or `UINT32_MAX` when the traversal is exhausted.

Construction records boundaries as physical indexes from the beginning. New
groups and padding are appended above existing nodes, so their indexes and
spans remain stable across growth. There is no final slot-index rebasing.

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

* `cursor-forward`: Walks every visible node using the ordinary cursor and records node identity without reading attributes

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
