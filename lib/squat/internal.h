#ifndef SQUAT_INTERNAL_H_
#define SQUAT_INTERNAL_H_
#include "include/tree_sitter/squat.h"
#include "../src/language.h"
#include <limits.h>
#include <stddef.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

// Alternate sizes are experiment builds, with distinct magic flags.
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
// Prototype format version 0; no persisted data or backward compatibility contract.
#define SQ_VERSION                                                                                 \
  (UINT32_C(0x53510000) |                                                                          \
   (SQ_GROUP_SIZE == 32   ? 2u                                                                     \
    : SQ_GROUP_SIZE == 64 ? 4u                                                                     \
                          : 0u) |                                                                  \
   (SQ_COLUMN_ALIGNMENT == 64 ? 8u : 0u) | 1u)

#define SQ_NO_POINTS 0x100u
#define SQ_PRESENCE 0x200u
#define SQ_WIDE_SUPERTYPES 0x400u
#define SQ_SEPARATE_GRAMMAR 0x800u
#define SQ_EXTRAS 0x1000u
#define SQ_MISSING 0x2000u
#define SQ_ERRORS 0x4000u
#define SQ_OPTIONAL_FLAGS (SQ_EXTRAS | SQ_MISSING | SQ_ERRORS | SQ_SEPARATE_GRAMMAR)
#define SQ_NONE UINT32_MAX

typedef struct {
  uint32_t format_flags;
  uint32_t group_count, group_capacity;
  uint32_t supertype_dictionary_count;
} SQHeader;

_Static_assert(sizeof(SQHeader) == 16, "slab header size");

enum { SQ_WASTE_OFFSET = (sizeof(SQHeader) + SQ_COLUMN_ALIGNMENT - 1) & ~(SQ_COLUMN_ALIGNMENT - 1) };

#define SQ_WASTE_BITS 16u

typedef struct {
  // Slab offsets in persisted order; group bases precede their node values.
  uint32_t waste;
  uint32_t start_byte_base;
  uint32_t start_byte_delta;
  uint32_t end_byte_base;
  uint32_t end_byte_delta;
  uint32_t span_base;
  uint32_t span_delta;
  uint32_t symbol;
  uint32_t field;
  uint32_t supertype;
  uint32_t last;
  uint32_t start_point_base;
  uint32_t start_point;
  uint32_t end_point_base;
  uint32_t end_point;
  uint32_t extra;
  uint32_t missing;
  uint32_t error;
  uint32_t grammar;
  uint32_t end;
  uint8_t symbol_bits, field_bits, supertype_bits;
  // Grammar-wide decoder constants; runtime-only, never serialized.
  uint8_t symbol_lanes, field_lanes, symbol_shift;
  uint32_t symbol_mask, field_mask;
} SQLayout;

typedef struct SQSupertypeGrammar {
  const TSLanguage *language;
  uint64_t *masks;
  uint32_t *table;
  uint32_t count, words, supertype_count, table_capacity;
} SQSupertypeGrammar;

SQSupertypeGrammar *sq_supertype_grammar_new(const TSLanguage *, uint32_t, SQError *);
SQSupertypeGrammar *sq_supertype_grammar_new_cached(const TSLanguage *, uint32_t,
                                                  const void *, size_t, SQError *);
void sq_supertype_grammar_delete(SQSupertypeGrammar *);
uint32_t sq_supertype_mask_id(const SQSupertypeGrammar *, const uint64_t *);
size_t sq_supertype_grammar_cache_size(const SQSupertypeGrammar *);
bool sq_supertype_grammar_copy_cache(const SQSupertypeGrammar *, void *, size_t, SQError *);

typedef struct {
  uint32_t offset, length;
} DirectFieldSlice;

// Codes place the public display ID immediately above a grammar-wide variant.
// Dictionaries are shared by trees. Zero selects the unique display default
// in global mode; local mode indexes the dictionary by the whole code.
typedef enum { SQ_SYMBOL_LOCAL, SQ_SYMBOL_GLOBAL, SQ_SYMBOL_BYTES } SQSymbolEncoding;

