#include "internal.h"

SQNode sq_null(void) {
  return (SQNode){NULL, 0};
}
bool sq_node_is_null(SQNode node) {
  return !node.tree;
}
bool sq_node_eq(SQNode left, SQNode right) {
  return left.tree == right.tree && (!left.tree || left.slot == right.slot);
}
uint32_t sq_next_slot(const SQTree *tree, uint32_t slot) {
  if (slot >= sq_tree_slot_count(tree)) {
    return sq_tree_slot_count(tree);
  }
  uint32_t first =
      slot / SQ_GROUP_SIZE * SQ_GROUP_SIZE + sq_group_get(tree, G_WASTE, slot / SQ_GROUP_SIZE);
  return slot < first ? first : slot;
}
SQNode sq_tree_node_at_slot(const SQTree *tree, uint32_t slot) {
  if (!tree || slot >= sq_tree_slot_count(tree) || sq_next_slot(tree, slot) != slot) {
    return sq_null();
  }
  return (SQNode){tree, slot};
}
SQNode sq_tree_root_node(const SQTree *tree) {
  return tree && sq_tree_group_count(tree) ? (SQNode){tree, sq_group_get(tree, G_WASTE, 0)}
                                           : sq_null();
}
uint32_t sq_node_end_slot(SQNode node) {
  return node.slot + 1 + sq_group_get(node.tree, G_SPAN, node.slot / SQ_GROUP_SIZE) +
         sq_node_get(node, N_SPAN);
}
static TSSymbol raw_symbol(SQNode node) {
  return sq_decode_symbol(node.tree, sq_node_get(node, N_SYMBOL));
}
TSSymbol sq_node_symbol(SQNode node) {
  return node.tree ? ts_language_public_symbol(node.tree->language, raw_symbol(node)) : 0;
}
TSSymbol sq_node_grammar_symbol(SQNode node) {
  return node.tree ? sq_decode_symbol(node.tree, sq_node_get(node, N_GRAMMAR)) : 0;
}
const char *sq_node_type(SQNode node) {
  return node.tree ? ts_language_symbol_name(node.tree->language, raw_symbol(node)) : NULL;
}
const char *sq_node_grammar_type(SQNode node) {
  return node.tree ? ts_language_symbol_name(node.tree->language, sq_node_grammar_symbol(node))
                   : NULL;
}
static uint32_t coordinate(SQNode node, unsigned group_col, unsigned node_col, bool subtract) {
  if (!node.tree) {
    return 0;
  }
  uint32_t base = sq_group_get(node.tree, group_col, node.slot / SQ_GROUP_SIZE),
           delta = sq_node_get(node, node_col);
  return subtract ? base - delta : base + delta;
}
uint32_t sq_node_start_byte(SQNode node) {
  return coordinate(node, G_BYTE, N_BYTE, false);
}
uint32_t sq_node_end_byte(SQNode node) {
  return coordinate(node, G_END_BYTE, N_END_BYTE, true);
}
TSPoint sq_node_start_point(SQNode node) {
  return (TSPoint){coordinate(node, G_ROW, N_ROW, false), coordinate(node, G_COL, N_COL, false)};
}
TSPoint sq_node_end_point(SQNode node) {
  return (TSPoint){coordinate(node, G_END_ROW, N_END_ROW, true),
                   coordinate(node, G_END_COL, N_END_COL, true)};
}
bool sq_node_is_named(SQNode node) {
  return node.tree && ts_language_symbol_metadata(node.tree->language, raw_symbol(node)).named;
}
bool sq_node_is_extra(SQNode node) {
  return node.tree && sq_node_get(node, N_EXTRA);
}
bool sq_node_is_missing(SQNode node) {
  return node.tree && sq_node_get(node, N_MISSING);
}
bool sq_node_is_error(SQNode node) {
  return node.tree && sq_node_symbol(node) == ts_builtin_sym_error;
}
bool sq_node_has_error(SQNode node) {
  return node.tree && sq_node_get(node, N_ERROR);
}
bool sq_node_has_changes(SQNode node) {
  (void)node;
  return false;
}
TSFieldId sq_node_field_id(SQNode node) {
  return node.tree ? (TSFieldId)sq_node_get(node, N_FIELD) : 0;
}
const char *sq_node_field_name(SQNode node) {
  return node.tree ? ts_language_field_name_for_id(node.tree->language, sq_node_field_id(node))
                   : NULL;
}
bool sq_node_has_supertype(SQNode node, TSSymbol symbol) {
  if (!node.tree) {
    return false;
  }
  const SQTree *tree = node.tree;
  for (uint32_t i = 0; i < tree->supertype_count; i++) {
    if (tree->supertypes[i] != symbol) {
      continue;
    }
    uint32_t value = sq_node_get(node, N_SUPER);
    if (tree->supertype_count <= 8) {
      return (value >> i) & 1;
    }
    uint32_t words = (tree->supertype_count + 63) / 64;
    uint64_t word;
    memcpy(&word,
           tree->data + sq_header(tree)->supertype_dictionary_byte_offset +
               ((size_t)value * words + i / 64) * 8,
           8);
    return (word >> (i % 64)) & 1;
  }
  return false;
}
uint32_t sq_node_descendant_count(SQNode node) {
  if (!node.tree) {
    return 0;
  }
  uint32_t end = sq_node_end_slot(node), result = end - node.slot;
  for (uint32_t group = node.slot / SQ_GROUP_SIZE + 1; group * SQ_GROUP_SIZE < end; group++) {
    result -= sq_group_get(node.tree, G_WASTE, group);
  }
  return result;
}
SQNode sq_node_next_preorder(SQNode node) {
  return node.tree ? sq_tree_node_at_slot(node.tree, sq_next_slot(node.tree, node.slot + 1))
                   : sq_null();
}
SQNode sq_node_prev_preorder(SQNode node) {
  if (!node.tree || node.slot == sq_group_get(node.tree, G_WASTE, 0)) {
    return sq_null();
  }
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  uint32_t first = group * SQ_GROUP_SIZE + sq_group_get(node.tree, G_WASTE, group);
  return (SQNode){node.tree, node.slot == first ? group * SQ_GROUP_SIZE - 1 : node.slot - 1};
}
static SQNode first_child(SQNode node) {
  if (!node.tree) {
    return sq_null();
  }
  uint32_t slot = sq_next_slot(node.tree, node.slot + 1);
  return slot < sq_node_end_slot(node) ? (SQNode){node.tree, slot} : sq_null();
}
SQNode sq_node_next_sibling_including_empty(SQNode node) {
  if (!node.tree || sq_node_get(node, N_LAST)) {
    return sq_null();
  }
  return sq_tree_node_at_slot(node.tree, sq_node_end_slot(node));
}
SQNode sq_node_parent(SQNode node) {
  if (!node.tree || node.slot == sq_tree_root_node(node.tree).slot) {
    return sq_null();
  }
  // In preorder, the nearest earlier node whose subtree reaches us is our
  // parent. Starting nearby avoids rescanning every preceding root sibling.
  // A group's shared span base plus the u8 maximum bounds all its subtree ends,
  // so distant groups of small subtrees can be rejected with one header read.
  uint32_t slot = node.slot - 1;
  for (;;) {
    uint32_t group = slot / SQ_GROUP_SIZE;
    uint32_t base = sq_group_get(node.tree, G_SPAN, group);
    if ((uint64_t)slot + 1 + base + UINT8_MAX > node.slot) {
      uint32_t first = group * SQ_GROUP_SIZE + sq_group_get(node.tree, G_WASTE, group);
      while (slot >= first) {
        SQNode candidate = {node.tree, slot};
        if ((uint64_t)slot + 1 + base + sq_node_get(candidate, N_SPAN) > node.slot) {
          return candidate;
        }
        if (slot == first) {
          break;
        }
        slot--;
      }
    }
    if (!group) {
      return sq_null();
    }
    slot = group * SQ_GROUP_SIZE - 1;
  }
}
SQNode sq_node_prev_sibling(SQNode node) {
  SQNode child = first_child(sq_node_parent(node)), previous = sq_null();
  for (; child.tree && child.slot != node.slot;
       child = sq_node_next_sibling_including_empty(child)) {
    previous = child;
  }
  return previous;
}
SQNode sq_node_next_sibling(SQNode node) {
  uint32_t end_byte = sq_node_end_byte(node);
  do {
    node = sq_node_next_sibling_including_empty(node);
  } while (node.tree && sq_node_end_byte(node) <= end_byte);
  return node;
}
SQNode sq_node_next_named_sibling(SQNode node) {
  uint32_t end_byte = sq_node_end_byte(node);
  do {
    node = sq_node_next_sibling_including_empty(node);
  } while (node.tree && (sq_node_end_byte(node) <= end_byte || !sq_node_is_named(node)));
  return node;
}

