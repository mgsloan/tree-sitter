#include "attributes.h"

/* Cache modes: 0 IDs only, 1 u16 deltas, 2 absolute u32 coordinates. */
#ifndef SQ_ITERATOR_CACHE_ALL
#define SQ_ITERATOR_CACHE_ALL 2
#endif
_Static_assert(SQ_ITERATOR_CACHE_ALL >= 0 && SQ_ITERATOR_CACHE_ALL <= 2,
               "unknown iterator cache mode");

typedef struct {
  uint32_t group;
  unsigned filled;
  SQUnpack unpack;
#if SQ_ITERATOR_CACHE_ALL == 2
  SQUnpackCoordinates coordinates;
  // Coordinate order follows the stored node and group coordinate columns.
  uint32_t absolute[SQ_COORDINATES][SQ_ITERATOR_UNPACK_SLOTS];
  uint16_t values[3][SQ_ITERATOR_UNPACK_SLOTS];
#elif SQ_ITERATOR_CACHE_ALL == 1
  uint32_t base_group;
  uint32_t bases[G_COLUMNS];
  uint16_t values[N_COLUMNS][SQ_ITERATOR_UNPACK_SLOTS];
#else
  uint16_t values[3][SQ_ITERATOR_UNPACK_SLOTS];
#endif
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
#elif SQ_ITERATOR_CACHE_ALL == 1
    iterator->cache->base_group = SQ_NONE;
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
    iterator->next -= sq_group_get(iterator->tree, G_WASTE, group);
    iterator->group_start -= SQ_GROUP_SIZE;
  }
  iterator->current = (SQNode){iterator->tree, slot};
  return iterator->current;
}

/* A snapshot asks for several columns together. Check the group and filled mask
 * once, rather than repeating the same cache-hit checks for every field. */
static UnpackCache *cache_columns(SQNodeIterator *iterator, unsigned needed) {
  SQNode node = iterator->current;
  UnpackCache *cache = iterator->cache;
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  const unsigned groups_per_window = SQ_ITERATOR_UNPACK_SLOTS / SQ_GROUP_SIZE;
  uint32_t first_group = group & ~(groups_per_window - 1u);
  if (cache->group != first_group) {
    cache->group = first_group;
    cache->filled = 0;
  }
#if SQ_ITERATOR_CACHE_ALL == 1
  // Coordinates use the current group's bases even when decoded lanes span
  // several groups. Advancing bases must not discard the wider unpack window.
  if (cache->base_group != group) {
    cache->base_group = group;
    for (unsigned base = G_BYTE; base < G_COLUMNS; base++) {
      cache->bases[base] = sq_group_get(node.tree, base, group);
    }
  }
#endif
  unsigned missing = needed & ~cache->filled;
  if (missing) {
    const SQHeader *header = sq_header(node.tree);
    uint32_t first = first_group * SQ_GROUP_SIZE;
    uint32_t groups = header->group_count - first_group;
    if (groups > groups_per_window) {
      groups = groups_per_window;
    }
    // The final window can contain fewer groups. Stop at the tree's allocated
    // columns; decoding ahead may cross subtree bounds, but never tree bounds.
    uint32_t count = groups * SQ_GROUP_SIZE;
    do {
      // Enumerate only missing columns; field-only callers preserve lazy filling.
      unsigned index = (unsigned)__builtin_ctz(missing);
#if SQ_ITERATOR_CACHE_ALL == 1
      unsigned column = index;
#else
      unsigned column = N_SYMBOL + index;
#endif
#if SQ_ITERATOR_CACHE_ALL == 2
      if (index >= 3) {
        unsigned coordinate = index - 3;
        column = N_BYTE + coordinate;
        bool subtract = coordinate & 1u;
        // A wide window shares decoded storage, but bases change every group.
        // Reconstruct each group's lanes with that group's broadcast base.
        for (uint32_t offset = 0; offset < count; offset += SQ_GROUP_SIZE) {
          uint32_t base = sq_group_get(node.tree, G_BYTE + coordinate,
                                      first_group + offset / SQ_GROUP_SIZE);
          cache->coordinates(node.tree->data + node.tree->layout.nodes[column],
                             first + offset, SQ_GROUP_SIZE,
                             sq_node_width(&node.tree->layout, column), base, subtract,
                             cache->absolute[coordinate] + offset);
        }
      } else
#endif
      {
        cache->unpack(node.tree->data + node.tree->layout.nodes[column], first, count,
                      sq_node_width(&node.tree->layout, column), cache->values[index]);
      }
      missing &= missing - 1;
    } while (missing);
    cache->filled |= needed;
  }
  return cache;
}