typedef struct {
  uint16_t *grammar_ids; // local code or global selector -> original ID
  uint16_t *default_codes; // original ID -> unaliased code
  uint16_t *counts; // variants per public display ID
  uint16_t *defaults, *grammar_codes; // global: unique display default, original ID -> selector
  SQSymbolEncoding encoding;
  uint32_t length;
  uint8_t shift;
  bool separate;
} SQSymbolTable;

bool sq_symbol_table_init(const TSLanguage *, SQSymbolTable *, SQError *);
void sq_symbol_table_delete(SQSymbolTable *);
uint32_t sq_symbol_code(const SQGrammar *, uint32_t display, uint32_t original);

struct SQGrammar {
  atomic_size_t references;
  _Atomic(struct TFLanguage *) direct_language;
  const TSLanguage *language;
  TSSymbol *supertypes;
  uint32_t supertype_count;
  uint16_t *supertype_indexes, *public_index;
  SQSymbolTable symbols;
  DirectFieldSlice *production_fields;
  TSFieldId *direct_fields;
  SQSupertypeGrammar *supertype_grammar;
};

typedef enum { SQ_STORAGE_COLOCATED, SQ_STORAGE_BORROWED } SQStorage;
struct SQTree {
  SQGrammar *grammar;
  const TSLanguage *language;
  uint8_t *data;
  uint32_t size;
  SQLayout layout;

  // Sorted original grammar IDs, including supertype metadata in older ABIs.
  const TSSymbol *supertypes;
  uint32_t supertype_count;
  SQStorage storage;
  SQSupertypeGrammar *supertype_grammar;
};

// Private Rust scan bridge. Offsets address immutable little-endian slab columns;
// grammar tables use native integers. All pointers borrow the tree.
typedef struct {
  uint32_t group_shift, symbol_count, symbol_shift;
  uint32_t waste, span_base, span_delta;
  uint32_t start_byte_base, start_byte_delta, end_byte_base, end_byte_delta;
  uint32_t symbol, field, supertype, extra, missing;
} SQScanLayout;

typedef struct {
  const uint8_t *data;
  const TSSymbol *supertypes;
  const uint64_t *supertype_masks;
  uint32_t size, supertype_count, supertype_mask_count;
  SQScanLayout layout;
} SQScanColumns;

void sq_tree_scan_columns(const SQTree *, SQScanColumns *);

typedef struct {
  uint32_t modes, entries, entry_bytes;
} SQScanSymbolIndex;

void sq_tree_scan_symbol_index(const SQTree *, SQScanSymbolIndex *);

typedef struct {
  uint32_t start_base, start_delta, end_base, end_delta;
} SQScanPointLayout;

void sq_tree_scan_point_layout(const SQTree *, SQScanPointLayout *);

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

// Symbols decoded from validated nodes include aliases and the two built-in errors.
static inline bool sq_symbol_is_named(const TSLanguage *language, TSSymbol symbol) {
  return symbol < ts_builtin_sym_error_repeat ? language->symbol_metadata[symbol].named
                                              : symbol == ts_builtin_sym_error;
}

static inline const char *sq_symbol_name(const TSLanguage *language, TSSymbol symbol) {
  return symbol < ts_builtin_sym_error_repeat ? language->symbol_names[symbol]
                                              : ts_language_symbol_name(language, symbol);
}

uint8_t *sq_allocate_data(size_t size);
uint8_t *sq_reallocate_data(uint8_t *data, size_t old_size, size_t new_size);
uint64_t sq_lane_starts(uint8_t bits);
uint64_t sq_equal_lanes(uint64_t word, uint32_t value, uint8_t bits);
uint8_t sq_width(uint32_t max);
uint8_t sq_field_width(uint32_t max);
uint8_t sq_symbol_width(uint32_t max);
uint64_t sq_column_size(uint32_t count, uint8_t bits);
static inline uint64_t sq_array_size(uint32_t count, unsigned bytes) {
  return ((uint64_t)count * bytes + 7) & ~UINT64_C(7);
}