SQNode sq_node_prev_named_sibling(SQNode node) {
  SQNode child = first_child(sq_node_parent(node)), previous = sq_null();
  for (; child.tree && child.slot != node.slot;
       child = sq_node_next_sibling_including_empty(child)) {
    if (sq_node_is_named(child)) {
      previous = child;
    }
  }
  return previous;
}
static SQNode child_at(SQNode node, uint32_t index, bool named) {
  for (SQNode child = first_child(node); child.tree;
       child = sq_node_next_sibling_including_empty(child)) {
    if (!named || sq_node_is_named(child)) {
      if (!index) {
        return child;
      }
      index--;
    }
  }
  return sq_null();
}
static uint32_t child_count(SQNode node, bool named) {
  uint32_t count = 0;
  for (SQNode child = first_child(node); child.tree;
       child = sq_node_next_sibling_including_empty(child)) {
    count += !named || sq_node_is_named(child);
  }
  return count;
}
SQNode sq_node_child(SQNode node, uint32_t i) {
  return child_at(node, i, false);
}
SQNode sq_node_named_child(SQNode node, uint32_t i) {
  return child_at(node, i, true);
}
uint32_t sq_node_child_count(SQNode node) {
  return child_count(node, false);
}
uint32_t sq_node_named_child_count(SQNode node) {
  return child_count(node, true);
}
SQNode sq_node_child_by_field_id(SQNode node, TSFieldId field) {
  SQNode exceptional;
  if (node.tree && sq_lookup_field_exception(node, field, &exceptional)) {
    return exceptional;
  }
  // ERROR productions have no field map. Hidden children can still contribute
  // field names to enumeration, but mainline's field lookup stops at ERROR.
  if (sq_node_is_error(node)) {
    return sq_null();
  }
  if (field) {
    for (SQNode child = first_child(node); child.tree;
         child = sq_node_next_sibling_including_empty(child)) {
      if (sq_node_field_id(child) == field) {
        return child;
      }
    }
  }
  return sq_null();
}
SQNode sq_node_child_by_field_name(SQNode node, const char *name, uint32_t length) {
  return node.tree && name ? sq_node_child_by_field_id(node, ts_language_field_id_for_name(
                                                                 node.tree->language, name, length))
                           : sq_null();
}
const char *sq_node_field_name_for_child(SQNode node, uint32_t i) {
  return sq_node_field_name(sq_node_child(node, i));
}
const char *sq_node_field_name_for_named_child(SQNode node, uint32_t i) {
  return sq_node_field_name(sq_node_named_child(node, i));
}
SQNode sq_node_child_with_descendant(SQNode node, SQNode descendant) {
  if (!node.tree || node.tree != descendant.tree || descendant.slot <= node.slot ||
      descendant.slot >= sq_node_end_slot(node)) {
    return sq_null();
  }
  for (SQNode child = first_child(node); child.tree;
       child = sq_node_next_sibling_including_empty(child)) {
    if (sq_node_end_slot(child) > descendant.slot) {
      return child;
    }
  }
  return sq_null();
}
static SQNode first_for_byte(SQNode node, uint32_t byte, bool named) {
  for (SQNode child = first_child(node); child.tree;
       child = sq_node_next_sibling_including_empty(child)) {
    if (sq_node_end_byte(child) > byte && (!named || sq_node_is_named(child))) {
      return child;
    }
  }
  return sq_null();
}
SQNode sq_node_first_child_for_byte(SQNode node, uint32_t right) {
  return first_for_byte(node, right, false);
}
SQNode sq_node_first_named_child_for_byte(SQNode node, uint32_t right) {
  return first_for_byte(node, right, true);
}
static int point_cmp(TSPoint left, TSPoint right) {
  return left.row != right.row ? (left.row > right.row ? 1 : -1)
                               : (left.column > right.column) - (left.column < right.column);
}
static SQNode seek(SQNode node, TSPoint start, TSPoint end, bool named, bool bytes) {
  if (!node.tree || point_cmp(start, end) > 0) {
    return sq_null();
  }
  SQNode result = node;
  for (;;) {
    SQNode found = sq_null();
    for (SQNode child = first_child(node); child.tree;
         child = sq_node_next_sibling_including_empty(child)) {
      TSPoint child_start =
          bytes ? (TSPoint){0, sq_node_start_byte(child)} : sq_node_start_point(child);
      TSPoint child_end = bytes ? (TSPoint){0, sq_node_end_byte(child)} : sq_node_end_point(child);
      if (point_cmp(child_end, end) < 0) {
        continue;
      }
      int past = point_cmp(child_end, start);
      if (point_cmp(child_start, child_end) == 0 ? past < 0 : past <= 0) {
        continue;
      }
      if (point_cmp(start, child_start) < 0) {
        break;
      }
      found = child;
      break;
    }
    if (!found.tree) {
      return result;
    }
    node = found;
    if (!named || sq_node_is_named(node)) {
      result = node;
    }
  }
}
SQNode sq_node_descendant_for_byte_range(SQNode node, uint32_t left, uint32_t right) {
  return seek(node, (TSPoint){0, left}, (TSPoint){0, right}, false, true);
}
SQNode sq_node_named_descendant_for_byte_range(SQNode node, uint32_t left, uint32_t right) {
  return seek(node, (TSPoint){0, left}, (TSPoint){0, right}, true, true);
}
SQNode sq_node_descendant_for_point_range(SQNode node, TSPoint left, TSPoint right) {
  return seek(node, left, right, false, false);
}
SQNode sq_node_named_descendant_for_point_range(SQNode node, TSPoint left, TSPoint right) {
  return seek(node, left, right, true, false);
}

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

