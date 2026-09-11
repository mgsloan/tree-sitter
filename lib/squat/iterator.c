#include "attributes.h"

/* Mode 0 preserves the historical ID-only cache experiment. */
#ifndef SQ_ITERATOR_CACHE_ALL
#define SQ_ITERATOR_CACHE_ALL 2
#endif
_Static_assert(SQ_ITERATOR_CACHE_ALL == 0 || SQ_ITERATOR_CACHE_ALL == 2,
               "unknown iterator cache mode");

typedef struct {
  uint32_t group;
  bool field_filled, attributes_filled;
  SQUnpack unpack;
#if SQ_ITERATOR_CACHE_ALL == 2
  SQUnpackCoordinates coordinates;
  uint32_t start_byte[SQ_ITERATOR_UNPACK_SLOTS];
  uint32_t end_byte[SQ_ITERATOR_UNPACK_SLOTS];
#if SQ_INCLUDE_POINTS
  uint32_t start_row[SQ_ITERATOR_UNPACK_SLOTS];
  uint32_t end_row[SQ_ITERATOR_UNPACK_SLOTS];
  uint32_t start_column[SQ_ITERATOR_UNPACK_SLOTS];
  uint32_t end_column[SQ_ITERATOR_UNPACK_SLOTS];
#endif
#endif
  uint16_t symbol[SQ_ITERATOR_UNPACK_SLOTS];
  uint16_t grammar_symbol[SQ_ITERATOR_UNPACK_SLOTS];
  uint16_t field[SQ_ITERATOR_UNPACK_SLOTS];
} UnpackCache;

struct SQNodeIterator {
  const SQTree *tree;
  SQNode current;
  uint32_t next, end, group_start;
  UnpackCache *cache;
};

SQNodeIterator *sq_node_iterator_new(SQNode root, bool unpack_cache) {
  if (!root.tree) {
    return NULL;
  }
  size_t bytes = sizeof(SQNodeIterator) + (unpack_cache ? sizeof(UnpackCache) : 0);
  SQNodeIterator *iterator = calloc(1, bytes);
  if (!iterator) {
    return NULL;
  }
  iterator->tree = root.tree;
  iterator->next = root.slot;
  iterator->end = sq_node_first_slot(root);
  iterator->group_start = root.slot / SQ_GROUP_SIZE * SQ_GROUP_SIZE;
  if (unpack_cache) {
    iterator->cache = (UnpackCache *)(iterator + 1);
    iterator->cache->group = SQ_NONE;
#if SQ_ITERATOR_CACHE_ALL == 2
    iterator->cache->coordinates = sq_unpack_coordinates_select(SQ_COORDINATE_KERNEL);
#endif
    iterator->cache->unpack = sq_unpack_select(SQ_UNPACK_KERNEL);
  }
  return iterator;
}
void sq_node_iterator_delete(SQNodeIterator *iterator) {
  free(iterator);
}
SQNode sq_node_iterator_node(const SQNodeIterator *iterator) {
  return iterator ? iterator->current : sq_null();
}
SQNode sq_node_iterator_next(SQNodeIterator *iterator) {
  if (!iterator) {
    return sq_null();
  }
  uint32_t slot = iterator->next;
  if (slot == SQ_NONE || slot < iterator->end) {
    iterator->current = sq_null();
    iterator->next = SQ_NONE;
    return iterator->current;
  }
  iterator->next = slot - 1;
  if (slot == iterator->group_start && slot) {
    uint32_t group = slot / SQ_GROUP_SIZE - 1;
    iterator->next -= sq_group_waste(iterator->tree, group);
    iterator->group_start -= SQ_GROUP_SIZE;
  }
  iterator->current = (SQNode){iterator->tree, slot};
  return iterator->current;
}

/* The API can request a field alone or a complete snapshot. Two fill states
 * express that directly, without a column mask or trailing-zero-bit dispatch. */
