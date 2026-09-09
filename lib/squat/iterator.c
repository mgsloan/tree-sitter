#include "attributes.h"

#ifndef SQ_ITERATOR_CACHE_ALL
#define SQ_ITERATOR_CACHE_ALL 0
#endif

typedef struct {
  uint32_t group;
  unsigned filled;
  SQUnpack unpack;
#if SQ_ITERATOR_CACHE_ALL
  uint32_t bases[G_COLUMNS];
  uint16_t values[N_COLUMNS][SQ_GROUP_SIZE];
#else
  uint16_t values[3][SQ_GROUP_SIZE];
#endif
} UnpackCache;

struct SQNodeIterator {
  const SQTree *tree;
  SQNode current;
  uint32_t next, end, group_end;
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
  iterator->end = sq_node_end_slot(root);
  iterator->group_end = (root.slot / SQ_GROUP_SIZE + 1) * SQ_GROUP_SIZE;
  if (unpack_cache) {
    iterator->cache = (UnpackCache *)(iterator + 1);
    iterator->cache->group = SQ_NONE;
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
  if (slot < iterator->end && slot == iterator->group_end) {
    // Slots inside a group are consecutive. Consult its waste column only
    // when crossing a boundary, rather than once per visited node.
    slot += sq_group_get(iterator->tree, G_WASTE, slot / SQ_GROUP_SIZE);
    iterator->group_end += SQ_GROUP_SIZE;
  }
  if (slot >= iterator->end) {
    iterator->current = sq_null();
    iterator->next = iterator->end;
    return iterator->current;
  }
  iterator->next = slot + 1;
  iterator->current = (SQNode){iterator->tree, slot};
  return iterator->current;
}

static uint16_t cached_value(SQNodeIterator *iterator, unsigned column) {
  SQNode node = iterator->current;
  UnpackCache *cache = iterator->cache;
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  if (cache->group != group) {
    cache->group = group;
    cache->filled = 0;
#if SQ_ITERATOR_CACHE_ALL
    for (unsigned base = G_BYTE; base < G_COLUMNS; base++) {
      cache->bases[base] = sq_group_get(node.tree, base, group);
    }
#endif
  }
#if SQ_ITERATOR_CACHE_ALL
  unsigned index = column;
#else
  unsigned index = column - N_SYMBOL;
#endif
  if (!(cache->filled & (1u << index))) {
    const SQHeader *header = sq_header(node.tree);
    uint32_t first = (header->group_capacity - header->group_count + group) * SQ_GROUP_SIZE;
    cache->unpack(node.tree->data + node.tree->layout.nodes[column], first, SQ_GROUP_SIZE,
                  sq_node_width(&node.tree->layout, column), cache->values[index]);
    cache->filled |= 1u << index;
  }
  return cache->values[index][node.slot % SQ_GROUP_SIZE];
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
#if SQ_ITERATOR_CACHE_ALL
    // Experimental full cache: coordinates/flags use the same bounded unpacker.
    // Counts still use ordinary tree scans and never evict the iterator's group.
    // One group check per snapshot; fill missing columns only on first use.
    (void)cached_value(iterator, N_SYMBOL);
    const unsigned needed = ((1u << N_COLUMNS) - 1) &
                            ~((1u << N_LAST) | (1u << N_SPAN) | (1u << N_SUPER));
    if ((iterator->cache->filled & needed) != needed) {
      for (unsigned column = N_EXTRA; column < N_COLUMNS; column++) {
        if (needed & (1u << column)) {
          (void)cached_value(iterator, column);
        }
      }
    }
    const uint32_t *base = iterator->cache->bases;
    const uint16_t (*value)[SQ_GROUP_SIZE] = iterator->cache->values;
    uint32_t lane = node.slot % SQ_GROUP_SIZE;
    out->start_byte = base[G_BYTE] + value[N_BYTE][lane];
    out->end_byte = base[G_END_BYTE] - value[N_END_BYTE][lane];
    out->start_point = (TSPoint){base[G_ROW] + value[N_ROW][lane],
                                 base[G_COL] + value[N_COL][lane]};
    out->end_point = (TSPoint){base[G_END_ROW] - value[N_END_ROW][lane],
                               base[G_END_COL] - value[N_END_COL][lane]};
    out->is_extra = value[N_EXTRA][lane];
    out->is_missing = value[N_MISSING][lane];
    out->has_error = value[N_ERROR][lane];
    sq_attributes_finish(node, value[N_SYMBOL][lane], value[N_GRAMMAR][lane],
                          value[N_FIELD][lane], out);
#else
    sq_attributes_with_ids(node, cached_value(iterator, N_SYMBOL), cached_value(iterator, N_GRAMMAR),
                            cached_value(iterator, N_FIELD), out);
#endif
  } else {
    sq_attributes_with_ids(node, sq_node_get(node, N_SYMBOL), sq_node_get(node, N_GRAMMAR),
                            (TSFieldId)sq_node_get(node, N_FIELD), out);
  }
}