SQCursor *sq_cursor_new(SQNode node) {
  if (!node.tree) {
    return NULL;
  }
  SQCursor *cursor = calloc(1, sizeof(*cursor));
  if (cursor) {
    cursor->node = node;
  }
  return cursor;
}

void sq_cursor_delete(SQCursor *cursor) {
  if (!cursor) {
    return;
  }
  for (uint32_t i = 0; i < cursor->depth; i++) {
    free(cursor->parents[i].children);
  }
  free(cursor->parents);
  free(cursor);
}

SQNode sq_cursor_node(const SQCursor *cursor) {
  return cursor ? cursor->node : sq_null();
}

SQNode sq_cursor_parent_node(const SQCursor *cursor) {
  return cursor && cursor->depth ? cursor->parents[cursor->depth - 1].parent : sq_null();
}

uint32_t sq_cursor_depth(const SQCursor *cursor) {
  return cursor ? cursor->depth : 0;
}

static bool remember_child(CursorFrame *frame, uint32_t slot) {
  if (frame->count == frame->capacity) {
    uint64_t capacity = frame->capacity ? (uint64_t)frame->capacity * 2 : 8;
    if (capacity > UINT32_MAX || capacity * sizeof(uint32_t) > SIZE_MAX) {
      return false;
    }
    uint32_t *children = realloc(frame->children, (size_t)capacity * sizeof(uint32_t));
    if (!children) {
      return false;
    }
    frame->children = children;
    frame->capacity = (uint32_t)capacity;
  }
  frame->children[frame->count++] = slot;
  return true;
}

