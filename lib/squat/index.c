#include "internal.h"

uint64_t sq_presence_size(const SQTree *tree) {
  uint64_t symbols = sq_symbols(tree);
  uint64_t entry_bytes = ((uint64_t)sq_tree_group_count(tree) + 31) / 32 * 4;
  return (sq_column_size((uint32_t)symbols, 1) + symbols * entry_bytes + 7) & ~UINT64_C(7);
}

static void set_group(uint8_t *entry, uint32_t group) {
  entry[group / 8] |= (uint8_t)(1u << (group % 8));
}

bool sq_build_presence(SQTree *tree, SQError *error) {
  uint8_t *scratch = NULL;
  size_t capacity = 0;
  bool ok = sq_build_presence_cached(tree, NULL, &scratch, &capacity, error);
  free(scratch);
  return ok;
}

bool sq_build_presence_cached(SQTree *tree, const uint16_t *cached_index,
                               uint8_t **scratch_pointer, size_t *capacity, SQError *error) {
  uint32_t groups = sq_tree_group_count(tree);
  if (groups <= 32) {
    return true;
  }

  uint64_t length = sq_presence_size(tree);
  uint32_t symbols = sq_symbols(tree), entry_bytes = (groups + 31) / 32 * 4;
  uint32_t entry_slots = entry_bytes / sizeof(uint32_t);

  // Scratch holds per-symbol occurrence counts, the raw-to-public index table,
  // and one bitmap used while promoting an entry.
  uint64_t scratch_size =
      (uint64_t)symbols * (sizeof(uint32_t) + sizeof(uint16_t)) + entry_bytes;
  if (scratch_size > SIZE_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }
  if (scratch_size > *capacity) {
    uint8_t *next = realloc(*scratch_pointer, (size_t)scratch_size);
    if (!next) {
      sq_fail(error, SQ_ERROR_ALLOCATION);
      return false;
    }
    *scratch_pointer = next;
    *capacity = (size_t)scratch_size;
  }
  uint8_t *scratch = *scratch_pointer;
  memset(scratch, 0, (size_t)symbols * sizeof(uint32_t));
  uint32_t *counts = (uint32_t *)scratch;
  uint16_t *public_index = (uint16_t *)(scratch + (size_t)symbols * sizeof(uint32_t));
  uint8_t *bitmap = scratch + (size_t)symbols * (sizeof(uint32_t) + sizeof(uint16_t));

  uint32_t offset = tree->layout.end;
  if ((uint64_t)offset + length > tree->size) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }

  // Map each stored symbol to its public entry once, rather than calling the
  // runtime accessor per slot. Encoded indexes are below sq_symbols, at most
  // 65536, so they fit. The packer never stores ERROR_REPEAT, which is hidden and
  // never an alias, and the public map has no entry for it.
  for (uint32_t raw = 0; !cached_index && raw < symbols; raw++) {
    TSSymbol symbol = sq_decode_symbol(tree, raw);
    public_index[raw] =
        symbol == ts_builtin_sym_error_repeat
            ? (uint16_t)raw
            : (uint16_t)sq_encode_symbol(tree, ts_language_public_symbol(tree->language, symbol));
  }
  const uint16_t *indexes = cached_index ? cached_index : public_index;

  uint8_t *next = tree->data;
  memset(next + offset, 0, (size_t)length);
  sq_header(tree)->format_flags |= SQ_PRESENCE;

  uint8_t *entries = next + offset + sq_column_size(symbols, 1);
  memset(entries, 0xff, (size_t)symbols * entry_bytes);

  // Every packed column, including 8- and 16-bit widths, stores lane i of a word
  // at shift i * bits under a native load (see sq_get_packed), so decode whole
  // words and walk lanes downward instead of dividing per slot.
  const uint8_t *symbol_column = next + tree->layout.symbol;
  uint8_t bits = tree->layout.symbol_bits;
  uint32_t lanes = 64 / bits;
  uint64_t value_mask = (UINT64_C(1) << bits) - 1;

  // Physical slots descend in public preorder. Scan groups and their live
  // lanes in that order so sparse entries retain their serialized ordering.
  // An entry promotes exactly when its next occurrence no longer fits; a count
  // above entry_slots marks a promoted entry, mirroring its mode bit.
  for (uint32_t group = groups; group-- > 0;) {
    uint32_t first = group * SQ_GROUP_SIZE;
    uint32_t end = first + SQ_GROUP_SIZE - sq_group_waste(tree, group);
    if (end == first) continue;

    uint32_t slot = end - 1;
    uint32_t word_index = slot / lanes, lane = slot % lanes;
    uint64_t word;
    memcpy(&word, symbol_column + (uint64_t)word_index * 8, sizeof(word));
    for (;;) {
      uint32_t symbol_index = indexes[(word >> (lane * bits)) & value_mask];
      uint8_t *entry = entries + (size_t)symbol_index * entry_bytes;
      uint32_t count = counts[symbol_index];
      if (count > entry_slots) {
        set_group(entry, group);
      } else if (count < entry_slots) {
        memcpy(entry + (size_t)count * sizeof(slot), &slot, sizeof(slot));
        counts[symbol_index] = count + 1;
      } else {
        memset(bitmap, 0, entry_bytes);
        for (uint32_t i = 0; i < entry_slots; i++) {
          uint32_t previous;
          memcpy(&previous, entry + (size_t)i * sizeof(previous), sizeof(previous));
          set_group(bitmap, previous / SQ_GROUP_SIZE);
        }
        set_group(bitmap, group);
        memcpy(entry, bitmap, entry_bytes);
        sq_set_packed(next, offset, symbol_index, 1, 1);
        counts[symbol_index] = entry_slots + 1;
      }

      if (slot == first) break;
      slot--;
      if (lane-- == 0) {
        lane = lanes - 1;
        word_index--;
        memcpy(&word, symbol_column + (uint64_t)word_index * 8, sizeof(word));
      }
    }
  }

  return true;
}

