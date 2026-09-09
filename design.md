# Squat representation

The idea here is to create a compact yet efficient representation for tree-sitter trees that do not require incremental reparse. It all gets allocated into a contiguous slab, and nodes are enumerated in preorder. No pointers are used, so this can also be used as a format for persistence.

To make the representation compact without much access overhead, a statistical
fact about preorder nodes is exploited. In the space of possible values for a
field, their values are often clustered.

So, the idea is to split the nodes into groups. Each squat group stores the absolute base value for each field. This allows most fields to be `u8`. If a node has a value that doesn't fit, the rest of the block's slots are wasted and it becomes the first node of the next group.

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

# C API

This will be offered as an additional API alongside Tree-sitter itself, keeping Tree-sitter in the repo. As much of the Tree-sitter API as possible will be implemented atop this representation.

A `pack_tree` function will be provided which packs a Tree-sitter tree to bytes. The squat parse functions simply call mainline parse and then pack.

# Rust API

The Rust API will export types and functions for this squat representation, along with traits that work with both the mainline and the squat representation. This way code can be written that is specialized to both representations.

# Tests and benchmarks

Tests and benchmarks are written in Rust.

* `zed-queries` benchmark FIXME

# Open questions

* How should "symbol present" bitmaps be stored?

* Should padding / spacing etc be used to make things align on cache lines etc?


# Future work

* Representation of which char in unexpected char nodes

* Evaluate whether storing `byte_len` is better than `end_byte`. Similar for row / col

* Revisit node count

* ABI compatible drop-in

* Make persistence work across BE vs LE?

* Per-symbol occurrence list for rare symbols to accelerate queries

  - Instead of having say per group or interval bitmap of symbol membership, what if there was a per-symbol bitmap where each bit covers an inerval? For rare symbols can instead have the occurrence list.
