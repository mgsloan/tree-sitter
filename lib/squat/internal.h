#ifndef SQUAT_INTERNAL_H_
#define SQUAT_INTERNAL_H_
#include "include/tree_sitter/squat.h"
#include "../src/language.h"
#include <limits.h>
#include <stdlib.h>
#include <string.h>

#define SQ_GROUP_SIZE 16u
#define SQ_VERSION 0x10u
#define SQ_DICTIONARY 1u
#define SQ_NONE UINT32_MAX

typedef struct {
  uint8_t magic_bits;
  uint8_t reserved[3];
  uint32_t group_count, group_capacity;
  uint32_t groups_byte_offset, nodes_byte_offset;
  uint32_t symbol_presence_byte_offset;
  uint32_t supertype_dictionary_byte_offset, supertype_dictionary_count;
} SQHeader;
_Static_assert(sizeof(SQHeader) == 32, "slab header size");

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
uint8_t sq_width(uint32_t max);
uint8_t sq_group_width(unsigned col);
uint8_t sq_node_width(const SQLayout *, unsigned col);
uint64_t sq_column_size(uint32_t count, uint8_t bits);
bool sq_layout(const TSLanguage *, uint32_t capacity, SQLayout *);
uint32_t sq_get(const uint8_t *, uint32_t offset, uint32_t index, uint8_t bits);
void sq_set(uint8_t *, uint32_t offset, uint32_t index, uint8_t bits, uint32_t);
uint32_t sq_group_get(const SQTree *, unsigned, uint32_t);
uint32_t sq_node_get(SQNode, unsigned);
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
