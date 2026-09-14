#include "internal.h"

uint8_t *sq_allocate_data(size_t size) {
#if SQ_COLUMN_ALIGNMENT == 64
  if (size > SIZE_MAX - 63) {
    return NULL;
  }

  uint8_t *data = aligned_alloc(64, (size + 63) & ~(size_t)63);
  if (data) {
    memset(data, 0, size);
  }

  return data;
#else
  return calloc(1, size);
#endif
}

uint8_t *sq_reallocate_data(uint8_t *data, size_t old_size, size_t new_size) {
#if SQ_COLUMN_ALIGNMENT == 64
  // realloc need not retain an over-aligned address. The experimental layout
  // needs both an aligned base and aligned offsets to test cache-line starts.
  uint8_t *next = sq_allocate_data(new_size);
  if (next) {
    memcpy(next, data, old_size < new_size ? old_size : new_size);
    free(data);
  }

  return next;
#else
  (void)old_size;
  return realloc(data, new_size);
#endif
}

uint8_t sq_width(uint32_t max) {
  uint8_t bits = 2;
  while ((max >>= 1) > 1) {
    bits++;
  }

  return bits;
}

uint64_t sq_column_size(uint32_t count, uint8_t bits) {
  if (!bits) return 0;
  uint32_t lanes = 64 / bits;
  return ((uint64_t)count + lanes - 1) / lanes * 8;
}

static uint32_t column_offset(uint64_t *next, uint64_t bytes) {
  uint32_t result = (uint32_t)*next;
  *next = (*next + bytes + SQ_COLUMN_ALIGNMENT - 1) & ~(uint64_t)(SQ_COLUMN_ALIGNMENT - 1);
  return result;
}

bool sq_layout(const TSLanguage *language, uint32_t capacity, bool wide_supertypes, SQLayout *layout) {
  if (!capacity || capacity > UINT32_MAX / SQ_GROUP_SIZE) return false;
  layout->supertype_bits = wide_supertypes ? 16 : 8;
  layout->symbol_bits = sq_width(language->symbol_count + language->alias_count + 1);
  layout->field_bits = language->field_count ? sq_width(language->field_count) : 0;
  layout->symbol_lanes = (uint8_t)(64 / layout->symbol_bits);
  layout->field_lanes = layout->field_bits ? (uint8_t)(64 / layout->field_bits) : 0;
  layout->symbol_mask = (uint32_t)((UINT64_C(1) << layout->symbol_bits) - 1);
  layout->field_mask = (uint32_t)((UINT64_C(1) << layout->field_bits) - 1);
  uint32_t slots = capacity * SQ_GROUP_SIZE;
  uint64_t next =
      (sizeof(SQHeader) + SQ_COLUMN_ALIGNMENT - 1) & ~(uint64_t)(SQ_COLUMN_ALIGNMENT - 1);
  layout->waste = column_offset(&next, sq_column_size(capacity, SQ_WASTE_BITS));
  layout->start_byte_base = column_offset(&next, sq_array_size(capacity, 4));
  layout->start_byte_delta = column_offset(&next, sq_array_size(slots, 1));
  layout->end_byte_base = column_offset(&next, sq_array_size(capacity, 4));
  layout->end_byte_delta = column_offset(&next, sq_array_size(slots, 2));
  layout->span_base = column_offset(&next, sq_array_size(capacity, 4));
  layout->span_delta = column_offset(&next, sq_array_size(slots, 1));
  layout->symbol = column_offset(&next, sq_column_size(slots, layout->symbol_bits));
  layout->field = column_offset(&next, sq_column_size(slots, layout->field_bits));
  layout->supertype = column_offset(&next, sq_array_size(slots, layout->supertype_bits / 8));
  layout->last = column_offset(&next, sq_column_size(slots, 1));
  layout->extra = column_offset(&next, sq_column_size(slots, 1));
  layout->error = column_offset(&next, sq_column_size(slots, 1));
  layout->missing = column_offset(&next, sq_column_size(slots, 1));
#if SQ_INCLUDE_POINTS
  layout->start_point_base = column_offset(&next, sq_array_size(capacity, 8));
  layout->start_point = column_offset(&next, sq_array_size(slots, 2));
  layout->end_point_base = column_offset(&next, sq_array_size(capacity, 8));
  layout->end_point = column_offset(&next, sq_array_size(slots, 2));
#endif

  // Accumulate in u64 and reject overflow before exposing any offsets.
  if (next > UINT32_MAX) return false;
  layout->end = (uint32_t)next;
  return true;
}

