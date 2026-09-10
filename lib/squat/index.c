#include "internal.h"

uint64_t sq_presence_size(const SQTree *tree) {
  uint64_t symbols = sq_symbols(tree);
  uint64_t entry_bytes = ((uint64_t)sq_tree_group_count(tree) + 31) / 32 * 4;
  return (sq_column_size((uint32_t)symbols, 1) + symbols * entry_bytes + 7) & ~UINT64_C(7);
}
bool sq_build_presence(SQTree **tree_pointer, SQError *error) {
  SQTree *tree = *tree_pointer;
  uint32_t groups = sq_tree_group_count(tree);
  if (groups <= 32) {
    return true;
  }
  uint64_t length = sq_presence_size(tree), total = (uint64_t)tree->size + length;
  if (total > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }
  uint32_t symbols = sq_symbols(tree), entry_bytes = (groups + 31) / 32 * 4;
  uint32_t *counts = calloc(symbols, sizeof(uint32_t));
  if (!counts) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }
  uint32_t offset = tree->size;
  if (!sq_grow_data(tree_pointer, (uint32_t)total, error)) {
    free(counts);
    return false;
  }
  tree = *tree_pointer;
  uint8_t *next = tree->data;
  memset(next + offset, 0, (size_t)length);
  sq_header(tree)->format_flags |= SQ_PRESENCE;
  for (SQNode node = sq_tree_root_node(tree); node.tree; node = sq_node_next_preorder(node)) {
    counts[sq_encode_symbol(tree, sq_node_symbol(node))]++;
  }
  uint8_t *entries = next + offset + sq_column_size(symbols, 1);
  for (uint32_t symbol_index = 0; symbol_index < symbols; symbol_index++) {
    if (counts[symbol_index] > entry_bytes / 4) {
      sq_set_packed(next, offset, symbol_index, 1, 1);
    } else {
      memset(entries + (size_t)symbol_index * entry_bytes, 0xff, entry_bytes);
    }
    counts[symbol_index] = 0;
  }
  for (SQNode node = sq_tree_root_node(tree); node.tree; node = sq_node_next_preorder(node)) {
    uint32_t symbol_index = sq_encode_symbol(tree, sq_node_symbol(node));
    uint8_t *entry = entries + (size_t)symbol_index * entry_bytes;
    if (sq_get_packed(next, offset, symbol_index, 1)) {
      uint32_t group = node.slot / SQ_GROUP_SIZE;
      entry[group / 8] |= (uint8_t)(1u << (group % 8));
    } else {
      memcpy(entry + (size_t)counts[symbol_index]++ * 4, &node.slot, 4);
    }
  }
  free(counts);
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
bool sq_append_dictionary(SQTree **tree_pointer, const uint64_t *dictionary, uint32_t count,
                          SQError *error) {
  SQTree *tree = *tree_pointer;
  uint64_t bytes = (uint64_t)count * ((tree->supertype_count + 63) / 64) * 8;
  if ((uint64_t)tree->size + bytes > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }
  uint32_t offset = tree->size;
  if (!sq_grow_data(tree_pointer, tree->size + (uint32_t)bytes, error)) {
    return false;
  }
  tree = *tree_pointer;
  sq_header(tree)->supertype_dictionary_count = count;
  memcpy(tree->data + offset, dictionary, (size_t)bytes);
  return true;
}

/* Validate before exposing any nodes. Layout offsets must be canonical, so no
 * column read can escape the buffer even when the input is hostile. */
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
    uint64_t span = (uint64_t)sq_group_span_base(tree, node.slot / SQ_GROUP_SIZE) +
                    sq_node_span_delta(node);
    if (span > node.slot) goto invalid;
    uint32_t end = node.slot - (uint32_t)span;
    if (end && sq_tree_node_at_slot(tree, end - 1).tree == NULL) goto invalid;
    if (node.slot == root.slot) {
      if (end || !sq_node_last_flag(node) || sq_node_field_id(node)) goto invalid;
    } else if (!depth || end < ends[depth - 1] ||
               sq_node_last_flag(node) != (end == ends[depth - 1])) {
      goto invalid;
    }
    if (sq_node_symbol_id(node) >= sq_symbols(tree) ||
        sq_node_grammar_id(node) >= sq_symbols(tree)) goto invalid;
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
        sq_group_end_byte_base(tree, group) < sq_node_end_byte_delta(node)) goto invalid;
#if SQ_INCLUDE_POINTS
    if ((uint64_t)sq_group_start_row_base(tree, group) + sq_node_start_row_delta(node) >
            UINT32_MAX ||
        sq_group_end_row_base(tree, group) < sq_node_end_row_delta(node)) goto invalid;
    if ((uint64_t)sq_group_start_column_base(tree, group) + sq_node_start_column_delta(node) >
            UINT32_MAX ||
        sq_group_end_column_base(tree, group) < sq_node_end_column_delta(node)) goto invalid;
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
/* Verify the index without copying the columns or rebuilding a second slab.
 * Preorder visits groups monotonically, so the last group seen for each symbol
 * suffices to count its distinct groups without a tree-sized bitmap. */
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
      (header.format_flags & ~SQ_PRESENCE) != (SQ_VERSION | SQ_LAYOUT_FLAGS)) {
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
  uint32_t supertype_count = 0;
  if (language->abi_version < TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION ||
      language->abi_version > TREE_SITTER_LANGUAGE_VERSION ||
      (uint64_t)language->symbol_count + language->alias_count > ts_builtin_sym_error_repeat) {
    sq_fail(error, SQ_ERROR_LANGUAGE);
    return NULL;
  }
  for (uint32_t symbol = 0; symbol < language->symbol_count + language->alias_count; symbol++) {
    supertype_count += ts_language_symbol_metadata(language, (TSSymbol)symbol).supertype;
  }
  if (supertype_count > 8) {
    if (!header.supertype_dictionary_count || header.supertype_dictionary_count > 256) {
      goto invalid;
    }
    expected += (uint64_t)header.supertype_dictionary_count * ((supertype_count + 63) / 64) * 8;
  } else if (header.supertype_dictionary_count) {
    goto invalid;
  }
  if (expected != length) goto invalid;

  SQTree *tree = sq_allocate_loaded(language, header.group_capacity, bytes, (uint32_t)length,
                                     borrowed, error);
  if (!tree) return NULL;
  const uint8_t *data = tree->data;
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
