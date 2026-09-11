#ifndef SQUAT_INTERNAL_H_
#define SQUAT_INTERNAL_H_
#include "include/tree_sitter/squat.h"
#include "../src/language.h"
#include <limits.h>
#include <stdlib.h>
#include <string.h>

// Alternate sizes are experiment builds, with distinct magic flags.
#ifndef SQ_GROUP_SIZE
#define SQ_GROUP_SIZE 16u
#endif
_Static_assert(SQ_GROUP_SIZE == 16 || SQ_GROUP_SIZE == 32 || SQ_GROUP_SIZE == 64,
               "supported experimental group sizes");

// Iterator unpack windows are independent of the serialized group layout.
#ifndef SQ_ITERATOR_UNPACK_SLOTS
#define SQ_ITERATOR_UNPACK_SLOTS SQ_GROUP_SIZE
#endif
_Static_assert(SQ_ITERATOR_UNPACK_SLOTS >= SQ_GROUP_SIZE &&
                   (SQ_ITERATOR_UNPACK_SLOTS & (SQ_ITERATOR_UNPACK_SLOTS - 1)) == 0,
               "unpack windows must contain a power-of-two number of whole groups");
#ifndef SQ_COLUMN_ALIGNMENT
#define SQ_COLUMN_ALIGNMENT 8u
#endif
_Static_assert(SQ_COLUMN_ALIGNMENT == 8 || SQ_COLUMN_ALIGNMENT == 64,
               "supported experimental column alignments");
#define SQ_VERSION                                                                                 \
  (UINT32_C(0x53510040) |                                                                          \
   (SQ_GROUP_SIZE == 32   ? 2u                                                                     \
    : SQ_GROUP_SIZE == 64 ? 4u                                                                     \
                          : 0u) |                                                                  \
   (SQ_COLUMN_ALIGNMENT == 64 ? 8u : 0u))

// Version 4: layout flags and optional sections share one format word.
#define SQ_LAYOUT_FLAGS (SQ_INCLUDE_POINTS ? 0u : 0x100u)
#define SQ_PRESENCE 0x200u
#define SQ_NONE UINT32_MAX

typedef struct {
  uint32_t format_flags;
  uint32_t group_count, group_capacity;
  uint32_t supertype_dictionary_count;
} SQHeader;

_Static_assert(sizeof(SQHeader) == 16, "slab header size");

#define SQ_WASTE_BITS (SQ_GROUP_SIZE == 16 ? 4u : SQ_GROUP_SIZE == 32 ? 5u : 6u)

typedef struct {
  // Per-group columns: packed waste followed by native u32 bases.
  uint32_t waste;
  uint32_t span_base;
  uint32_t start_byte_base;
  uint32_t end_byte_base;
#if SQ_INCLUDE_POINTS
  uint32_t start_row_base;
  uint32_t end_row_base;
  uint32_t start_column_base;
  uint32_t end_column_base;
#endif
  // Per-node columns, in their persisted order. These are slab offsets.
  uint32_t last;
  uint32_t extra;
  uint32_t error;
  uint32_t missing;
  uint32_t span_delta;
  uint32_t start_byte_delta;
  uint32_t end_byte_delta;
#if SQ_INCLUDE_POINTS
  uint32_t start_row_delta;
  uint32_t end_row_delta;
  uint32_t start_column_delta;
  uint32_t end_column_delta;
#endif
  uint32_t supertype;
  uint32_t symbol;
  uint32_t grammar_symbol;
  uint32_t field;
  uint32_t end;
  uint8_t symbol_bits, field_bits;
} SQLayout;

typedef enum { SQ_STORAGE_COLOCATED, SQ_STORAGE_COPIED, SQ_STORAGE_BORROWED } SQStorage;
struct SQTree {
  const TSLanguage *language;
  uint8_t *data;
  uint32_t size;
  SQLayout layout;