bool sq_layout(const SQGrammar *, uint32_t capacity, bool wide_supertypes, bool points,
               uint32_t flags, SQLayout *);

// Serialized integers are little-endian; packed words start at their low bits.
// The endian test and native-host conversions fold away.
static inline bool sq_little_endian(void) {
  const uint16_t one = 1;
  return *(const uint8_t *)&one != 0;
}

static inline uint8_t sq_get_u8(const uint8_t *data, uint32_t offset, uint32_t index) {
  return data[offset + (uint64_t)index];
}

static inline void sq_set_u8(uint8_t *data, uint32_t offset, uint32_t index, uint8_t value) {
  data[offset + (uint64_t)index] = value;
}

static inline uint16_t sq_get_u16(const uint8_t *data, uint32_t offset, uint32_t index) {
  uint16_t value;
  memcpy(&value, data + offset + (uint64_t)index * 2, sizeof(value));
  return sq_little_endian() ? value : __builtin_bswap16(value);
}

static inline void sq_set_u16(uint8_t *data, uint32_t offset, uint32_t index, uint16_t value) {
  if (!sq_little_endian()) value = __builtin_bswap16(value);
  memcpy(data + offset + (uint64_t)index * 2, &value, sizeof(value));
}

static inline uint32_t sq_get_u32(const uint8_t *data, uint32_t offset, uint32_t index) {
  uint32_t value;
  memcpy(&value, data + offset + (uint64_t)index * 4, sizeof(value));
  return sq_little_endian() ? value : __builtin_bswap32(value);
}

static inline void sq_set_u32(uint8_t *data, uint32_t offset, uint32_t index, uint32_t value) {
  if (!sq_little_endian()) value = __builtin_bswap32(value);
  memcpy(data + offset + (uint64_t)index * 4, &value, sizeof(value));
}

static inline uint64_t sq_get_u64(const uint8_t *data, uint32_t offset, uint32_t index) {
  uint64_t value;
  memcpy(&value, data + offset + (uint64_t)index * 8, sizeof(value));
  return sq_little_endian() ? value : __builtin_bswap64(value);
}

static inline void sq_set_u64(uint8_t *data, uint32_t offset, uint32_t index, uint64_t value) {
  if (!sq_little_endian()) value = __builtin_bswap64(value);
  memcpy(data + offset + (uint64_t)index * 8, &value, sizeof(value));
}

#define sq_header_get(tree, field) sq_get_u32((tree)->data, offsetof(SQHeader, field), 0)
#define sq_header_set(tree, field, value) \
  sq_set_u32((tree)->data, offsetof(SQHeader, field), 0, value)

static inline SQHeader sq_read_header(const void *data) {
  return (SQHeader){sq_get_u32(data, 0, 0), sq_get_u32(data, 0, 1),
                    sq_get_u32(data, 0, 2), sq_get_u32(data, 0, 3)};
}

static inline void sq_write_header(uint8_t *data, SQHeader header) {
  sq_set_u32(data, 0, 0, header.format_flags);
  sq_set_u32(data, 0, 1, header.group_count);
  sq_set_u32(data, 0, 2, header.group_capacity);
  sq_set_u32(data, 0, 3, header.supertype_dictionary_count);
}

static inline bool sq_get_bit(const uint8_t *data, uint32_t offset, uint32_t index) {
  return (sq_get_u8(data, offset, index / 8) >> (index % 8)) & 1;
}

static inline void sq_set_bit(uint8_t *data, uint32_t offset, uint32_t index, bool value) {
  uint8_t byte = sq_get_u8(data, offset, index / 8);
  uint8_t mask = (uint8_t)(1u << (index % 8));
  sq_set_u8(data, offset, index / 8, (uint8_t)((byte & ~mask) | (value ? mask : 0)));
}

