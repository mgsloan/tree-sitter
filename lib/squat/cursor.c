#include "attributes.h"

struct SQCursor {
  SQNode node;
  uint32_t *parents;
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
  if (cursor) {
    free(cursor->parents);
    free(cursor);
  }
}

SQNode sq_cursor_node(const SQCursor *cursor) {
  return cursor ? cursor->node : sq_null();
}

SQNode sq_cursor_parent_node(const SQCursor *cursor) {
  return cursor && cursor->depth ? (SQNode){cursor->node.tree, cursor->parents[cursor->depth - 1]}
                                 : sq_null();
}

uint32_t sq_cursor_depth(const SQCursor *cursor) {
  return cursor ? cursor->depth : 0;
}

void sq_cursor_reset(SQCursor *cursor, SQNode node) {
  if (!cursor || !node.tree) return;
  cursor->node = node;
  cursor->depth = 0;
}

bool sq_cursor_goto_first_child(SQCursor *cursor) {
  if (!cursor) {
    return false;
  }

  SQNode parent = cursor->node;
  uint32_t slot = sq_previous_slot(parent.tree, parent.slot - 1);
  if (slot == SQ_NONE || slot < sq_node_first_slot(parent)) {
    return false;
  }

  if (cursor->depth == cursor->capacity) {
    uint64_t capacity = cursor->capacity ? (uint64_t)cursor->capacity * 2 : 16;
    if (capacity > UINT32_MAX || capacity * sizeof(uint32_t) > SIZE_MAX) {
      return false;
    }

    uint32_t *parents = realloc(cursor->parents, (size_t)capacity * sizeof(uint32_t));
    if (!parents) {
      return false;
    }

    cursor->parents = parents;
    cursor->capacity = (uint32_t)capacity;
  }

  cursor->parents[cursor->depth++] = parent.slot;
  cursor->node.slot = slot;
  return true;
}

bool sq_cursor_goto_last_child(SQCursor *cursor) {
  if (!sq_cursor_goto_first_child(cursor)) {
    return false;
  }

  while (sq_cursor_goto_next_sibling(cursor)) {
  }

  return true;
}

bool sq_cursor_goto_next_sibling(SQCursor *cursor) {
  if (!cursor || !cursor->depth) {
    return false;
  }

  SQNode next = sq_node_next_sibling_including_empty(cursor->node);
  if (!next.tree) {
    return false;
  }

  cursor->node = next;
  return true;
}

bool sq_cursor_goto_parent(SQCursor *cursor) {
  if (!cursor || !cursor->depth) {
    return false;
  }

  cursor->node.slot = cursor->parents[--cursor->depth];
  return true;
}

bool sq_cursor_goto_previous_sibling(SQCursor *cursor) {
  if (!cursor || !cursor->depth) return false;
  SQNode parent = sq_cursor_parent_node(cursor);
  SQNode previous = sq_null();
  for (SQNode child = sq_node_child(parent, 0); child.tree && child.slot != cursor->node.slot;
       child = sq_node_next_sibling_including_empty(child)) {
    previous = child;
  }
  if (!previous.tree) return false;
  cursor->node = previous;
  return true;
}

static int64_t goto_first_child_for(SQCursor *cursor, uint32_t byte, TSPoint point) {
  if (!sq_cursor_goto_first_child(cursor)) return -1;
  int64_t index = 0;
  do {
    TSPoint end = sq_node_end_point(cursor->node);
    if (sq_node_end_byte(cursor->node) > byte &&
        (end.row > point.row || (end.row == point.row && end.column > point.column))) {
      return index;
    }
    index++;
  } while (sq_cursor_goto_next_sibling(cursor));
  sq_cursor_goto_parent(cursor);
  return -1;
}

int64_t sq_cursor_goto_first_child_for_byte(SQCursor *cursor, uint32_t byte) {
  return goto_first_child_for(cursor, byte, (TSPoint){0});
}

int64_t sq_cursor_goto_first_child_for_point(SQCursor *cursor, TSPoint point) {
  return goto_first_child_for(cursor, 0, point);
}

void sq_cursor_attributes(SQCursor *cursor, SQCursorAttributes *out) {
  if (!out) {
    return;
  }

  memset(out, 0, sizeof(*out));
  if (!cursor) {
    return;
  }

  SQNode node = cursor->node;
  sq_attributes_with_ids(node, sq_node_symbol_id(node), sq_node_grammar_id(node),
                         (TSFieldId)sq_node_field_value(node), out);
}
