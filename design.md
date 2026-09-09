# Squat representation

The idea here is to create a compact yet efficient representation for tree-sitter trees that do not require incremental reparse. It all gets allocated into a contiguous slab, and nodes are enumerated in preorder. No pointers are used, so this can also be used as a format for persistence.

To make the representation compact without much access overhead, a statistical
fact about preorder nodes is exploited. In the space of possible values for a
field, their values are often clustered.

So, the idea is to split the nodes into groups. Each squat group stores the absolute base value for each field. This allows most fields to be `u8`. If a node has a value that doesn't fit, the rest of the block's slots are wasted and it becomes the first node of the next group.

Despite the code below being Rust, this will be implemented in C in `lib/squat/`. Mainline Tree-sitter code will be unmodified.

```rs
struct SquatHeader {
    /// Identifies tree-squatter serialization version.
    magic_bits: u8,
    symbol_bits: u8,
    field_bits: u8,
    group_count: u32,
}

struct SquatGroup {
    /// The other fields in this actually make semantic sense for the group, but not this one. This
    /// is just the min absolute value of subtree_size for all nodes in the group.
    subtree_size: u32,
    start_byte: u32,
    end_byte: u32,
    start_row: u32,
    end_row: u32,
    start_col: u32,
    end_col: u32,
}

struct SquatNode {
  /// Whether there is no later visible sibling.
  is_last_child: bool,
  /// "extra" grammar nodes like comments. Unfortunately not inferrable from symbol.
  is_extra: bool,
  /// Whether this node or a descendant is an error symbol.
  has_error: bool,
  /// Whether this symbol was inserted as part of error recovery (and this indicates an error).
  is_missing: bool,

  /// Count of slots used for descendants of this tree (add group subtree_size). Adding this plus one to the current slot index jumps to the next preorder node outside of this subtree.
  subtree_size: u8,
  /// Start byte offset in the input text (add group start_byte).
  start_byte: u8,
  /// End byte offset in the input text (subtract from group end_byte).
  end_byte_sub: u16,
  /// Start row in the input text (add group start_row).
  start_row: u8,
  /// Start row in the input text (add group end_row).
  end_row_sub: u8,
  /// Start col in the input text, in bytes (add group start_col).
  start_col: u8,
  /// End col in the input text, in bytes (subtract from group end_col).
  end_col_sub: u8,

  symbol: VarBits,

  field: VarBits,
}
```

`SquatHeader` is a real struct but `SquatGroup` and `SquatNode` are not. Instead the values for each field are stored contiguously (struct-of-arrays style). However, no layout info needs to be stored. It's all implied by the fields of `SquatHeader`.

`tools/memory-pareto` was used to determine that `u16` should be used for `end_byte_sub`. This results in `~13.6B/node` whereas `u8` was `15.6B/node`. After that choice, it also determined that `16` slots per group is better than `32`, which was `14.3B/node`.

While all that's needed to know the whole layout is `group_count` / `symbol_bits` / `field_bits`, numbers that make calculating the position of data quickly are also stored in the `SquatHeader` (`nodes_start` / `node_stride`).

Note that the fields for `SquatNode` are not actually grouped. There is one contiguous interval of bytes that has all `symbol` data.

Symbol and field ids use the grammar's required width, with a minimum of two
bits so that SWAR tricks can be used. A nine-bit column holds seven values per
word, wasting one bit per word. Not spanning multiple words allows bitwise tricks to be much faster.

Builtin error symbols are remapped to the two values immediately after the
grammar's real symbol range, then decoded at the API boundary.


## Symbol presence bitmaps

After the `SquatNode`s comes an index of which symbols are present in a given group. This is only present if there are more than 32 groups.

First is a bitmap with `symbol` arity. 0 bit indicates the symbol is rare and so can have an occurrence list. 1 bit indicates that a per-group bitmap is used.

`group_count` is rounded up to the nearest multiple of 32, and this is how many bits is in each bitmap.

When the symbol has a `0` bit, it is a sequence of `u32` slot indexes where the symbol appears. This takes up the same amount of space as the bitmaps. 0xFFFFFFFF is filled in for unused parts of the sequence.

When the symbol has a `1` then it is a bitmap where a `1` indicates that the corresponding group has a node with that symbol.


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

* `walk-forward`: Walks every node in preorder and queries every attribute

* `walk-backward`: Walks every node in reverse preorder and queries every attribute

* `seek-byte`: Finds the deepest node for a byte. Does this 100 times.

* `seek-point`: Finds the deepest node for a point. Does this 100 times.

* `cold-parse`: Cold parse time.

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

  - `NAME-run.json` contains the the effective seed, code-corpora/tool/grammar/query identities, machine/build information, and planned/completed/failed counts. --repeat should repeat the same selected and mutated inputs. A short-circuited run should flush its measurements and explicitly mark summaries as partial.

The benchmark does the following:

1. Parse a batches of N from the selected files. Always records cold parse timings. If `cold-parse` is specified additionally walks both results in parallel and compares every attribute for equality. That's the test part - it is not timed.

2. The mainline bench is then performed on all N.  Then the same with squatter. This order alternates each batch.  Results are collected so they can be equality compared at the end. This result collection must have equal cost for both.

3. The results are compared. The first detected mismatch for a test type is recorded for reporting later (subsequent ones just increment a counter). Unless `--`

The failure count is accumulated, but only the details of the first failure are reported in the output, along with the timings.  It exits with failure if there are any.


# FIXME

* “Every attribute” and full query comparison expose unresolved representation
  requirements. Lines 148–158 (design.md:148) promise broader equivalence than
  the layout currently specifies. Tree-sitter distinguishes public symbols from
  underlying grammar symbols; aliases can make them differ. Query execution also
  consults hidden supertype ancestry. A public-node tree containing one symbol
  per node does not establish that this information survives packing.

  Explicitly list supported attributes and query semantics, then identify their
  stored or reconstructible information. This is an architectural decision to
  settle before implementing the benchmark suite.


# Future work

* Representation of which char in unexpected char nodes

* Evaluate whether storing `byte_len` is better than `end_byte`. Similar for row / col

* Revisit node count

* ABI compatible drop-in

* Make persistence work across BE vs LE?

* Select the threshold for having symbol bitmaps

* Should padding / spacing etc be used to make things align on cache lines etc?

* How to compute magic value - is it a hash of representation version + grammar metadata?

* Sampling was download weighted, but removed.  Consider

  > Within each of these, the number of files selected will be chosen to be roughly proportional the the number of downloads for the most popular language extension for that language in Zed. This may not be possible in all cases - when it greatly deviates it just does its best and warns about which languages are not proportionately represented.
