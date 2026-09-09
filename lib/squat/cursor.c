#include "internal.h"

typedef struct {
  SQNode parent;
  uint32_t *children;
  uint32_t count;
  uint32_t capacity;
  uint32_t index;
} CursorFrame;

struct SQCursor {
  SQNode node;
  CursorFrame *parents;
  uint32_t depth;
  uint32_t capacity;
};

typedef struct {
  uint32_t group;
  uint32_t values[SQ_GROUP_SIZE];
} DecodedColumn;

struct SQCachedCursor {
  SQNode node;
  CursorFrame *parents;
  uint32_t depth;
  uint32_t capacity;
  // Columns are lazy and independent: navigation need not unpack coordinates,
  // and reading one ancestor's span need not evict a descendant's symbols.
  DecodedColumn columns[N_COLUMNS];
  uint32_t base_group;
  uint32_t bases[G_COLUMNS];
};

static void cached_init(SQCachedCursor *cursor) {
  cursor->base_group = SQ_NONE;
  for (unsigned column = 0; column < N_COLUMNS; column++) {
    cursor->columns[column].group = SQ_NONE;
  }
}

static uint32_t cached_group(SQCachedCursor *cursor, SQNode node, unsigned column) {
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  if (cursor->base_group != group) {
    for (unsigned i = 0; i < G_COLUMNS; i++) {
      cursor->bases[i] = sq_group_get(node.tree, i, group);
    }
    cursor->base_group = group;
  }
  return cursor->bases[column];
}

void sq_decode_group(const SQTree *tree, uint32_t group, unsigned column,
                     uint32_t values[SQ_GROUP_SIZE]) {
  const SQHeader *header = sq_header(tree);
  uint32_t first = (header->group_capacity - header->group_count + group) * SQ_GROUP_SIZE;
  uint8_t bits = sq_node_width(&tree->layout, column);
  uint32_t lanes = 64 / bits;
  uint64_t mask = (UINT64_C(1) << bits) - 1;
  uint32_t offset = 0;
  // Groups need not start on word boundaries (e.g. seven 9-bit lanes).
  // Load each word once, discard its unused tail bits, and stop at the group
  // boundary even when the last word also contains the next group.
  while (offset < SQ_GROUP_SIZE) {
    uint32_t index = first + offset;
    uint32_t lane = index % lanes;
    uint32_t count = lanes - lane;
    if (count > SQ_GROUP_SIZE - offset) {
      count = SQ_GROUP_SIZE - offset;
    }
    uint64_t word;
    memcpy(&word, tree->data + tree->layout.nodes[column] + (uint64_t)(index / lanes) * 8, 8);
    word >>= lane * bits;
    for (uint32_t i = 0; i < count; i++) {
      values[offset++] = (uint32_t)(word & mask);
      word >>= bits;
    }
  }
}

static uint32_t cached_get(SQCachedCursor *cursor, SQNode node, unsigned column) {
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  DecodedColumn *decoded = &cursor->columns[column];
  if (decoded->group != group) {
    sq_decode_group(node.tree, group, column, decoded->values);
    decoded->group = group;
  }
  return decoded->values[node.slot % SQ_GROUP_SIZE];
}

#define CURSOR_TYPE SQCursor
#define CURSOR_FN(name) sq_cursor_##name
#define CURSOR_INIT(cursor) ((void)(cursor))
#define CURSOR_GET(cursor, node, column) ((void)(cursor), sq_node_get(node, column))
#define CURSOR_GROUP(cursor, node, column)                                                         \
  ((void)(cursor), sq_group_get((node).tree, column, (node).slot / SQ_GROUP_SIZE))
#include "cursor_impl.h"
#undef CURSOR_TYPE
#undef CURSOR_FN
#undef CURSOR_INIT
#undef CURSOR_GET
#undef CURSOR_GROUP

#define CURSOR_TYPE SQCachedCursor
#define CURSOR_FN(name) sq_cached_cursor_##name
#define CURSOR_INIT(cursor) cached_init(cursor)
#define CURSOR_GET(cursor, node, column) cached_get(cursor, node, column)
#define CURSOR_GROUP(cursor, node, column) cached_group(cursor, node, column)
#include "cursor_impl.h"