  // Sorted original grammar IDs, including supertype metadata in older ABIs.
  TSSymbol *supertypes;
  uint32_t supertype_count;
  SQStorage storage;
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

uint8_t *sq_allocate_data(size_t size);
uint8_t *sq_reallocate_data(uint8_t *data, size_t old_size, size_t new_size);
uint64_t sq_lane_starts(uint8_t bits);
uint64_t sq_equal_lanes(uint64_t word, uint32_t value, uint8_t bits);
uint8_t sq_width(uint32_t max);
uint64_t sq_column_size(uint32_t count, uint8_t bits);
static inline uint64_t sq_array_size(uint32_t count, unsigned bytes) {
  return ((uint64_t)count * bytes + 7) & ~UINT64_C(7);
}

bool sq_layout(const TSLanguage *, uint32_t capacity, SQLayout *);

// Packed words store their first lane in the low bits. On big-endian hosts,
// reverse byte/halfword positions within each word before a native load.
// This constant endian test folds away; fixed-width access needs no division.
static inline bool sq_little_endian(void) {
  const uint16_t one = 1;
  return *(const uint8_t *)&one != 0;
}

static inline uint8_t sq_get_u8(const uint8_t *data, uint32_t offset, uint32_t index) {
  if (!sq_little_endian()) index ^= 7;
  return data[offset + (uint64_t)index];
}

static inline void sq_set_u8(uint8_t *data, uint32_t offset, uint32_t index, uint8_t value) {
  if (!sq_little_endian()) index ^= 7;
  data[offset + (uint64_t)index] = value;
}

static inline uint16_t sq_get_u16(const uint8_t *data, uint32_t offset, uint32_t index) {
  if (!sq_little_endian()) index ^= 3;
  uint16_t value;
  memcpy(&value, data + offset + (uint64_t)index * 2, sizeof(value));
  return value;
}

static inline void sq_set_u16(uint8_t *data, uint32_t offset, uint32_t index, uint16_t value) {
  if (!sq_little_endian()) index ^= 3;
  memcpy(data + offset + (uint64_t)index * 2, &value, sizeof(value));
}

static inline uint32_t sq_get_u32(const uint8_t *data, uint32_t offset, uint32_t index) {
  if (!sq_little_endian()) index ^= 1;
  uint32_t value;
  memcpy(&value, data + offset + (uint64_t)index * 4, sizeof(value));
  return value;
}

static inline void sq_set_u32(uint8_t *data, uint32_t offset, uint32_t index, uint32_t value) {
  if (!sq_little_endian()) index ^= 1;
  memcpy(data + offset + (uint64_t)index * 4, &value, sizeof(value));
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
  case 8:
    return sq_get_u8(data, offset, index);
  case 16:
    return sq_get_u16(data, offset, index);
  case 32:
    return sq_get_u32(data, offset, index);
  }

  uint32_t lanes = 64 / bits;
  uint64_t word;
  memcpy(&word, data + offset + (uint64_t)(index / lanes) * 8, sizeof(word));
  return (uint32_t)((word >> (index % lanes * bits)) & ((UINT64_C(1) << bits) - 1));
}

void sq_set_packed(uint8_t *, uint32_t offset, uint32_t index, uint8_t bits, uint32_t);