bool sq_tree_group_has_symbol(const SQTree *tree, uint32_t group, TSSymbol symbol) {
  if (!tree || group >= sq_tree_group_count(tree)) {
    return false;
  }

  uint32_t symbol_index = sq_encode_symbol(tree, symbol);
  if (symbol_index >= sq_symbols(tree)) {
    return false;
  }

  uint32_t offset = sq_presence_offset(tree);
  if (!offset) {
    uint32_t end = (group + 1) * SQ_GROUP_SIZE - sq_group_waste(tree, group);
    for (uint32_t i = group * SQ_GROUP_SIZE; i < end; i++) {
      if (sq_node_symbol((SQNode){tree, i}) == symbol) {
        return true;
      }
    }

    return false;
  }

  uint32_t entry_bytes = (sq_tree_group_count(tree) + 31) / 32 * 4;
  const uint8_t *entry = tree->data + offset + sq_column_size(sq_symbols(tree), 1) +
                         (size_t)symbol_index * entry_bytes;
  if (sq_get_packed(tree->data, offset, symbol_index, 1)) {
    return (entry[group / 8] >> (group % 8)) & 1;
  }

  for (uint32_t i = 0; i < entry_bytes / 4; i++) {
    uint32_t slot;
    memcpy(&slot, entry + (size_t)i * 4, 4);
    if (slot == SQ_NONE || slot / SQ_GROUP_SIZE < group) {
      break;
    }

    if (slot / SQ_GROUP_SIZE == group) {
      return true;
    }
  }

  return false;
}

bool sq_append_dictionary(SQTree *tree, const uint64_t *dictionary, uint32_t count,
                          SQError *error) {
  uint64_t bytes = (uint64_t)count * ((tree->supertype_count + 63) / 64) * 8;
  uint64_t offset = tree->layout.end;
  if (sq_presence_offset(tree)) offset += sq_presence_size(tree);
  if (offset + bytes > tree->size) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }

  sq_header(tree)->supertype_dictionary_count = count;
  memcpy(tree->data + (uint32_t)offset, dictionary, (size_t)bytes);
  return true;
}