static UnpackCache *prepare_cache(SQNodeIterator *iterator) {
  UnpackCache *cache = iterator->cache;
  uint32_t group = iterator->current.slot / SQ_GROUP_SIZE;
  const unsigned groups_per_window = SQ_ITERATOR_UNPACK_SLOTS / SQ_GROUP_SIZE;
  uint32_t first_group = group & ~(groups_per_window - 1u);
  if (cache->group != first_group) {
    cache->group = first_group;
    cache->field_filled = cache->attributes_filled = false;
  }
  return cache;
}
static uint32_t cache_slot_count(const SQTree *tree, const UnpackCache *cache) {
  uint32_t groups = sq_header(tree)->group_count - cache->group;
  const unsigned groups_per_window = SQ_ITERATOR_UNPACK_SLOTS / SQ_GROUP_SIZE;
  if (groups > groups_per_window) groups = groups_per_window;
  return groups * SQ_GROUP_SIZE;
}
static void fill_field(const SQTree *tree, UnpackCache *cache) {
  if (cache->field_filled) return;
  cache->unpack(tree->data + tree->layout.field, cache->group * SQ_GROUP_SIZE,
                cache_slot_count(tree, cache), tree->layout.field_bits, cache->field);
  cache->field_filled = true;
}
#if SQ_ITERATOR_CACHE_ALL == 2
static void fill_coordinate(const SQTree *tree, const UnpackCache *cache,
                             uint32_t delta_offset, uint32_t base_offset,
                             uint8_t bits, bool subtract, uint32_t *out) {
  uint32_t count = cache_slot_count(tree, cache);
  uint32_t first = cache->group * SQ_GROUP_SIZE;
  // A wider window still changes bases every physical group. The SIMD kernel
  // widens directly to u32 and adds/subtracts a broadcast base in each lane.
  for (uint32_t offset = 0; offset < count; offset += SQ_GROUP_SIZE) {
    uint32_t base = sq_get_u32(tree->data, base_offset, cache->group + offset / SQ_GROUP_SIZE);
    cache->coordinates(tree->data + delta_offset, first + offset, SQ_GROUP_SIZE,
                        bits, base, subtract, out + offset);
  }
}
#endif
static void fill_attributes(SQNodeIterator *iterator, UnpackCache *cache) {
  SQNode node = iterator->current;
  const SQTree *tree = node.tree;
  if (cache->attributes_filled) return;
  fill_field(tree, cache);
  uint32_t first = cache->group * SQ_GROUP_SIZE;
  uint32_t count = cache_slot_count(tree, cache);
  cache->unpack(tree->data + tree->layout.symbol, first, count,
                tree->layout.symbol_bits, cache->symbol);
  cache->unpack(tree->data + tree->layout.grammar_symbol, first, count,
                tree->layout.symbol_bits, cache->grammar_symbol);
#if SQ_ITERATOR_CACHE_ALL == 2
  fill_coordinate(tree, cache, tree->layout.start_byte_delta, tree->layout.start_byte_base,
                    8, false, cache->start_byte);
  fill_coordinate(tree, cache, tree->layout.end_byte_delta, tree->layout.end_byte_base,
                    16, true, cache->end_byte);
#if SQ_INCLUDE_POINTS
  fill_coordinate(tree, cache, tree->layout.start_row_delta, tree->layout.start_row_base,
                    8, false, cache->start_row);
  fill_coordinate(tree, cache, tree->layout.end_row_delta, tree->layout.end_row_base,
                    8, true, cache->end_row);
  fill_coordinate(tree, cache, tree->layout.start_column_delta, tree->layout.start_column_base,
                    8, false, cache->start_column);
  fill_coordinate(tree, cache, tree->layout.end_column_delta, tree->layout.end_column_base,
                    8, true, cache->end_column);
#endif
#endif
  cache->attributes_filled = true;
}

TSFieldId sq_node_iterator_field_id(SQNodeIterator *iterator) {
  if (!iterator || !iterator->current.tree) return 0;
  if (!iterator->cache) return (TSFieldId)sq_node_field_value(iterator->current);
  UnpackCache *cache = prepare_cache(iterator);
  fill_field(iterator->tree, cache);
  uint32_t lane = iterator->current.slot & (SQ_ITERATOR_UNPACK_SLOTS - 1u);
  return cache->field[lane];
}
void sq_node_iterator_attributes(SQNodeIterator *iterator, SQCursorAttributes *out) {
  if (!out) return;
  memset(out, 0, sizeof(*out));
  if (!iterator || !iterator->current.tree) return;
  SQNode node = iterator->current;
  if (!iterator->cache) {
    sq_attributes_with_ids(node, sq_node_symbol_id(node), sq_node_grammar_id(node),
                            (TSFieldId)sq_node_field_value(node), out);
    return;
  }
  UnpackCache *cache = prepare_cache(iterator);
  fill_attributes(iterator, cache);
  uint32_t lane = node.slot & (SQ_ITERATOR_UNPACK_SLOTS - 1u);
#if SQ_ITERATOR_CACHE_ALL == 2
  out->start_byte = cache->start_byte[lane];
  out->end_byte = cache->end_byte[lane];
#if SQ_INCLUDE_POINTS
  out->start_point = (TSPoint){cache->start_row[lane], cache->start_column[lane]};
  out->end_point = (TSPoint){cache->end_row[lane], cache->end_column[lane]};
#endif
  out->is_extra = sq_node_extra_flag(node);
  out->is_missing = sq_node_missing_flag(node);
  out->has_error = sq_node_error_flag(node);
#else
  sq_attributes_with_ids(node, cache->symbol[lane], cache->grammar_symbol[lane],
                          cache->field[lane], out);
  return;
#endif
#if SQ_ITERATOR_CACHE_ALL != 0
  sq_attributes_finish(node, cache->symbol[lane], cache->grammar_symbol[lane],
                       cache->field[lane], out);
#endif
}
