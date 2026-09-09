#ifndef SQUAT_INTERNAL_H_
#define SQUAT_INTERNAL_H_
#include "include/tree_sitter/squat.h"
#include "../src/language.h"
#include <limits.h>
#include <stdlib.h>
#include <string.h>

/* Alternate sizes are experiment builds, with distinct magic flags. */
#ifndef SQ_GROUP_SIZE
#define SQ_GROUP_SIZE 16u
#endif
_Static_assert(SQ_GROUP_SIZE == 16 || SQ_GROUP_SIZE == 32 || SQ_GROUP_SIZE == 64,
               "supported experimental group sizes");
#ifndef SQ_COLUMN_ALIGNMENT
#define SQ_COLUMN_ALIGNMENT 8u
#endif
_Static_assert(SQ_COLUMN_ALIGNMENT == 8 || SQ_COLUMN_ALIGNMENT == 64,
               "supported experimental column alignments");
#define SQ_VERSION                                                                                 \
  (0x20u |                                                                                         \
   (SQ_GROUP_SIZE == 32   ? 2u                                                                     \
    : SQ_GROUP_SIZE == 64 ? 4u                                                                     \
                          : 0u) |                                                                  \
   (SQ_COLUMN_ALIGNMENT == 64 ? 8u : 0u))
#define SQ_DICTIONARY 1u
#define SQ_NONE UINT32_MAX

typedef struct {
  uint8_t magic_bits;
  uint8_t reserved[3];
  uint32_t group_count, group_capacity;
  uint32_t groups_byte_offset, nodes_byte_offset;
  uint32_t symbol_presence_byte_offset;
  uint32_t supertype_dictionary_byte_offset, supertype_dictionary_count;
  uint32_t field_exceptions_byte_offset, field_exceptions_count;
} SQHeader;
_Static_assert(sizeof(SQHeader) == 40, "slab header size");

enum { G_WASTE, G_SPAN, G_BYTE, G_END_BYTE, G_ROW, G_END_ROW, G_COL, G_END_COL, G_COLUMNS };
enum {
  N_LAST,
  N_EXTRA,
  N_ERROR,
  N_MISSING,
  N_SPAN,
  N_BYTE,
  N_END_BYTE,
  N_ROW,
  N_END_ROW,
  N_COL,
  N_END_COL,
  N_SUPER,
  N_SYMBOL,
  N_GRAMMAR,
  N_FIELD,
  N_COLUMNS
};

typedef struct {
  uint32_t parent, field, target;
} SQFieldException;
_Static_assert(sizeof(SQFieldException) == 12, "field exception size");

typedef struct {
  uint32_t groups[G_COLUMNS], nodes[N_COLUMNS], end;
  uint8_t symbol_bits, field_bits;
} SQLayout;
struct SQTree {
  const TSLanguage *language;
  uint8_t *data;
  uint32_t size;
  SQLayout layout;
  /* Sorted original grammar IDs, including supertype metadata in older ABIs. */
  TSSymbol *supertypes;
  uint32_t supertype_count;
};

static inline SQHeader *sq_header(const SQTree *tree) {
  return (SQHeader *)tree->data;
}
static inline uint32_t sq_symbols(const SQTree *tree) {
  return tree->language->symbol_count + tree->language->alias_count + 2;
}
static inline uint32_t sq_encode_symbol(const SQTree *tree, TSSymbol symbol) {
  return symbol == ts_builtin_sym_error          ? sq_symbols(tree) - 2
         : symbol == ts_builtin_sym_error_repeat ? sq_symbols(tree) - 1
                                                 : symbol;
}
static inline TSSymbol sq_decode_symbol(const SQTree *tree, uint32_t symbol) {
  return symbol == sq_symbols(tree) - 2   ? ts_builtin_sym_error
         : symbol == sq_symbols(tree) - 1 ? ts_builtin_sym_error_repeat
                                          : (TSSymbol)symbol;
}
bool sq_append_field_exceptions(SQTree *, const SQFieldException *, uint32_t, SQError *);
bool sq_lookup_field_exception(SQNode, TSFieldId, SQNode *);
uint8_t *sq_allocate_data(size_t size);
uint8_t *sq_reallocate_data(uint8_t *data, size_t old_size, size_t new_size);
uint64_t sq_lane_starts(uint8_t bits);
uint64_t sq_equal_lanes(uint64_t word, uint32_t value, uint8_t bits);
uint8_t sq_width(uint32_t max);
static inline uint8_t sq_group_width(unsigned column) {
  return column == G_WASTE ? (SQ_GROUP_SIZE == 16 ? 4 : SQ_GROUP_SIZE == 32 ? 5 : 6) : 32;
}
static inline uint8_t sq_node_width(const SQLayout *layout, unsigned column) {
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
uint64_t sq_column_size(uint32_t count, uint8_t bits);
bool sq_layout(const TSLanguage *, uint32_t capacity, SQLayout *);
static inline uint32_t sq_get(const uint8_t *data, uint32_t offset, uint32_t index,
                              uint8_t bits) {
  const uint8_t *column = data + offset;
  // Lanes occupy the low bits first in native-endian words. Narrow loads use
  // consecutive addresses only on little-endian hosts; the compiler folds this test.
  const uint16_t native_endian = 1;
  if (*(const uint8_t *)&native_endian) {
    switch (bits) {
    case 1:
      return (column[index / 8] >> (index % 8)) & 1;
    case 8:
      return column[index];
    case 16: {
      uint16_t value;
      memcpy(&value, column + (uint64_t)index * 2, sizeof(value));
      return value;
    }
    case 32: {
      uint32_t value;
      memcpy(&value, column + (uint64_t)index * 4, sizeof(value));
      return value;
    }
    }
  }
  uint32_t lanes = 64 / bits;
  uint64_t word;
  memcpy(&word, column + (uint64_t)(index / lanes) * 8, sizeof(word));
  return (uint32_t)((word >> (index % lanes * bits)) & ((UINT64_C(1) << bits) - 1));
}
void sq_set(uint8_t *, uint32_t offset, uint32_t index, uint8_t bits, uint32_t);
static inline uint32_t sq_group_get(const SQTree *tree, unsigned column, uint32_t group) {
  SQHeader *header = sq_header(tree);
  return sq_get(tree->data, tree->layout.groups[column],
                header->group_capacity - header->group_count + group, sq_group_width(column));
}
static inline uint32_t sq_node_get(SQNode node, unsigned column) {
  SQHeader *header = sq_header(node.tree);
  return sq_get(node.tree->data, node.tree->layout.nodes[column],
                (header->group_capacity - header->group_count) * SQ_GROUP_SIZE + node.slot,
                sq_node_width(&node.tree->layout, column));
}
uint32_t sq_next_slot(const SQTree *, uint32_t);
uint32_t sq_node_end_slot(SQNode);
SQNode sq_null(void);
SQTree *sq_allocate(const TSLanguage *, uint32_t, SQError *);
bool sq_resize(SQTree *, uint32_t, SQError *);
bool sq_build_presence(SQTree *, SQError *);
bool sq_append_dictionary(SQTree *, const uint64_t *, uint32_t, SQError *);
static inline void sq_fail(SQError *error, SQError value) {
  if (error) {
    *error = value;
  }
}
#endif