// Validate before exposing any nodes. Layout offsets must be canonical, so no
// column read can escape the buffer even when the input is hostile.
static bool validate_nodes(SQTree *tree, SQError *error) {
  uint32_t depth = 0;
  size_t capacity = 32;
  uint32_t *ends = malloc(capacity * sizeof(uint32_t));
  if (!ends) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }

  SQNode root = sq_tree_root_node(tree);
  for (SQNode node = root; node.tree; node = sq_node_next_preorder(node)) {
    while (depth && ends[depth - 1] > node.slot) {
      depth--;
    }

    uint64_t span =
        (uint64_t)sq_group_span_base(tree, node.slot / SQ_GROUP_SIZE) + sq_node_span_delta(node);
    if (span > node.slot) goto invalid;
    uint32_t end = node.slot - (uint32_t)span;
    if (end && sq_tree_node_at_slot(tree, end - 1).tree == NULL) goto invalid;
    if (node.slot == root.slot) {
      if (end || !sq_node_last_flag(node) || sq_node_field_id(node)) goto invalid;
    } else if (!depth || end < ends[depth - 1] ||
               sq_node_last_flag(node) != (end == ends[depth - 1])) {
      goto invalid;
    }

    if (sq_node_symbol_id(node) >= sq_symbols(tree) || sq_node_grammar_id(node) >= sq_symbols(tree))
      goto invalid;
    if (sq_node_field_value(node) > tree->language->field_count) {
      goto invalid;
    }

    uint32_t super = sq_node_supertype(node);
    if (tree->supertype_count > 8 ? super >= sq_header(tree)->supertype_dictionary_count
                                  : super >= (1u << tree->supertype_count)) {
      goto invalid;
    }

    uint32_t group = node.slot / SQ_GROUP_SIZE;
    if ((uint64_t)sq_group_start_byte_base(tree, group) + sq_node_start_byte_delta(node) >
            UINT32_MAX ||
        sq_group_end_byte_base(tree, group) < sq_node_end_byte_delta(node))
      goto invalid;
#if SQ_INCLUDE_POINTS
    TSPoint start_base = sq_point_from_key(sq_group_start_point_base(tree, group));
    TSPoint end_base = sq_point_from_key(sq_group_end_point_base(tree, group));
    uint32_t start_delta = sq_node_start_point_key(node);
    uint32_t end_delta = sq_node_end_point_key(node);
    if ((uint64_t)start_base.row + (start_delta >> 8) > UINT32_MAX ||
        (uint64_t)start_base.column + (start_delta & UINT8_MAX) > UINT32_MAX ||
        end_base.row < (end_delta >> 8) || end_base.column < (end_delta & UINT8_MAX)) {
      goto invalid;
    }
#endif
#if SQ_INCLUDE_POINTS
    TSPoint start = sq_node_start_point(node), finish = sq_node_end_point(node);
    if (start.row > finish.row || (start.row == finish.row && start.column > finish.column)) {
      goto invalid;
    }
#endif
    if (sq_node_start_byte(node) > sq_node_end_byte(node)) {
      goto invalid;
    }

    if (depth == capacity) {
      if (capacity > SIZE_MAX / 2 / sizeof(uint32_t)) {
        sq_fail(error, SQ_ERROR_OVERFLOW);
        free(ends);
        return false;
      }

      uint32_t *next = realloc(ends, capacity * 2 * sizeof(uint32_t));
      if (!next) {
        sq_fail(error, SQ_ERROR_ALLOCATION);
        free(ends);
        return false;
      }

      ends = next;
      capacity *= 2;
    }

    ends[depth++] = (uint32_t)end;
  }

  free(ends);
  return true;
invalid:
  free(ends);
  sq_fail(error, SQ_ERROR_INVALID_SLAB);
  return false;
}