void sq_set_packed(uint8_t *data, uint32_t offset, uint32_t index, uint8_t bits, uint32_t value) {
  switch (bits) {
  case 1:
    sq_set_bit(data, offset, index, value != 0);
    return;
  case 8:
    sq_set_u8(data, offset, index, (uint8_t)value);
    return;
  case 16:
    sq_set_u16(data, offset, index, (uint16_t)value);
    return;
  case 32:
    sq_set_u32(data, offset, index, value);
    return;
  }

  uint32_t lanes = 64 / bits, shift = index % lanes * bits;
  uint8_t *address = data + offset + (uint64_t)(index / lanes) * 8;
  uint64_t word, mask = ((UINT64_C(1) << bits) - 1) << shift;
  memcpy(&word, address, 8);
  word = (word & ~mask) | ((uint64_t)value << shift);
  memcpy(address, &word, 8);
}

size_t sq_runtime_size(const TSLanguage *language) {
  size_t symbols = (size_t)language->symbol_count + language->alias_count;
  size_t bytes = sizeof(SQTree) + symbols * sizeof(TSSymbol);
  return (bytes + SQ_COLUMN_ALIGNMENT - 1) & ~(size_t)(SQ_COLUMN_ALIGNMENT - 1);
}

bool sq_language_compatible(const TSLanguage *language) {
  return language && language->abi_version >= TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION &&
         language->abi_version <= TREE_SITTER_LANGUAGE_VERSION &&
         (uint64_t)language->symbol_count + language->alias_count <= ts_builtin_sym_error_repeat;
}

static SQTree *allocate_tree(const TSLanguage *language, uint32_t capacity, uint32_t payload_size,
                             SQStorage storage, const TSSymbol *supertypes,
                             uint32_t supertype_count, SQError *error) {
  sq_fail(error, SQ_OK);
  if (!sq_language_compatible(language)) {
    sq_fail(error, SQ_ERROR_LANGUAGE);
    return NULL;
  }

  TSSymbol direct_supertypes[8];
  if (!supertypes) {
    supertype_count = 0;
    for (uint32_t symbol = 0; symbol < language->symbol_count + language->alias_count; symbol++) {
      if (language->symbol_metadata[symbol].supertype) {
        if (supertype_count < 8) direct_supertypes[supertype_count] = (TSSymbol)symbol;
        supertype_count++;
      }
    }
  }
  if (!supertypes && supertype_count <= 8) supertypes = direct_supertypes;
  SQSupertypeGrammar *grammar = NULL;
  if (supertype_count > 8) {
    grammar = sq_supertype_grammar_acquire(language, supertype_count, error);
    if (!grammar) return NULL;
  }
  SQLayout layout;
  if (!sq_layout(language, capacity, grammar && grammar->count > 256, &layout)) {
    sq_supertype_grammar_release(grammar);
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return NULL;
  }

  if (!payload_size) payload_size = layout.end;
  size_t prefix = sq_runtime_size(language);
  if (storage == SQ_STORAGE_COLOCATED && payload_size > SIZE_MAX - prefix) {
    sq_supertype_grammar_release(grammar);
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return NULL;
  }

  size_t allocation = prefix + (storage == SQ_STORAGE_COLOCATED ? payload_size : 0);
  SQTree *tree = (SQTree *)sq_allocate_data(allocation);
  if (!tree) {
    sq_supertype_grammar_release(grammar);
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return NULL;
  }

  tree->supertype_grammar = grammar;
  tree->storage = storage;
  tree->language = ts_language_copy(language);
  tree->layout = layout;
  tree->size = payload_size;

  // Runtime metadata, including the supertype list, precedes the aligned payload.
  // ts_language_symbol_metadata is an out-of-line call that only special-cases the
  // two builtin error symbols, which this range never reaches. Read the array
  // directly: this scan is proportional to the grammar, not the tree, so it
  // otherwise dominates conversion of a small file. Keep ascending order, which
  // fixes each supertype's bit position in the serialized column.
  tree->supertypes = (TSSymbol *)(tree + 1);
  if (supertypes) {
    memcpy(tree->supertypes, supertypes, supertype_count * sizeof(TSSymbol));
    tree->supertype_count = supertype_count;
  } else {
    const TSSymbolMetadata *metadata = language->symbol_metadata;
    uint32_t symbols = (uint32_t)language->symbol_count + language->alias_count;
    for (uint32_t symbol = 0; symbol < symbols; symbol++) {
      if (metadata[symbol].supertype) {
        tree->supertypes[tree->supertype_count++] = (TSSymbol)symbol;
      }
    }
  }

  if (storage == SQ_STORAGE_COLOCATED) {
    tree->data = (uint8_t *)tree + prefix;
  } else if (storage == SQ_STORAGE_COPIED) {
    tree->data = sq_allocate_data(payload_size);
    if (!tree->data) {
      sq_tree_delete(tree);
      sq_fail(error, SQ_ERROR_ALLOCATION);
      return NULL;
    }
  }

  return tree;
}

