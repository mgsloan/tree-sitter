#ifndef SQUAT_INTERNAL_H_
#define SQUAT_INTERNAL_H_
#include "include/tree_sitter/squat.h"
#include "../src/language.h"
#include <limits.h>
#include <stddef.h>
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
  (UINT32_C(0x535100c0) |                                                                          \
   (SQ_GROUP_SIZE == 32   ? 2u                                                                     \
    : SQ_GROUP_SIZE == 64 ? 4u                                                                     \
                          : 0u) |                                                                  \
   (SQ_COLUMN_ALIGNMENT == 64 ? 8u : 0u))

// Version 12: measured ID width rounding and compact direct supertype masks.
#define SQ_NO_POINTS 0x100u
#define SQ_PRESENCE 0x200u
#define SQ_WIDE_SUPERTYPES 0x400u
#define SQ_GRAMMAR_OVERRIDES 0x800u
#define SQ_NONE UINT32_MAX

typedef struct {
  uint32_t format_flags;
  uint32_t group_count, group_capacity;
  uint32_t supertype_dictionary_count;
} SQHeader;

_Static_assert(sizeof(SQHeader) == 16, "slab header size");

#define SQ_WASTE_BITS (SQ_GROUP_SIZE == 16 ? 4u : SQ_GROUP_SIZE == 32 ? 5u : 6u)

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
  uint32_t extra;
  uint32_t error;
  uint32_t missing;
  uint32_t start_point_base;
  uint32_t start_point;
  uint32_t end_point_base;
  uint32_t end_point;
  uint32_t end;
  uint8_t symbol_bits, field_bits, supertype_bits;
  // Grammar-wide decoder constants; runtime-only, never serialized.
  uint8_t symbol_lanes, field_lanes;
  uint32_t symbol_mask, field_mask;
} SQLayout;

typedef struct SQSupertypeGrammar {
  const TSLanguage *language;
  uint64_t *masks;
  uint32_t *table;
  uint32_t count, words, supertype_count, table_capacity;
  // Only accessed under the cache lock.
  uint32_t references;
  struct SQSupertypeGrammar *next;
} SQSupertypeGrammar;

SQSupertypeGrammar *sq_supertype_grammar_acquire(const TSLanguage *, uint32_t, SQError *);
SQSupertypeGrammar *sq_supertype_grammar_acquire_cached(const TSLanguage *, uint32_t,
                                                        const void *, size_t, SQError *);
void sq_supertype_grammar_release(SQSupertypeGrammar *);
uint32_t sq_supertype_mask_id(const SQSupertypeGrammar *, const uint64_t *);
size_t sq_supertype_grammar_cache_size(const SQSupertypeGrammar *);
bool sq_supertype_grammar_copy_cache(const SQSupertypeGrammar *, void *, size_t, SQError *);

typedef enum { SQ_STORAGE_COLOCATED, SQ_STORAGE_BORROWED } SQStorage;
struct SQTree {
  const TSLanguage *language;
  uint8_t *data;
  uint32_t size;
  SQLayout layout;

  // Sorted original grammar IDs, including supertype metadata in older ABIs.
  TSSymbol *supertypes;
  uint32_t supertype_count;
  SQStorage storage;
  SQSupertypeGrammar *supertype_grammar;
};

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
uint8_t sq_field_width(uint32_t max);
uint8_t sq_symbol_width(uint32_t max);
uint64_t sq_column_size(uint32_t count, uint8_t bits);
static inline uint64_t sq_array_size(uint32_t count, unsigned bytes) {
  return ((uint64_t)count * bytes + 7) & ~UINT64_C(7);
}

bool sq_layout(const TSLanguage *, uint32_t capacity, bool wide_supertypes, bool points,
               SQLayout *);

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