static uint16_t cached_value(SQNodeIterator *iterator, unsigned column) {
#if SQ_ITERATOR_CACHE_ALL == 1
  unsigned index = column;
#else
  unsigned index = column - N_SYMBOL;
#endif
  UnpackCache *cache = cache_columns(iterator, 1u << index);
  uint32_t lane = iterator->current.slot & (SQ_ITERATOR_UNPACK_SLOTS - 1u);
  return cache->values[index][lane];
}
TSFieldId sq_node_iterator_field_id(SQNodeIterator *iterator) {
  if (!iterator || !iterator->current.tree) {
    return 0;
  }
  return iterator->cache ? cached_value(iterator, N_FIELD)
                         : (TSFieldId)sq_node_get(iterator->current, N_FIELD);
}
void sq_node_iterator_attributes(SQNodeIterator *iterator, SQCursorAttributes *out) {
  if (!out) {
    return;
  }
  memset(out, 0, sizeof(*out));
  if (!iterator || !iterator->current.tree) {
    return;
  }
  SQNode node = iterator->current;
  if (iterator->cache) {
#if SQ_ITERATOR_CACHE_ALL == 2
    // Filled bits 0..2 are IDs; subsequent bits are stored coordinates.
    const UnpackCache *cache = cache_columns(iterator, (1u << (3 + SQ_COORDINATES)) - 1);
    uint32_t lane = node.slot & (SQ_ITERATOR_UNPACK_SLOTS - 1u);
    out->start_byte = cache->absolute[0][lane];
    out->end_byte = cache->absolute[1][lane];
#if SQ_INCLUDE_POINTS
    out->start_point = (TSPoint){cache->absolute[2][lane], cache->absolute[4][lane]};
    out->end_point = (TSPoint){cache->absolute[3][lane], cache->absolute[5][lane]};
#endif
    // Single-bit flags remain packed; they need no widening or base arithmetic.
    out->is_extra = sq_node_get(node, N_EXTRA);
    out->is_missing = sq_node_get(node, N_MISSING);
    out->has_error = sq_node_get(node, N_ERROR);
    sq_attributes_finish(node, cache->values[0][lane], cache->values[1][lane],
                         cache->values[2][lane], out);
#elif SQ_ITERATOR_CACHE_ALL == 1
    // Experimental full cache: coordinates/flags use the same bounded unpacker.
    // Counts still use ordinary tree scans and never evict the iterator's group.
    const unsigned needed = ((1u << N_COLUMNS) - 1) &
                            ~((1u << N_LAST) | (1u << N_SPAN) | (1u << N_SUPER));
    const UnpackCache *cache = cache_columns(iterator, needed);
    const uint32_t *base = cache->bases;
    const uint16_t (*value)[SQ_ITERATOR_UNPACK_SLOTS] = cache->values;
    uint32_t lane = node.slot & (SQ_ITERATOR_UNPACK_SLOTS - 1u);
    out->start_byte = base[G_BYTE] + value[N_BYTE][lane];
    out->end_byte = base[G_END_BYTE] - value[N_END_BYTE][lane];
#if SQ_INCLUDE_POINTS
    out->start_point = (TSPoint){base[G_ROW] + value[N_ROW][lane],
                                 base[G_COL] + value[N_COL][lane]};
    out->end_point = (TSPoint){base[G_END_ROW] - value[N_END_ROW][lane],
                               base[G_END_COL] - value[N_END_COL][lane]};
#endif
    out->is_extra = value[N_EXTRA][lane];
    out->is_missing = value[N_MISSING][lane];
    out->has_error = value[N_ERROR][lane];
    sq_attributes_finish(node, value[N_SYMBOL][lane], value[N_GRAMMAR][lane],
                          value[N_FIELD][lane], out);
#else
    const UnpackCache *cache = cache_columns(iterator, (1u << 3) - 1);
    uint32_t lane = node.slot & (SQ_ITERATOR_UNPACK_SLOTS - 1u);
    sq_attributes_with_ids(node, cache->values[0][lane],
                            cache->values[N_GRAMMAR - N_SYMBOL][lane],
                            cache->values[N_FIELD - N_SYMBOL][lane], out);
#endif
  } else {
    sq_attributes_with_ids(node, sq_node_get(node, N_SYMBOL), sq_node_get(node, N_GRAMMAR),
                            (TSFieldId)sq_node_get(node, N_FIELD), out);
  }
}