static inline uint32_t sq_get_packed(const uint8_t *data, uint32_t offset, uint32_t index,
                                     uint8_t bits) {
  switch (bits) {
  case 1:
    return sq_get_bit(data, offset, index);
  case 2:
    return (sq_get_u8(data, offset, index / 4) >> ((index % 4) * 2)) & 3u;
  case 4:
    return (sq_get_u8(data, offset, index / 2) >> ((index % 2) * 4)) & 15u;
  case 8:
    return sq_get_u8(data, offset, index);
  case 16:
    return sq_get_u16(data, offset, index);
  case 32:
    return sq_get_u32(data, offset, index);
  }

  uint32_t lanes = 64 / bits;
  uint64_t word = sq_get_u64(data, offset, index / lanes);
  return (uint32_t)((word >> (index % lanes * bits)) & ((UINT64_C(1) << bits) - 1));
}

void sq_set_packed(uint8_t *, uint32_t offset, uint32_t index, uint8_t bits, uint32_t);

static inline uint32_t sq_group_waste(const SQTree *tree, uint32_t group) {
  return sq_get_packed(tree->data, SQ_WASTE_OFFSET, group, SQ_WASTE_BITS);
}

// The input is live; only crossing a group boundary can encounter waste.
static inline uint32_t sq_previous_live_slot(const SQTree *tree, uint32_t slot) {
  if (!slot) return SQ_NONE;
  uint32_t previous = slot - 1;
  return slot % SQ_GROUP_SIZE == 0 ? previous - sq_group_waste(tree, previous / SQ_GROUP_SIZE)
                                  : previous;
}

static inline uint32_t sq_group_span_base(const SQTree *tree, uint32_t group) {
  return sq_get_u32(tree->data, tree->layout.span_base, group);
}

static inline uint32_t sq_group_start_byte_base(const SQTree *tree, uint32_t group) {
  return sq_get_u32(tree->data, tree->layout.start_byte_base, group);
}

static inline uint32_t sq_group_end_byte_base(const SQTree *tree, uint32_t group) {
  return sq_get_u32(tree->data, tree->layout.end_byte_base, group);
}

static inline uint64_t sq_point_key(TSPoint point) {
  return (uint64_t)point.row << 32 | point.column;
}

static inline TSPoint sq_point_from_key(uint64_t key) {
  return (TSPoint){(uint32_t)(key >> 32), (uint32_t)key};
}

static inline uint64_t sq_expand_point_key(uint16_t key) {
  return (uint64_t)(key >> 8) << 32 | (key & UINT8_MAX);
}

static inline uint64_t sq_group_start_point_base(const SQTree *tree, uint32_t group) {
  return sq_get_u64(tree->data, tree->layout.start_point_base, group);
}

static inline uint64_t sq_group_end_point_base(const SQTree *tree, uint32_t group) {
  return sq_get_u64(tree->data, tree->layout.end_point_base, group);
}

static inline uint32_t sq_node_last_flag(SQNode node) {
  return sq_get_bit(node.tree->data, node.tree->layout.last, node.slot);
}

static inline uint32_t sq_node_extra_flag(SQNode node) {
  return (sq_header_get(node.tree, format_flags) & SQ_EXTRAS) &&
         sq_get_bit(node.tree->data, node.tree->layout.extra, node.slot);
}

static inline uint32_t sq_node_error_flag(SQNode node) {
  return (sq_header_get(node.tree, format_flags) & SQ_ERRORS) &&
         sq_get_bit(node.tree->data, node.tree->layout.error, node.slot / SQ_GROUP_SIZE);
}

static inline uint32_t sq_node_missing_flag(SQNode node) {
  return (sq_header_get(node.tree, format_flags) & SQ_MISSING) &&
         sq_get_bit(node.tree->data, node.tree->layout.missing, node.slot);
}