SQTree *sq_allocate(const TSLanguage *language, uint32_t capacity, SQError *error) {
  return sq_allocate_cached(language, capacity, NULL, 0, error);
}

SQTree *sq_allocate_cached(const TSLanguage *language, uint32_t capacity,
                           const TSSymbol *supertypes, uint32_t count, SQError *error) {
  SQTree *tree = allocate_tree(language, capacity, 0, SQ_STORAGE_COLOCATED, supertypes, count, error);
  if (tree) {
    *sq_header(tree) =
        (SQHeader){.format_flags = SQ_VERSION | SQ_LAYOUT_FLAGS |
                         (tree->layout.supertype_bits == 16 ? SQ_WIDE_SUPERTYPES : 0),
                   .group_capacity = capacity,
                   .supertype_dictionary_count = tree->supertype_grammar ? tree->supertype_grammar->count : 0};
  }

  return tree;
}

SQTree *sq_allocate_loaded(const TSLanguage *language, uint32_t capacity, const void *bytes,
                           uint32_t length, bool borrowed, SQError *error) {
  SQTree *tree = allocate_tree(language, capacity, length,
                               borrowed ? SQ_STORAGE_BORROWED : SQ_STORAGE_COPIED, NULL, 0, error);
  if (tree) {
    if (borrowed) {
      // This storage is only read. Mutable helpers reject borrowed descriptors.
      tree->data = (uint8_t *)bytes;
    } else {
      memcpy(tree->data, bytes, length);
    }
  }

  return tree;
}

// Release storage without changing the language reference during relocation.
static void free_storage(SQTree *tree) {
  if (tree->storage == SQ_STORAGE_COPIED) free(tree->data);
  free(tree);
}

bool sq_grow_data(SQTree **tree_pointer, uint32_t size, SQError *error) {
  SQTree *tree = *tree_pointer;
  if (tree->storage == SQ_STORAGE_BORROWED || size < tree->size) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }

  if (tree->storage == SQ_STORAGE_COLOCATED) {
    size_t prefix = sq_runtime_size(tree->language);
    if (size > SIZE_MAX - prefix) {
      sq_fail(error, SQ_ERROR_OVERFLOW);
      return false;
    }

    SQTree *next =
        (SQTree *)sq_reallocate_data((uint8_t *)tree, prefix + tree->size, prefix + size);
    if (!next) {
      sq_fail(error, SQ_ERROR_ALLOCATION);
      return false;
    }

    next->data = (uint8_t *)next + prefix;
    next->supertypes = (TSSymbol *)(next + 1);
    *tree_pointer = tree = next;
  } else {
    uint8_t *data = sq_reallocate_data(tree->data, tree->size, size);
    if (!data) {
      sq_fail(error, SQ_ERROR_ALLOCATION);
      return false;
    }

    tree->data = data;
  }

  tree->size = size;
  return true;
}