static bool cursor_down(SQCursor *cursor, bool last) {
  if (!cursor) {
    return false;
  }
  SQNode child = first_child(cursor->node);
  if (!child.tree) {
    return false;
  }
  if (cursor->depth == cursor->capacity) {
    uint64_t capacity = cursor->capacity ? (uint64_t)cursor->capacity * 2 : 16;
    if (capacity > UINT32_MAX || capacity * sizeof(CursorFrame) > SIZE_MAX) {
      return false;
    }
    CursorFrame *parents = realloc(cursor->parents, (size_t)capacity * sizeof(CursorFrame));
    if (!parents) {
      return false;
    }
    cursor->parents = parents;
    cursor->capacity = (uint32_t)capacity;
  }
  CursorFrame frame = {.parent = cursor->node};
  for (;;) {
    if (!remember_child(&frame, child.slot)) {
      free(frame.children);
      return false;
    }
    if (!last || sq_node_get(child, N_LAST)) {
      break;
    }
    child = sq_node_next_sibling_including_empty(child);
  }
  frame.index = frame.count - 1;
  cursor->parents[cursor->depth++] = frame;
  cursor->node = child;
  return true;
}

bool sq_cursor_goto_first_child(SQCursor *cursor) {
  return cursor_down(cursor, false);
}

bool sq_cursor_goto_last_child(SQCursor *cursor) {
  return cursor_down(cursor, true);
}

bool sq_cursor_goto_next_sibling(SQCursor *cursor) {
  if (!cursor || !cursor->depth) {
    return false;
  }
  CursorFrame *frame = &cursor->parents[cursor->depth - 1];
  if (frame->index + 1 == frame->count) {
    SQNode next = sq_node_next_sibling_including_empty(cursor->node);
    if (!next.tree || !remember_child(frame, next.slot)) {
      return false;
    }
  }
  cursor->node.slot = frame->children[++frame->index];
  return true;
}

bool sq_cursor_goto_previous_sibling(SQCursor *cursor) {
  if (!cursor || !cursor->depth) {
    return false;
  }
  CursorFrame *frame = &cursor->parents[cursor->depth - 1];
  if (!frame->index) {
    return false;
  }
  // The format has no backward subtree span. Cache only sibling slots in the
  // open cursor frames so reverse walks do not rescan a wide parent quadratically.
  cursor->node.slot = frame->children[--frame->index];
  return true;
}

bool sq_cursor_goto_parent(SQCursor *cursor) {
  if (!cursor || !cursor->depth) {
    return false;
  }
  CursorFrame *frame = &cursor->parents[--cursor->depth];
  cursor->node = frame->parent;
  free(frame->children);
  return true;
}