static inline uint32_t sq_node_span_delta(SQNode node) {
  return sq_get_u8(node.tree->data, node.tree->layout.span_delta, node.slot);
}

static inline uint32_t sq_node_start_byte_delta(SQNode node) {
  return sq_get_u8(node.tree->data, node.tree->layout.start_byte_delta, node.slot);
}

static inline uint32_t sq_node_end_byte_delta(SQNode node) {
  return sq_get_u16(node.tree->data, node.tree->layout.end_byte_delta, node.slot);
}

static inline uint32_t sq_node_start_point_key(SQNode node) {
  return sq_get_u16(node.tree->data, node.tree->layout.start_point, node.slot);
}

static inline uint32_t sq_node_end_point_key(SQNode node) {
  return sq_get_u16(node.tree->data, node.tree->layout.end_point, node.slot);
}

static inline uint32_t sq_node_supertype(SQNode node) {
  return sq_get_u16(node.tree->data, node.tree->layout.supertype, node.slot);
}

static inline uint16_t sq_node_symbol_code(SQNode node) {
  return sq_get_u16(node.tree->data, node.tree->layout.symbol, node.slot);
}

static inline uint32_t sq_node_symbol_id(SQNode node) {
  return node.tree->layout.symbol_shift == 8
      ? node.tree->data[node.tree->layout.symbol + (uint64_t)node.slot * 2 + 1]
      : sq_node_symbol_code(node) >> node.tree->layout.symbol_shift;
}

uint32_t sq_node_grammar_id(SQNode);
uint32_t sq_node_grammar_id_with_code(SQNode, uint16_t code);

static inline uint32_t sq_node_field_value(SQNode node) {
  return sq_get_u16(node.tree->data, node.tree->layout.field, node.slot);
}

uint32_t sq_previous_slot(const SQTree *, uint32_t);
uint32_t sq_next_position(const SQTree *, uint32_t);
uint32_t sq_node_first_slot(SQNode);

// Query plans use ascending preorder positions; node handles use direct,
// descending physical slots. Conversion is confined to ordered scans.
uint32_t sq_node_end_slot(SQNode);
static inline uint32_t sq_node_position(SQNode node) {
  return sq_tree_slot_count(node.tree) - 1 - node.slot;
}

static inline SQNode sq_position_node(const SQTree *tree, uint32_t position) {
  return (SQNode){tree, sq_tree_slot_count(tree) - 1 - position};
}

static inline uint32_t sq_position_group(const SQTree *tree, uint32_t group) {
  return sq_tree_group_count(tree) - 1 - group;
}

SQNode sq_null(void);
SQTree *sq_allocate(SQGrammar *, uint32_t, bool points, SQError *);
bool sq_language_compatible(const TSLanguage *);

// Builder operations may move a colocated descriptor. Refresh the caller's
// pointer before reading it again; finalized public trees never move.
size_t sq_runtime_size(void);
SQTree *sq_allocate_loaded(SQGrammar *, uint32_t, const void *, uint32_t, bool borrowed,
                           bool points, SQError *);
bool sq_resize(SQTree **, uint32_t, SQError *);
bool sq_prepare_final(SQTree **, uint32_t capacity, uint32_t trailing_size, uint32_t flags, SQError *);
bool sq_grow_data(SQTree **, uint32_t, SQError *);
bool sq_build_presence(SQTree *, SQError *);
bool sq_build_presence_cached(SQTree *, uint8_t **, size_t *, SQError *);
uint64_t sq_presence_size(const SQTree *);
static inline uint32_t sq_presence_offset(const SQTree *tree) {
  return sq_header_get(tree, format_flags) & SQ_PRESENCE ? tree->layout.end : 0;
}

static inline void sq_fail(SQError *error, SQError value) {
  if (error) {
    *error = value;
  }
}
#endif
