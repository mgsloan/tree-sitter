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
  bool ok = sq_build_presence_cached(tree, &scratch, &capacity, error);
  free(scratch);
  return ok;
}

bool sq_build_presence_cached(SQTree *tree, uint8_t **scratch_pointer, size_t *capacity,
                              SQError *error) {
  uint32_t groups = sq_tree_group_count(tree);
  if (groups <= 32) {
    return true;
  }

  uint64_t length = sq_presence_size(tree);
  uint32_t symbols = sq_symbols(tree), entry_bytes = (groups + 31) / 32 * 4;
  uint32_t entry_slots = entry_bytes / sizeof(uint32_t);

  // Scratch holds per-symbol counts and one bitmap used while promoting an entry.
  uint64_t scratch_size = (uint64_t)symbols * sizeof(uint32_t) + entry_bytes;
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
  uint8_t *bitmap = scratch + (size_t)symbols * sizeof(uint32_t);

  uint32_t offset = tree->layout.end;
  if ((uint64_t)offset + length > tree->size) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }

  uint8_t *next = tree->data;
  memset(next + offset, 0, (size_t)length);
  sq_header_set(tree, format_flags, sq_header_get(tree, format_flags) | SQ_PRESENCE);

  uint8_t *entries = next + offset + sq_column_size(symbols, 1);
  memset(entries, 0xff, (size_t)symbols * entry_bytes);

  // Every packed column, including 8- and 16-bit widths, stores lane i of a word
  // at shift i * bits under a native load (see sq_get_packed), so decode whole
  // words and walk lanes downward instead of dividing per slot.
  const uint8_t *symbol_column = next + tree->layout.symbol;
  uint8_t bits = 16;
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
    uint64_t word = sq_get_u64(symbol_column, 0, word_index);
    for (;;) {
      uint32_t symbol_index = ((word >> (lane * bits)) & value_mask) >> tree->layout.symbol_shift;
      uint8_t *entry = entries + (size_t)symbol_index * entry_bytes;
      uint32_t count = counts[symbol_index];
      if (count > entry_slots) {
        set_group(entry, group);
      } else if (count < entry_slots) {
        sq_set_u32(entry, 0, count, slot);
        counts[symbol_index] = count + 1;
      } else {
        memset(bitmap, 0, entry_bytes);
        for (uint32_t i = 0; i < entry_slots; i++) {
          uint32_t previous = sq_get_u32(entry, 0, i);
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
        word = sq_get_u64(symbol_column, 0, word_index);
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
    uint32_t slot = sq_get_u32(entry, 0, i);
    if (slot == SQ_NONE || slot / SQ_GROUP_SIZE < group) {
      break;
    }

    if (slot / SQ_GROUP_SIZE == group) {
      return true;
    }
  }

  return false;
}

// Validate before exposing any nodes. Layout offsets must be canonical, so no
// column read can escape the buffer even when the input is hostile.
static bool validate_nodes(SQTree *tree, SQError *error) {
  uint32_t depth = 0;
  uint32_t local_ends[64];
  size_t capacity = sizeof(local_ends) / sizeof(local_ends[0]);
  uint32_t *ends = local_ends;

  uint32_t groups = sq_tree_group_count(tree), symbols = sq_symbols(tree);
  uint32_t root_slot = groups * SQ_GROUP_SIZE - sq_group_waste(tree, groups - 1) - 1;
  uint32_t dictionary_count = sq_header_get(tree, supertype_dictionary_count);
  bool points = sq_tree_has_points(tree);
  for (uint32_t group = groups; group-- > 0;) {
    uint32_t first = group * SQ_GROUP_SIZE;
    uint32_t group_end = first + SQ_GROUP_SIZE - sq_group_waste(tree, group);
    uint32_t span_base = sq_group_span_base(tree, group);
    uint32_t start_byte_base = sq_group_start_byte_base(tree, group);
    uint32_t end_byte_base = sq_group_end_byte_base(tree, group);
    TSPoint start_base = {0}, end_base = {0};
    if (points) {
      start_base = sq_point_from_key(sq_group_start_point_base(tree, group));
      end_base = sq_point_from_key(sq_group_end_point_base(tree, group));
    }
    for (uint32_t slot = group_end; slot-- > first;) {
      SQNode node = {tree, slot};
      while (depth && ends[depth - 1] > slot) depth--;

      uint64_t span = (uint64_t)span_base + sq_node_span_delta(node);
      if (span > slot) goto invalid;
      uint32_t end = slot - (uint32_t)span;
      if (end) {
        uint32_t end_slot = end - 1;
        uint32_t end_group = end_slot / SQ_GROUP_SIZE;
        uint32_t occupied_end =
            (end_group + 1) * SQ_GROUP_SIZE - sq_group_waste(tree, end_group);
        if (end_slot >= occupied_end) goto invalid;
      }
      bool last = sq_node_last_flag(node);
      uint32_t field = sq_node_field_value(node);
      if (slot == root_slot) {
        if (end || !last || field) goto invalid;
      } else if (!depth || end < ends[depth - 1] || last != (end == ends[depth - 1])) {
        goto invalid;
      }

      uint16_t code = sq_node_symbol_code(node);
      uint32_t symbol = code >> tree->layout.symbol_shift;
      const SQSymbolTable *table = &tree->grammar->symbols;
      if (symbol >= symbols || field > tree->language->field_count ||
          (symbol < symbols - 2 && tree->language->public_symbol_map[symbol] != symbol)) {
        goto invalid;
      }
      if (table->separate || table->encoding == SQ_SYMBOL_BYTES) {
        if (sq_node_grammar_id(node) >= symbols) goto invalid;
      } else {
        uint32_t variant = code & ((1u << table->shift) - 1);
        if (table->encoding == SQ_SYMBOL_GLOBAL) {
          if (!table->counts[symbol] || variant >= table->length ||
              (!variant && table->counts[symbol] != 1)) goto invalid;
        } else if (variant >= table->counts[symbol]) {
          goto invalid;
        }
      }

      uint32_t super = sq_node_supertype(node);
      if (tree->supertype_count > 8 ? super >= dictionary_count
                                    : super >= (1u << tree->supertype_count)) {
        goto invalid;
      }

      uint32_t start_byte_delta = sq_node_start_byte_delta(node);
      uint32_t end_byte_delta = sq_node_end_byte_delta(node);
      if ((uint64_t)start_byte_base + start_byte_delta > UINT32_MAX ||
          end_byte_base < end_byte_delta ||
          start_byte_base + start_byte_delta > end_byte_base - end_byte_delta) {
        goto invalid;
      }
      if (points) {
        uint32_t start_delta = sq_node_start_point_key(node);
        uint32_t end_delta = sq_node_end_point_key(node);
        if ((uint64_t)start_base.row + (start_delta >> 8) > UINT32_MAX ||
            (uint64_t)start_base.column + (start_delta & UINT8_MAX) > UINT32_MAX ||
            end_base.row < (end_delta >> 8) || end_base.column < (end_delta & UINT8_MAX)) {
          goto invalid;
        }
        TSPoint start = {.row = start_base.row + (start_delta >> 8),
                         .column = start_base.column + (start_delta & UINT8_MAX)};
        TSPoint finish = {.row = end_base.row - (end_delta >> 8),
                          .column = end_base.column - (end_delta & UINT8_MAX)};
        if (start.row > finish.row || (start.row == finish.row && start.column > finish.column)) {
          goto invalid;
        }
      }

      if (depth == capacity) {
        if (capacity > SIZE_MAX / 2 / sizeof(uint32_t)) {
          sq_fail(error, SQ_ERROR_OVERFLOW);
          if (ends != local_ends) free(ends);
          return false;
        }

        uint32_t *next;
        if (ends == local_ends) {
          next = malloc(capacity * 2 * sizeof(uint32_t));
          if (next) memcpy(next, local_ends, capacity * sizeof(uint32_t));
        } else {
          next = realloc(ends, capacity * 2 * sizeof(uint32_t));
        }
        if (!next) {
          sq_fail(error, SQ_ERROR_ALLOCATION);
          if (ends != local_ends) free(ends);
          return false;
        }

        ends = next;
        capacity *= 2;
      }

      ends[depth++] = end;
    }
  }

  if (ends != local_ends) free(ends);
  return true;
invalid:
  if (ends != local_ends) free(ends);
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
  SymbolCount local_counts[256] = {0};
  SymbolCount *counts = symbols <= sizeof(local_counts) / sizeof(local_counts[0])
                            ? local_counts
                            : calloc(symbols, sizeof(SymbolCount));
  if (!counts) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }

  uint32_t groups = sq_tree_group_count(tree);
  for (uint32_t group = groups; group-- > 0;) {
    uint32_t first = group * SQ_GROUP_SIZE;
    uint32_t group_end = first + SQ_GROUP_SIZE - sq_group_waste(tree, group);
    for (uint32_t slot = group_end; slot-- > first;) {
      SQNode node = {tree, slot};
      uint32_t symbol = sq_encode_symbol(tree, sq_node_symbol(node));
      SymbolCount *count = &counts[symbol];
      const uint8_t *entry = entries + (size_t)symbol * entry_bytes;
      if (sq_get_packed(modes, 0, symbol, 1)) {
        if (!((entry[group / 8] >> (group % 8)) & 1)) goto invalid;
        if (!count->occurrences || count->last_group != group) {
          count->groups++;
          count->last_group = group;
        }
      } else {
        if (count->occurrences >= entry_slots) goto invalid;
        uint32_t stored_slot = sq_get_u32(entry, 0, count->occurrences);
        if (stored_slot != slot) goto invalid;
      }

      count->occurrences++;
    }
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
        uint32_t slot = sq_get_u32(entry, 0, index);
        if (slot != SQ_NONE) goto invalid;
      }
    }
  }

  if (symbols % 64) {
    uint64_t last_word = sq_get_u64(modes + mode_bytes - 8, 0, 0);
    if (last_word >> (symbols % 64)) goto invalid;
  }

  for (uint64_t offset = mode_bytes + (uint64_t)symbols * entry_bytes;
       offset < sq_presence_size(tree); offset++) {
    if (modes[offset]) goto invalid;
  }

  if (counts != local_counts) free(counts);
  return true;