// Rebuild column locations while preserving their prefix-filled lane indexes.
// Builder finalization can reserve a new empty suffix in the same allocation;
// ordinary resizing instead copies the tree's existing serialized suffix.
static bool resize_tree(SQTree **tree_pointer, uint32_t capacity, uint32_t trailing_size,
                        bool preserve_trailing, SQError *error) {
  SQTree *tree = *tree_pointer;
  if (tree->storage == SQ_STORAGE_BORROWED) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }

  SQHeader old = *sq_header(tree);
  if (capacity < old.group_count) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }

  SQLayout next;
  if (!sq_layout(tree->language, capacity, tree->layout.supertype_bits == 16, &next)) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }

  uint64_t total = (uint64_t)next.end + trailing_size;
  if (total > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }

  size_t prefix = sq_runtime_size(tree->language);
  if (total > SIZE_MAX - prefix) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }

  SQTree *replacement = (SQTree *)sq_allocate_data(prefix + (size_t)total);
  if (!replacement) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return false;
  }

  memcpy(replacement, tree, prefix);
  replacement->storage = SQ_STORAGE_COLOCATED;
  replacement->data = (uint8_t *)replacement + prefix;
  replacement->supertypes = (TSSymbol *)(replacement + 1);
  replacement->size = (uint32_t)total;
  replacement->layout = next;
  uint8_t *data = replacement->data;
  memcpy(data, &old, sizeof(old));
  ((SQHeader *)data)->group_capacity = capacity;

  // Prefix-filled reverse-preorder columns retain both physical indexes and
  // packed lane phase across growth. Copy their used words, including padding.
  uint32_t groups = old.group_count, slots = groups * SQ_GROUP_SIZE;
  memcpy(data + next.waste, tree->data + tree->layout.waste, sq_column_size(groups, SQ_WASTE_BITS));
  memcpy(data + next.start_byte_base, tree->data + tree->layout.start_byte_base,
         sq_array_size(groups, 4));
  memcpy(data + next.start_byte_delta, tree->data + tree->layout.start_byte_delta,
         sq_array_size(slots, 1));
  memcpy(data + next.end_byte_base, tree->data + tree->layout.end_byte_base,
         sq_array_size(groups, 4));
  memcpy(data + next.end_byte_delta, tree->data + tree->layout.end_byte_delta,
         sq_array_size(slots, 2));
  memcpy(data + next.span_base, tree->data + tree->layout.span_base, sq_array_size(groups, 4));
  memcpy(data + next.span_delta, tree->data + tree->layout.span_delta, sq_array_size(slots, 1));
  memcpy(data + next.symbol, tree->data + tree->layout.symbol,
         sq_column_size(slots, next.symbol_bits));
  memcpy(data + next.field, tree->data + tree->layout.field,
         sq_column_size(slots, next.field_bits));
  memcpy(data + next.supertype, tree->data + tree->layout.supertype,
         sq_array_size(slots, next.supertype_bits / 8));
  memcpy(data + next.last, tree->data + tree->layout.last, sq_column_size(slots, 1));
  memcpy(data + next.extra, tree->data + tree->layout.extra, sq_column_size(slots, 1));
  memcpy(data + next.error, tree->data + tree->layout.error, sq_column_size(slots, 1));
  memcpy(data + next.missing, tree->data + tree->layout.missing, sq_column_size(slots, 1));
#if SQ_INCLUDE_POINTS
  memcpy(data + next.start_point_base, tree->data + tree->layout.start_point_base,
         sq_array_size(groups, 8));
  memcpy(data + next.start_point, tree->data + tree->layout.start_point,
         sq_array_size(slots, 2));
  memcpy(data + next.end_point_base, tree->data + tree->layout.end_point_base,
         sq_array_size(groups, 8));
  memcpy(data + next.end_point, tree->data + tree->layout.end_point,
         sq_array_size(slots, 2));
#endif
  if (preserve_trailing) {
    memcpy(data + next.end, tree->data + tree->layout.end, trailing_size);
  }
  free_storage(tree);
  *tree_pointer = replacement;
  return true;
}

bool sq_resize(SQTree **tree_pointer, uint32_t capacity, SQError *error) {
  SQTree *tree = *tree_pointer;
  if (tree->storage == SQ_STORAGE_BORROWED || tree->size < tree->layout.end ||
      capacity < sq_header(tree)->group_count) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }
  if (capacity == sq_header(tree)->group_capacity && tree->storage == SQ_STORAGE_COLOCATED) {
    return true;
  }
  return resize_tree(tree_pointer, capacity, tree->size - tree->layout.end, true, error);
}

bool sq_prepare_final(SQTree **tree_pointer, uint32_t capacity, uint32_t trailing_size,
                      SQError *error) {
  SQTree *tree = *tree_pointer;
  if (tree->storage == SQ_STORAGE_BORROWED || tree->size != tree->layout.end ||
      capacity < sq_header(tree)->group_count) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }

  if (capacity != sq_header(tree)->group_capacity) {
    return resize_tree(tree_pointer, capacity, trailing_size, false, error);
  }

  uint64_t total = (uint64_t)tree->layout.end + trailing_size;
  if (total > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return false;
  }
  return total == tree->size || sq_grow_data(tree_pointer, (uint32_t)total, error);
}

void sq_tree_delete(SQTree *tree) {
  if (!tree) {
    return;
  }

  sq_supertype_grammar_release(tree->supertype_grammar);
  if (tree->language) {
    ts_language_delete(tree->language);
  }

  free_storage(tree);
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
    return "more than 65536 supertype masks";
  case SQ_ERROR_INVALID_SLAB:
    return "invalid or incompatible slab";
  case SQ_ERROR_LANGUAGE:
    return "unsupported language";
  default:
    return "unknown error";
  }
}