static inline uint32_t sq_group_waste(const SQTree *tree, uint32_t group) {
  return sq_get_packed(tree->data, tree->layout.waste, group, SQ_WASTE_BITS);
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

#if SQ_INCLUDE_POINTS
static inline uint32_t sq_group_start_row_base(const SQTree *tree, uint32_t group) {
  return sq_get_u32(tree->data, tree->layout.start_row_base, group);
}

static inline uint32_t sq_group_end_row_base(const SQTree *tree, uint32_t group) {
  return sq_get_u32(tree->data, tree->layout.end_row_base, group);
}

static inline uint32_t sq_group_start_column_base(const SQTree *tree, uint32_t group) {
  return sq_get_u32(tree->data, tree->layout.start_column_base, group);
}

static inline uint32_t sq_group_end_column_base(const SQTree *tree, uint32_t group) {
  return sq_get_u32(tree->data, tree->layout.end_column_base, group);
}
#endif

static inline uint32_t sq_node_last_flag(SQNode node) {
  return sq_get_bit(node.tree->data, node.tree->layout.last, node.slot);
}

static inline uint32_t sq_node_extra_flag(SQNode node) {
  return sq_get_bit(node.tree->data, node.tree->layout.extra, node.slot);
}

static inline uint32_t sq_node_error_flag(SQNode node) {
  return sq_get_bit(node.tree->data, node.tree->layout.error, node.slot);
}

static inline uint32_t sq_node_missing_flag(SQNode node) {
  return sq_get_bit(node.tree->data, node.tree->layout.missing, node.slot);
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

#if SQ_INCLUDE_POINTS
static inline uint32_t sq_node_start_row_delta(SQNode node) {
  return sq_get_u8(node.tree->data, node.tree->layout.start_row_delta, node.slot);
}

static inline uint32_t sq_node_end_row_delta(SQNode node) {
  return sq_get_u8(node.tree->data, node.tree->layout.end_row_delta, node.slot);
}

static inline uint32_t sq_node_start_column_delta(SQNode node) {
  return sq_get_u8(node.tree->data, node.tree->layout.start_column_delta, node.slot);
}

static inline uint32_t sq_node_end_column_delta(SQNode node) {
  return sq_get_u8(node.tree->data, node.tree->layout.end_column_delta, node.slot);
}
#endif

static inline uint32_t sq_node_supertype(SQNode node) {
  return sq_get_u8(node.tree->data, node.tree->layout.supertype, node.slot);
}

static inline uint32_t sq_node_symbol_id(SQNode node) {
  return sq_get_packed(node.tree->data, node.tree->layout.symbol, node.slot,
                       node.tree->layout.symbol_bits);
}

static inline uint32_t sq_node_grammar_id(SQNode node) {
  return sq_get_packed(node.tree->data, node.tree->layout.grammar_symbol, node.slot,
                       node.tree->layout.symbol_bits);
}

static inline uint32_t sq_node_field_value(SQNode node) {
  return sq_get_packed(node.tree->data, node.tree->layout.field, node.slot,
                       node.tree->layout.field_bits);
}

// Unpack native-endian, non-straddling fields of 1..16 bits. The caller
// provides count u16 outputs and enough complete packed words for the range.
// Kernels: 0 automatic, 1 scalar, 2 portable SWAR, 3 BMI2, 4 AVX2.
typedef void (*SQUnpack)(const uint8_t *, uint32_t first, uint32_t count, uint8_t bits,
                         uint16_t *out);
void sq_unpack_u16_scalar(const uint8_t *, uint32_t, uint32_t, uint8_t, uint16_t *);
void sq_unpack_u16_swar(const uint8_t *, uint32_t, uint32_t, uint8_t, uint16_t *);
bool sq_unpack_supported(unsigned kernel);
SQUnpack sq_unpack_select(unsigned kernel);
#ifndef SQ_UNPACK_KERNEL
#define SQ_UNPACK_KERNEL 0
#endif

// Reconstruct one group's 8- or 16-bit coordinate deltas into absolute u32s.
// Kernels: 0 automatic, 1 scalar, 2 SSE2, 4 AVX2 (portable fallback elsewhere).
// Base arithmetic is unsigned, matching the ordinary node accessors.
typedef void (*SQUnpackCoordinates)(const uint8_t *, uint32_t first, uint32_t count, uint8_t bits,
                                    uint32_t base, bool subtract, uint32_t *out);
SQUnpackCoordinates sq_unpack_coordinates_select(unsigned kernel);
#ifndef SQ_COORDINATE_KERNEL
#define SQ_COORDINATE_KERNEL 0
#endif

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
SQTree *sq_allocate(const TSLanguage *, uint32_t, SQError *);

// Builder operations may move a colocated descriptor. Refresh the caller's
// pointer before reading it again; finalized public trees never move.
size_t sq_runtime_size(const TSLanguage *);
SQTree *sq_allocate_loaded(const TSLanguage *, uint32_t, const void *, uint32_t, bool borrowed,
                           SQError *);
bool sq_resize(SQTree **, uint32_t, SQError *);
bool sq_grow_data(SQTree **, uint32_t, SQError *);
bool sq_build_presence(SQTree **, SQError *);
bool sq_append_dictionary(SQTree **, const uint64_t *, uint32_t, SQError *);
uint64_t sq_presence_size(const SQTree *);
static inline uint32_t sq_presence_offset(const SQTree *tree) {
  return sq_header(tree)->format_flags & SQ_PRESENCE ? tree->layout.end : 0;
}

static inline uint32_t sq_dictionary_offset(const SQTree *tree) {
  return sq_header(tree)->supertype_dictionary_count
             ? tree->layout.end + (sq_presence_offset(tree) ? (uint32_t)sq_presence_size(tree) : 0)
             : 0;
}

static inline void sq_fail(SQError *error, SQError value) {
  if (error) {
    *error = value;
  }
}
#endif