invalid:
  if (counts != local_counts) free(counts);
  sq_fail(error, SQ_ERROR_INVALID_SLAB);
  return false;
}

static SQTree *load_bytes(SQGrammar *grammar, const void *bytes, size_t length,
                          bool borrowed, bool check_auxiliary_contents,
                          SQError *error) {
  sq_fail(error, SQ_OK);
  const TSLanguage *language = sq_grammar_language(grammar);
  SQHeader header;
  if (!bytes || length < sizeof(header) || length > UINT32_MAX) {
    goto invalid;
  }

  // Copied input may be unaligned; borrowed slabs retain canonical alignment.
  if (borrowed && (uintptr_t)bytes % SQ_COLUMN_ALIGNMENT) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }

  header = sq_read_header(bytes);
  if (((header.format_flags & SQ_MISSING) && !(header.format_flags & SQ_ERRORS)) ||
      !header.group_count || header.group_count > header.group_capacity ||
      (header.format_flags &
       ~(SQ_NO_POINTS | SQ_PRESENCE | SQ_WIDE_SUPERTYPES | SQ_SEPARATE_GRAMMAR |
         SQ_OPTIONAL_FLAGS)) != SQ_VERSION) {
    goto invalid;
  }
  SQLayout layout;
  if (!language || !sq_layout(grammar, header.group_capacity,
                              (header.format_flags & SQ_WIDE_SUPERTYPES) != 0,
                              !(header.format_flags & SQ_NO_POINTS), header.format_flags, &layout) ||
      layout.end > length) {
    goto invalid;
  }

  for (uint32_t group = 0; group < header.group_count; group++) {
    if (sq_get_packed(bytes, layout.waste, group, SQ_WASTE_BITS) >= SQ_GROUP_SIZE) goto invalid;
  }

  // Validate section sizes before allocating or accessing their contents.
  // A temporary descriptor is sufficient to derive the optional index length.
  SQTree shape = {.language = language, .data = (uint8_t *)bytes};
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

  SQTree *tree =
      sq_allocate_loaded(grammar, header.group_capacity, bytes, (uint32_t)length, borrowed,
                         !(header.format_flags & SQ_NO_POINTS), error);
  if (!tree) return NULL;
  uint32_t dictionary_count = tree->supertype_grammar ? tree->supertype_grammar->count : 0;
  if (header.supertype_dictionary_count != dictionary_count ||
      !(header.format_flags & SQ_WIDE_SUPERTYPES)) {
    sq_tree_delete(tree);
    goto invalid;
  }

  if (!!(header.format_flags & SQ_SEPARATE_GRAMMAR) != grammar->symbols.separate) {
    sq_tree_delete(tree);
    goto invalid;
  }

  if (expected != length) {
    sq_tree_delete(tree);
    goto invalid;
  }

  if (!validate_nodes(tree, error)) {
    sq_tree_delete(tree);
    return NULL;
  }

  // Presence readers only inspect a size-checked bitmap or bounded sparse list.
  // Sparse values are compared with group numbers, never dereferenced as slots.
  // Reconstructing membership and checking padding is a semantic integrity check.
  if (check_auxiliary_contents && (header.format_flags & SQ_PRESENCE) &&
      !validate_presence(tree, error)) {
    sq_tree_delete(tree);
    return NULL;
  }

  return tree;
