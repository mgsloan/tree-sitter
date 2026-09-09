#include "internal.h"

uint8_t sq_width(uint32_t max) {
  uint8_t bits = 2;
  while ((max >>= 1) > 1) {
    bits++;
  }
  return bits;
}
uint8_t sq_group_width(unsigned column) {
  return column == G_WASTE ? sq_width(SQ_GROUP_SIZE - 1) : 32;
}
uint8_t sq_node_width(const SQLayout *layout, unsigned column) {
  if (column < N_SPAN) {
    return 1;
  }
  if (column == N_END_BYTE) {
    return 16;
  }
  if (column == N_SYMBOL || column == N_GRAMMAR) {
    return layout->symbol_bits;
  }
  if (column == N_FIELD) {
    return layout->field_bits;
  }
  return 8;
}
uint64_t sq_column_size(uint32_t count, uint8_t bits) {
  uint32_t lanes = 64 / bits;
  return ((uint64_t)count + lanes - 1) / lanes * 8;
}
bool sq_layout(const TSLanguage *language, uint32_t capacity, SQLayout *layout) {
  if (!capacity || capacity > UINT32_MAX / SQ_GROUP_SIZE) {
    return false;
  }
  layout->symbol_bits = sq_width(language->symbol_count + language->alias_count + 1);
  layout->field_bits = sq_width(language->field_count);
  uint64_t offset = sizeof(SQHeader);
  for (unsigned c = 0; c < G_COLUMNS; c++) {
    if (offset > UINT32_MAX) {
      return false;
    }
    layout->groups[c] = (uint32_t)offset;
    offset += sq_column_size(capacity, sq_group_width(c));
  }
  for (unsigned c = 0; c < N_COLUMNS; c++) {
    if (offset > UINT32_MAX) {
      return false;
    }
    layout->nodes[c] = (uint32_t)offset;
    offset += sq_column_size(capacity * SQ_GROUP_SIZE, sq_node_width(layout, c));
  }
  if (offset > UINT32_MAX) {
    return false;
  }
  layout->end = (uint32_t)offset;
  return true;
}
uint32_t sq_get(const uint8_t *data, uint32_t offset, uint32_t index, uint8_t bits) {
  uint32_t lanes = 64 / bits;
  uint64_t word;
  memcpy(&word, data + offset + (uint64_t)(index / lanes) * 8, 8);
  return (uint32_t)((word >> (index % lanes * bits)) & ((UINT64_C(1) << bits) - 1));
}
void sq_set(uint8_t *data, uint32_t offset, uint32_t index, uint8_t bits, uint32_t value) {
  uint32_t lanes = 64 / bits, shift = index % lanes * bits;
  uint8_t *address = data + offset + (uint64_t)(index / lanes) * 8;
  uint64_t word, mask = ((UINT64_C(1) << bits) - 1) << shift;
  memcpy(&word, address, 8);
  word = (word & ~mask) | ((uint64_t)value << shift);
  memcpy(address, &word, 8);
}
uint32_t sq_group_get(const SQTree *tree, unsigned column, uint32_t group) {
  SQHeader *header = sq_header(tree);
  return sq_get(tree->data, tree->layout.groups[column],
                header->group_capacity - header->group_count + group, sq_group_width(column));
}
uint32_t sq_node_get(SQNode node, unsigned column) {
  SQHeader *header = sq_header(node.tree);
  return sq_get(node.tree->data, node.tree->layout.nodes[column],
                (header->group_capacity - header->group_count) * SQ_GROUP_SIZE + node.slot,
                sq_node_width(&node.tree->layout, column));
}
SQTree *sq_allocate(const TSLanguage *language, uint32_t capacity, SQError *error) {
  sq_fail(error, SQ_OK);
  if (!language || language->abi_version < TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION ||
      language->abi_version > TREE_SITTER_LANGUAGE_VERSION ||
      language->symbol_count + language->alias_count > ts_builtin_sym_error_repeat) {
    sq_fail(error, SQ_ERROR_LANGUAGE);
    return NULL;
  }
  SQLayout layout;
  if (!sq_layout(language, capacity, &layout)) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return NULL;
  }
  SQTree *tree = calloc(1, sizeof(*tree));
  if (!tree) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return NULL;
  }
  tree->data = calloc(1, layout.end);
  tree->supertypes =
      malloc((size_t)(language->symbol_count + language->alias_count) * sizeof(TSSymbol));
  if (!tree->data || !tree->supertypes) {
    sq_tree_delete(tree);
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return NULL;
  }
  tree->language = ts_language_copy(language);
  tree->layout = layout;
  tree->size = layout.end;
  for (uint32_t s = 0; s < language->symbol_count + language->alias_count; s++) {
    if (ts_language_symbol_metadata(language, (TSSymbol)s).supertype) {
      tree->supertypes[tree->supertype_count++] = (TSSymbol)s;
    }
  }
  SQHeader *header = sq_header(tree);
  header->magic_bits = SQ_VERSION | (tree->supertype_count > 8 ? SQ_DICTIONARY : 0);
  header->group_capacity = capacity;
  header->groups_byte_offset = layout.groups[0];
  header->nodes_byte_offset = layout.nodes[0];
  return tree;
}
/* Relocate values, not bytes: non-straddling lanes may change alignment. */
bool sq_resize(SQTree *tree, uint32_t capacity, SQError *error) {
  SQHeader old = *sq_header(tree);
  if (capacity < old.group_count) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }
  SQLayout next;
  if (!sq_layout(tree->language, capacity, &next)) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }
  uint64_t total = (uint64_t)next.end + tree->size - tree->layout.end;
  if (total > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }
  uint8_t *data = calloc(1, (size_t)total);
  if (!data) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }
  memcpy(data, &old, sizeof(old));
  SQHeader *header = (SQHeader *)data;
  header->group_capacity = capacity;
  header->groups_byte_offset = next.groups[0];
  header->nodes_byte_offset = next.nodes[0];
  if (header->symbol_presence_byte_offset) {
    header->symbol_presence_byte_offset =
        next.end + old.symbol_presence_byte_offset - tree->layout.end;
  }
  if (header->supertype_dictionary_byte_offset) {
    header->supertype_dictionary_byte_offset =
        next.end + old.supertype_dictionary_byte_offset - tree->layout.end;
  }
  for (unsigned region = 0; region < 2; region++) {
    unsigned columns = region ? N_COLUMNS : G_COLUMNS;
    uint32_t scale = region ? SQ_GROUP_SIZE : 1;
    for (unsigned c = 0; c < columns; c++) {
      uint8_t bits = region ? sq_node_width(&next, c) : sq_group_width(c);
      uint32_t source_offset = region ? tree->layout.nodes[c] : tree->layout.groups[c];
      uint32_t destination_offset = region ? next.nodes[c] : next.groups[c];
      for (uint32_t i = 0; i < old.group_count * scale; i++) {
        uint32_t value = sq_get(tree->data, source_offset,
                                (old.group_capacity - old.group_count) * scale + i, bits);
        sq_set(data, destination_offset, (capacity - old.group_count) * scale + i, bits, value);
      }
    }
  }
  memcpy(data + next.end, tree->data + tree->layout.end, tree->size - tree->layout.end);
  free(tree->data);
  tree->data = data;
  tree->size = (uint32_t)total;
  tree->layout = next;
  return true;
}
void sq_tree_delete(SQTree *tree) {
  if (!tree) {
    return;
  }
  if (tree->language) {
    ts_language_delete(tree->language);
  }
  free(tree->data);
  free(tree->supertypes);
  free(tree);
}
const TSLanguage *sq_tree_language(const SQTree *tree) {
  return tree ? tree->language : NULL;
}
const void *sq_tree_data(const SQTree *tree, uint32_t *length) {
  if (length) {
    *length = tree ? tree->size : 0;
  }
  return tree ? tree->data : NULL;
}
uint32_t sq_tree_group_count(const SQTree *tree) {
  return tree ? sq_header(tree)->group_count : 0;
}
uint32_t sq_tree_group_capacity(const SQTree *tree) {
  return tree ? sq_header(tree)->group_capacity : 0;
}
uint32_t sq_tree_slot_count(const SQTree *tree) {
  return sq_tree_group_count(tree) * SQ_GROUP_SIZE;
}
const char *sq_error_string(SQError error) {
  switch (error) {
  case SQ_OK:
    return "success";
  case SQ_ERROR_ARGUMENT:
    return "invalid argument";
  case SQ_ERROR_ALLOCATION:
    return "allocation failed";
  case SQ_ERROR_OVERFLOW:
    return "slab exceeds 32-bit address space";
  case SQ_ERROR_DICTIONARY_FULL:
    return "more than 256 supertype masks";
  case SQ_ERROR_INVALID_SLAB:
    return "invalid or incompatible slab";
  case SQ_ERROR_LANGUAGE:
    return "unsupported language";
  default:
    return "unknown error";
  }
}