// Verify the index without copying the columns or rebuilding a second slab.
// Preorder visits groups monotonically, so the last group seen for each symbol
// suffices to count its distinct groups without a tree-sized bitmap.
static bool validate_presence(const SQTree *tree, SQError *error) {
  typedef struct {
    uint32_t occurrences, groups, last_group;
  } SymbolCount;
  uint32_t symbols = sq_symbols(tree);
  uint32_t entry_bytes = (sq_tree_group_count(tree) + 31) / 32 * 4;
  uint32_t entry_slots = entry_bytes / 4;
  uint32_t mode_bytes = (uint32_t)sq_column_size(symbols, 1);
  const uint8_t *modes = tree->data + sq_presence_offset(tree);
  const uint8_t *entries = modes + mode_bytes;
  SymbolCount *counts = calloc(symbols, sizeof(SymbolCount));
  if (!counts) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }

  for (SQNode node = sq_tree_root_node(tree); node.tree; node = sq_node_next_preorder(node)) {
    uint32_t symbol = sq_encode_symbol(tree, sq_node_symbol(node));
    SymbolCount *count = &counts[symbol];
    const uint8_t *entry = entries + (size_t)symbol * entry_bytes;
    if (sq_get_packed(modes, 0, symbol, 1)) {
      uint32_t group = node.slot / SQ_GROUP_SIZE;
      if (!((entry[group / 8] >> (group % 8)) & 1)) goto invalid;
      if (!count->occurrences || count->last_group != group) {
        count->groups++;
        count->last_group = group;
      }
    } else {
      if (count->occurrences >= entry_slots) goto invalid;
      uint32_t slot;
      memcpy(&slot, entry + (size_t)count->occurrences * 4, 4);
      if (slot != node.slot) goto invalid;
    }

    count->occurrences++;
  }

  for (uint32_t symbol = 0; symbol < symbols; symbol++) {
    const SymbolCount *count = &counts[symbol];
    const uint8_t *entry = entries + (size_t)symbol * entry_bytes;
    bool bitmap = sq_get_packed(modes, 0, symbol, 1);
    if (bitmap != (count->occurrences > entry_slots)) goto invalid;
    if (bitmap) {
      // Every real group bit was checked above. Equal cardinality now rules
      // out extra bits too, including bits beyond the final live group.
      uint32_t set_bits = 0;
      for (uint32_t offset = 0; offset < entry_bytes; offset += 4) {
        uint32_t word;
        memcpy(&word, entry + offset, 4);
        set_bits += (uint32_t)__builtin_popcount(word);
      }

      if (set_bits != count->groups) goto invalid;
    } else {
      for (uint32_t index = count->occurrences; index < entry_slots; index++) {
        uint32_t slot;
        memcpy(&slot, entry + (size_t)index * 4, 4);
        if (slot != SQ_NONE) goto invalid;
      }
    }
  }

  if (symbols % 64) {
    uint64_t last_word;
    memcpy(&last_word, modes + mode_bytes - 8, 8);
    if (last_word >> (symbols % 64)) goto invalid;
  }

  for (uint64_t offset = mode_bytes + (uint64_t)symbols * entry_bytes;
       offset < sq_presence_size(tree); offset++) {
    if (modes[offset]) goto invalid;
  }

  free(counts);
  return true;
invalid:
  free(counts);
  sq_fail(error, SQ_ERROR_INVALID_SLAB);
  return false;
}

// Called only after the complete sparse section has been bounds-checked.
static bool validate_grammar(const SQTree *tree) {
  uint32_t offset = sq_grammar_offset(tree), words = sq_grammar_words(tree);
  uint32_t count = sq_get_u32(tree->data, offset, 0);
  uint32_t bitmap = offset + 8, ranks = bitmap + words * 8;
  uint32_t values = ranks + (uint32_t)sq_array_size(words, 4), rank = 0;
  if (!count || sq_get_u32(tree->data, offset, 1)) return false;
  for (uint32_t i = 0; i < words; i++) {
    if (sq_get_u32(tree->data, ranks, i) != rank) return false;
    uint64_t word = sq_get_u64(tree->data, bitmap, i);
    while (word) {
      uint32_t slot = i * 64 + (uint32_t)__builtin_ctzll(word);
      SQNode node = sq_tree_node_at_slot(tree, slot);
      if (!node.tree || rank >= count) return false;
      uint32_t grammar = sq_get_packed(tree->data, values, rank++, tree->layout.symbol_bits);
      if (grammar >= sq_symbols(tree) || grammar == sq_node_symbol_id(node)) return false;
      word &= word - 1;
    }
  }
  if (rank != count) return false;
  if (words % 2 && sq_get_u32(tree->data, ranks, words)) return false;
  // Reject unused lanes and high padding bits in every packed value word.
  uint32_t lanes = tree->layout.symbol_lanes;
  for (uint32_t i = 0; i < (count + (uint64_t)lanes - 1) / lanes; i++) {
    uint32_t remaining = count - i * lanes;
    unsigned bits = (remaining < lanes ? remaining : lanes) * tree->layout.symbol_bits;
    if (bits < 64 && sq_get_u64(tree->data, values, i) >> bits) return false;
  }
  return true;
}