// Variable-width IDs reuse constants computed when the tree layout is created.
// Keep fixed-width loads and constant-width callers on their existing paths.
static inline uint32_t sq_get_packed_cached(const uint8_t *data, uint32_t offset,
                                           uint32_t index, uint8_t bits,
                                           uint8_t lanes, uint32_t mask) {
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

  uint64_t word = sq_get_u64(data, offset, index / lanes);
  return (uint32_t)(word >> (index % lanes * bits)) & mask;
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

static inline uint32_t sq_node_start_point_key(SQNode node) {
  return sq_get_u16(node.tree->data, node.tree->layout.start_point, node.slot);
}

static inline uint32_t sq_node_end_point_key(SQNode node) {
  return sq_get_u16(node.tree->data, node.tree->layout.end_point, node.slot);
}

static inline uint32_t sq_node_supertype(SQNode node) {
  if (!node.tree->layout.supertype_bits) return 0;
  return sq_get_packed(node.tree->data, node.tree->layout.supertype, node.slot,
                       node.tree->layout.supertype_bits);
}

static inline uint32_t sq_node_symbol_id(SQNode node) {
  return sq_get_packed_cached(node.tree->data, node.tree->layout.symbol, node.slot,
                              node.tree->layout.symbol_bits, node.tree->layout.symbol_lanes,
                              node.tree->layout.symbol_mask);
}

uint32_t sq_node_grammar_id(SQNode);
uint32_t sq_node_grammar_id_with_symbol(SQNode, uint32_t symbol);

static inline uint32_t sq_node_field_value(SQNode node) {
  if (!node.tree->layout.field_bits) return 0;
  return sq_get_packed_cached(node.tree->data, node.tree->layout.field, node.slot,
                              node.tree->layout.field_bits, node.tree->layout.field_lanes,
                              node.tree->layout.field_mask);
}

// Unpack little-endian, non-straddling fields of 1..16 bits. The caller
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
SQTree *sq_allocate(const TSLanguage *, uint32_t, bool points, SQError *);
bool sq_language_compatible(const TSLanguage *);
SQTree *sq_allocate_cached(const TSLanguage *, uint32_t, const TSSymbol *, uint32_t, bool points,
                           SQError *);

// Builder operations may move a colocated descriptor. Refresh the caller's
// pointer before reading it again; finalized public trees never move.
size_t sq_runtime_size(const TSLanguage *);
SQTree *sq_allocate_loaded(const TSLanguage *, uint32_t, const void *, uint32_t, bool borrowed,
                           bool points, const void *, size_t, SQError *);
bool sq_resize(SQTree **, uint32_t, SQError *);
bool sq_prepare_final(SQTree **, uint32_t capacity, uint32_t trailing_size, SQError *);
bool sq_grow_data(SQTree **, uint32_t, SQError *);
bool sq_build_presence(SQTree *, SQError *);
bool sq_build_presence_cached(SQTree *, const uint16_t *, uint8_t **, size_t *, SQError *);
uint64_t sq_presence_size(const SQTree *);
static inline uint32_t sq_presence_offset(const SQTree *tree) {
  return sq_header_get(tree, format_flags) & SQ_PRESENCE ? tree->layout.end : 0;
}

// Optional suffix after presence data. Header: count, reserved; then a live-slot
// bitmap, u32 ranks per 64 slots, and packed grammar IDs.
static inline uint32_t sq_grammar_offset(const SQTree *tree) {
  return tree->layout.end +
         (sq_presence_offset(tree) ? (uint32_t)sq_presence_size(tree) : 0);
}

static inline uint32_t sq_grammar_words(const SQTree *tree) {
  return (uint32_t)(((uint64_t)sq_tree_slot_count(tree) + 63) / 64);
}

static inline uint64_t sq_grammar_size(const SQTree *tree, uint32_t count) {
  uint32_t words = sq_grammar_words(tree);
  return 8 + (uint64_t)words * 8 + sq_array_size(words, 4) +
         sq_column_size(count, tree->layout.symbol_bits);
}

static inline void sq_fail(SQError *error, SQError value) {
  if (error) {
    *error = value;
  }
}
#endif