invalid:
  sq_fail(error, SQ_ERROR_INVALID_SLAB);
  return NULL;
}

SQTree *sq_tree_from_bytes(SQGrammar *grammar, const void *bytes, size_t length,
                           SQError *error) {
  return load_bytes(grammar, bytes, length, false, true, error);
}

SQTree *sq_tree_from_bytes_safety_checked(SQGrammar *grammar, const void *bytes,
                                         size_t length, SQError *error) {
  return load_bytes(grammar, bytes, length, false, false, error);
}

SQTree *sq_tree_from_bytes_borrowed(SQGrammar *grammar, const void *bytes, size_t length,
                                    SQError *error) {
  return load_bytes(grammar, bytes, length, true, true, error);
}

SQTree *sq_tree_from_bytes_borrowed_safety_checked(SQGrammar *grammar, const void *bytes,
                                                  size_t length, SQError *error) {
  return load_bytes(grammar, bytes, length, true, false, error);
}

SQTree *sq_tree_repack(const SQTree *tree, SQError *error) {
  if (!tree) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }

  SQTree *copy = sq_tree_from_bytes(tree->grammar, tree->data, tree->size, error);
  if (copy && !sq_resize(&copy, sq_tree_group_count(copy), error)) {
    sq_tree_delete(copy);
    return NULL;
  }

  return copy;
}