static SQTree *load_bytes(const TSLanguage *language, const void *bytes, size_t length,
                          bool borrowed, SQError *error) {
  sq_fail(error, SQ_OK);
  SQHeader header;
  if (!bytes || length < sizeof(header) || length > UINT32_MAX) {
    goto invalid;
  }

  // Copied input may be unaligned; borrowed slabs retain canonical alignment.
  if (borrowed && (uintptr_t)bytes % SQ_COLUMN_ALIGNMENT) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }

  memcpy(&header, bytes, sizeof(header));
  if (!header.group_count || header.group_count > header.group_capacity ||
      (header.format_flags & ~(SQ_PRESENCE | SQ_GRAMMAR_OVERRIDES)) != (SQ_VERSION | SQ_LAYOUT_FLAGS)) {
    goto invalid;
  }

  SQLayout layout;
  if (!language || !sq_layout(language, header.group_capacity, &layout) || layout.end > length) {
    goto invalid;
  }

  // Validate section sizes before allocating or accessing their contents.
  // A temporary descriptor is sufficient to derive the optional index length.
  SQTree shape = {.language = language, .data = (uint8_t *)&header};
  uint64_t expected = layout.end;
  if (header.format_flags & SQ_PRESENCE) {
    if (header.group_count <= 32) goto invalid;
    expected += sq_presence_size(&shape);
  }

  if (language->abi_version < TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION ||
      language->abi_version > TREE_SITTER_LANGUAGE_VERSION ||
      (uint64_t)language->symbol_count + language->alias_count > ts_builtin_sym_error_repeat) {
    sq_fail(error, SQ_ERROR_LANGUAGE);
    return NULL;
  }

  // The remaining size check needs the grammar's supertype count, which
  // allocate_tree derives anyway. Allocate first and read it back rather than
  // repeating that grammar-sized scan; the allocation covers the caller's length,
  // which is already bounded, and every later rejection releases the tree.
  SQTree *tree =
      sq_allocate_loaded(language, header.group_capacity, bytes, (uint32_t)length, borrowed, error);
  if (!tree) return NULL;
  if (tree->supertype_count > 8) {
    if (!header.supertype_dictionary_count || header.supertype_dictionary_count > 256) {
      sq_tree_delete(tree);
      goto invalid;
    }

    expected +=
        (uint64_t)header.supertype_dictionary_count * ((tree->supertype_count + 63) / 64) * 8;
  } else if (header.supertype_dictionary_count) {
    sq_tree_delete(tree);
    goto invalid;
  }

  if (header.format_flags & SQ_GRAMMAR_OVERRIDES) {
    if (expected + 8 > length) {
      sq_tree_delete(tree);
      goto invalid;
    }
    uint32_t count = sq_get_u32(tree->data, (uint32_t)expected, 0);
    expected += sq_grammar_size(tree, count);
  }

  if (expected != length) {
    sq_tree_delete(tree);
    goto invalid;
  }

  const uint8_t *data = tree->data;
  if ((header.format_flags & SQ_GRAMMAR_OVERRIDES) && !validate_grammar(tree)) {
    sq_tree_delete(tree);
    goto invalid;
  }
  if (!validate_nodes(tree, error)) {
    sq_tree_delete(tree);
    return NULL;
  }

  if (tree->supertype_count > 8 && tree->supertype_count % 64) {
    uint32_t words = (tree->supertype_count + 63) / 64;
    for (uint32_t i = 0; i < header.supertype_dictionary_count; i++) {
      uint64_t word;
      memcpy(&word, data + sq_dictionary_offset(tree) + ((size_t)(i + 1) * words - 1) * 8, 8);
      if (word >> (tree->supertype_count % 64)) {
        sq_tree_delete(tree);
        goto invalid;
      }
    }
  }

  if ((header.format_flags & SQ_PRESENCE) && !validate_presence(tree, error)) {
    sq_tree_delete(tree);
    return NULL;
  }

  return tree;
invalid:
  sq_fail(error, SQ_ERROR_INVALID_SLAB);
  return NULL;
}

SQTree *sq_tree_from_bytes(const TSLanguage *language, const void *bytes, size_t length,
                           SQError *error) {
  return load_bytes(language, bytes, length, false, error);
}

SQTree *sq_tree_from_bytes_borrowed(const TSLanguage *language, const void *bytes, size_t length,
                                    SQError *error) {
  return load_bytes(language, bytes, length, true, error);
}

SQTree *sq_tree_repack(const SQTree *tree, SQError *error) {
  if (!tree) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }

  SQTree *copy = sq_tree_from_bytes(tree->language, tree->data, tree->size, error);
  if (copy && !sq_resize(&copy, sq_tree_group_count(copy), error)) {
    sq_tree_delete(copy);
    return NULL;
  }

  return copy;
}
