#include "internal.h"

static uint64_t presence_size(const SQTree *tree) {
  uint64_t symbols = sq_symbols(tree);
  uint64_t entry_bytes = ((uint64_t)sq_tree_group_count(tree) + 31) / 32 * 4;
  return (sq_column_size((uint32_t)symbols, 1) + symbols * entry_bytes + 7) & ~UINT64_C(7);
}
bool sq_build_presence(SQTree *tree, SQError *error) {
  uint32_t groups = sq_tree_group_count(tree);
  if (groups <= 32) {
    return true;
  }
  uint64_t length = presence_size(tree), total = (uint64_t)tree->size + length;
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
  uint8_t *next = sq_reallocate_data(tree->data, tree->size, (size_t)total);
  if (!next) {
    free(counts);
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }
  tree->data = next;
  uint32_t offset = tree->size;
  memset(next + offset, 0, (size_t)length);
  sq_header(tree)->symbol_presence_byte_offset = offset;
  tree->size = (uint32_t)total;
  for (SQNode node = sq_tree_root_node(tree); node.tree; node = sq_node_next_preorder(node)) {
    counts[sq_encode_symbol(tree, sq_node_symbol(node))]++;
  }
  uint8_t *entries = next + offset + sq_column_size(symbols, 1);
  for (uint32_t symbol_index = 0; symbol_index < symbols; symbol_index++) {
    if (counts[symbol_index] > entry_bytes / 4) {
      sq_set(next, offset, symbol_index, 1, 1);
    } else {
      memset(entries + (size_t)symbol_index * entry_bytes, 0xff, entry_bytes);
    }
    counts[symbol_index] = 0;
  }
  for (SQNode node = sq_tree_root_node(tree); node.tree; node = sq_node_next_preorder(node)) {
    uint32_t symbol_index = sq_encode_symbol(tree, sq_node_symbol(node));
    uint8_t *entry = entries + (size_t)symbol_index * entry_bytes;
    if (sq_get(next, offset, symbol_index, 1)) {
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
  uint32_t offset = sq_header(tree)->symbol_presence_byte_offset;
  if (!offset) {
    uint32_t end = (group + 1) * SQ_GROUP_SIZE;
    for (uint32_t i = group * SQ_GROUP_SIZE + sq_group_get(tree, G_WASTE, group); i < end; i++) {
      if (sq_node_symbol((SQNode){tree, i}) == symbol) {
        return true;
      }
    }
    return false;
  }
  uint32_t entry_bytes = (sq_tree_group_count(tree) + 31) / 32 * 4;
  const uint8_t *entry = tree->data + offset + sq_column_size(sq_symbols(tree), 1) +
                         (size_t)symbol_index * entry_bytes;
  if (sq_get(tree->data, offset, symbol_index, 1)) {
    return (entry[group / 8] >> (group % 8)) & 1;
  }
  for (uint32_t i = 0; i < entry_bytes / 4; i++) {
    uint32_t slot;
    memcpy(&slot, entry + (size_t)i * 4, 4);
    if (slot == SQ_NONE || slot / SQ_GROUP_SIZE > group) {
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
  if ((uint64_t)tree->size + bytes > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }
  uint8_t *next = sq_reallocate_data(tree->data, tree->size, tree->size + (size_t)bytes);
  if (!next) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }
  tree->data = next;
  sq_header(tree)->supertype_dictionary_byte_offset = tree->size;
  sq_header(tree)->supertype_dictionary_count = count;
  memcpy(next + tree->size, dictionary, (size_t)bytes);
  tree->size += (uint32_t)bytes;
  return true;
}

bool sq_append_field_exceptions(SQTree *tree, const SQFieldException *entries, uint32_t count,
                                SQError *error) {
  if (!count) {
    return true;
  }
  uint64_t bytes = ((uint64_t)count * sizeof(SQFieldException) + 7) & ~UINT64_C(7);
  if ((uint64_t)tree->size + bytes > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }
  uint8_t *data = sq_reallocate_data(tree->data, tree->size, tree->size + (size_t)bytes);
  if (!data) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }
  tree->data = data;
  sq_header(tree)->field_exceptions_byte_offset = tree->size;
  sq_header(tree)->field_exceptions_count = count;
  memset(data + tree->size, 0, (size_t)bytes);
  memcpy(data + tree->size, entries, (size_t)count * sizeof(SQFieldException));
  tree->size += (uint32_t)bytes;
  return true;
}

bool sq_lookup_field_exception(SQNode node, TSFieldId field, SQNode *result) {
  SQHeader *header = sq_header(node.tree);
  uint32_t low = 0, high = header->field_exceptions_count;
  const uint8_t *entries = node.tree->data + header->field_exceptions_byte_offset;
  while (low < high) {
    uint32_t middle = low + (high - low) / 2;
    SQFieldException entry;
    memcpy(&entry, entries + (size_t)middle * sizeof(entry), sizeof(entry));
    if (entry.parent < node.slot || (entry.parent == node.slot && entry.field < field)) {
      low = middle + 1;
    } else if (entry.parent > node.slot || (entry.parent == node.slot && entry.field > field)) {
      high = middle;
    } else {
      *result = sq_tree_node_at_slot(node.tree, entry.target);
      return true;
    }
  }
  return false;
}

/* Validate before exposing any nodes. Layout offsets must be canonical, so no
 * column read can escape the buffer even when the input is hostile. */
static bool validate_nodes(SQTree *tree, SQError *error) {
  uint32_t slots = sq_tree_slot_count(tree), depth = 0;
  size_t capacity = 32;
  uint32_t *ends = malloc(capacity * sizeof(uint32_t));
  if (!ends) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }
  SQNode root = sq_tree_root_node(tree);
  for (SQNode node = root; node.tree; node = sq_node_next_preorder(node)) {
    while (depth && ends[depth - 1] == node.slot) {
      depth--;
    }
    uint64_t end = (uint64_t)node.slot + 1 + sq_group_get(tree, G_SPAN, node.slot / SQ_GROUP_SIZE) +
                   sq_node_get(node, N_SPAN);
    if (end > slots || (end < slots && sq_next_slot(tree, (uint32_t)end) != end)) {
      goto invalid;
    }
    if (node.slot == root.slot) {
      if (end != slots || !sq_node_get(node, N_LAST) || sq_node_field_id(node)) {
        goto invalid;
      }
    } else if (!depth || end > ends[depth - 1] ||
               sq_node_get(node, N_LAST) != (end == ends[depth - 1])) {
      goto invalid;
    }
    for (unsigned coordinate_index = N_SYMBOL; coordinate_index <= N_GRAMMAR; coordinate_index++) {
      if (sq_node_get(node, coordinate_index) >= sq_symbols(tree)) {
        goto invalid;
      }
    }
    if (sq_node_get(node, N_FIELD) > tree->language->field_count) {
      goto invalid;
    }
    uint32_t super = sq_node_get(node, N_SUPER);
    if (tree->supertype_count > 8 ? super >= sq_header(tree)->supertype_dictionary_count
                                  : super >= (1u << tree->supertype_count)) {
      goto invalid;
    }
    const unsigned starts_g[] = {G_BYTE, G_ROW, G_COL}, starts_n[] = {N_BYTE, N_ROW, N_COL};
    const unsigned ends_g[] = {G_END_BYTE, G_END_ROW, G_END_COL},
                   ends_n[] = {N_END_BYTE, N_END_ROW, N_END_COL};
    for (unsigned coordinate_index = 0; coordinate_index < 3; coordinate_index++) {
      if ((uint64_t)sq_group_get(tree, starts_g[coordinate_index], node.slot / SQ_GROUP_SIZE) +
              sq_node_get(node, starts_n[coordinate_index]) >
          UINT32_MAX) {
        goto invalid;
      }
      if (sq_group_get(tree, ends_g[coordinate_index], node.slot / SQ_GROUP_SIZE) <
          sq_node_get(node, ends_n[coordinate_index])) {
        goto invalid;
      }
    }
    TSPoint start = sq_node_start_point(node), finish = sq_node_end_point(node);
    if (sq_node_start_byte(node) > sq_node_end_byte(node) || start.row > finish.row ||
        (start.row == finish.row && start.column > finish.column)) {
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
SQTree *sq_tree_from_bytes(const TSLanguage *language, const void *bytes, size_t length,
                           SQError *error) {
  sq_fail(error, SQ_OK);
  SQHeader header;
  if (!bytes || length < sizeof(header) || length > UINT32_MAX) {
    goto invalid;
  }
  memcpy(&header, bytes, sizeof(header));
  if (!header.group_count || header.group_count > header.group_capacity || header.reserved[0] ||
      header.reserved[1] || header.reserved[2]) {
    goto invalid;
  }
  SQLayout layout;
  if (!language || !sq_layout(language, header.group_capacity, &layout) || layout.end > length ||
      header.groups_byte_offset != layout.groups[0] ||
      header.nodes_byte_offset != layout.nodes[0]) {
    goto invalid;
  }
  SQTree *tree = sq_allocate(language, header.group_capacity, error);
  if (!tree) {
    return NULL;
  }
  if (header.magic_bits != sq_header(tree)->magic_bits) {
    sq_tree_delete(tree);
    goto invalid;
  }
  uint64_t expected = layout.end;
  if (header.symbol_presence_byte_offset) {
    if (header.group_count <= 32 || header.symbol_presence_byte_offset != expected) {
      sq_tree_delete(tree);
      goto invalid;
    }
    /* The header is needed to compute the group-dependent index length. */
    sq_header(tree)->group_count = header.group_count;
    expected += presence_size(tree);
  }
  if (tree->supertype_count > 8) {
    if (!header.supertype_dictionary_count || header.supertype_dictionary_count > 256 ||
        header.supertype_dictionary_byte_offset != expected) {
      sq_tree_delete(tree);
      goto invalid;
    }
    expected +=
        (uint64_t)header.supertype_dictionary_count * ((tree->supertype_count + 63) / 64) * 8;
  } else if (header.supertype_dictionary_count || header.supertype_dictionary_byte_offset) {
    sq_tree_delete(tree);
    goto invalid;
  }
  if (header.field_exceptions_count) {
    if (header.field_exceptions_byte_offset != expected) {
      sq_tree_delete(tree);
      goto invalid;
    }
    expected +=
        ((uint64_t)header.field_exceptions_count * sizeof(SQFieldException) + 7) & ~UINT64_C(7);
  } else if (header.field_exceptions_byte_offset) {
    sq_tree_delete(tree);
    goto invalid;
  }
  if (expected != length) {
    sq_tree_delete(tree);
    goto invalid;
  }
  uint8_t *data = sq_reallocate_data(tree->data, tree->size, length);
  if (!data) {
    sq_tree_delete(tree);
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return NULL;
  }
  tree->data = data;
  tree->size = (uint32_t)length;
  memcpy(data, bytes, length);
  if (!validate_nodes(tree, error)) {
    sq_tree_delete(tree);
    return NULL;
  }
  if (tree->supertype_count > 8 && tree->supertype_count % 64) {
    uint32_t words = (tree->supertype_count + 63) / 64;
    for (uint32_t i = 0; i < header.supertype_dictionary_count; i++) {
      uint64_t word;
      memcpy(&word,
             data + header.supertype_dictionary_byte_offset + ((size_t)(i + 1) * words - 1) * 8, 8);
      if (word >> (tree->supertype_count % 64)) {
        sq_tree_delete(tree);
        goto invalid;
      }
    }
  }
  SQFieldException previous = {0};
  for (uint32_t i = 0; i < header.field_exceptions_count; i++) {
    SQFieldException entry;
    memcpy(&entry, data + header.field_exceptions_byte_offset + (size_t)i * sizeof(entry),
           sizeof(entry));
    SQNode parent = sq_tree_node_at_slot(tree, entry.parent);
    bool ordered = !i || entry.parent > previous.parent ||
                   (entry.parent == previous.parent && entry.field > previous.field);
    bool valid_target = entry.target == SQ_NONE || (parent.tree && entry.target > entry.parent &&
                                                    entry.target < sq_node_end_slot(parent) &&
                                                    sq_tree_node_at_slot(tree, entry.target).tree);
    if (!ordered || !parent.tree || !entry.field || entry.field > tree->language->field_count ||
        !valid_target) {
      sq_tree_delete(tree);
      goto invalid;
    }
    previous = entry;
  }
  if (header.symbol_presence_byte_offset) {
    /* Rebuilding also checks sorted occurrence lists, unused IDs, sentinels,
     * mode choices, padding and bitmap tail bits. */
    SQTree *check = sq_allocate(language, header.group_capacity, error);
    if (!check) {
      sq_tree_delete(tree);
      return NULL;
    }
    memcpy(check->data, data, layout.end);
    sq_header(check)->symbol_presence_byte_offset = 0;
    sq_header(check)->supertype_dictionary_byte_offset = 0;
    sq_header(check)->supertype_dictionary_count = 0;
    sq_header(check)->field_exceptions_byte_offset = 0;
    sq_header(check)->field_exceptions_count = 0;
    if (!sq_build_presence(check, error)) {
      sq_tree_delete(check);
      sq_tree_delete(tree);
      return NULL;
    }
    bool equal = !memcmp(data + header.symbol_presence_byte_offset,
                         check->data + sq_header(check)->symbol_presence_byte_offset,
                         (size_t)presence_size(tree));
    sq_tree_delete(check);
    if (!equal) {
      sq_tree_delete(tree);
      goto invalid;
    }
  }
  return tree;
invalid:
  sq_fail(error, SQ_ERROR_INVALID_SLAB);
  return NULL;
}
SQTree *sq_tree_repack(const SQTree *tree, SQError *error) {
  if (!tree) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }
  SQTree *copy = sq_tree_from_bytes(tree->language, tree->data, tree->size, error);
  if (copy && !sq_resize(copy, sq_tree_group_count(copy), error)) {
    sq_tree_delete(copy);
    return NULL;
  }
  return copy;
}
