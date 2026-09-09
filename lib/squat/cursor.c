#include "internal.h"

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
  return cursor && cursor->depth
             ? (SQNode){cursor->node.tree, cursor->parents[cursor->depth - 1]}
             : sq_null();
}

uint32_t sq_cursor_depth(const SQCursor *cursor) {
  return cursor ? cursor->depth : 0;
}

bool sq_cursor_goto_first_child(SQCursor *cursor) {
  if (!cursor) {
    return false;
  }
  SQNode parent = cursor->node;
  uint32_t slot = sq_next_slot(parent.tree, parent.slot + 1);
  if (slot >= sq_node_end_slot(parent)) {
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

void sq_cursor_attributes(SQCursor *cursor, SQCursorAttributes *out) {
  if (!out) {
    return;
  }
  memset(out, 0, sizeof(*out));
  if (!cursor) {
    return;
  }
  SQNode node = cursor->node;
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  const TSLanguage *language = node.tree->language;
  TSSymbol raw = sq_decode_symbol(node.tree, sq_node_get(node, N_SYMBOL));
  out->symbol = ts_language_public_symbol(language, raw);
  out->grammar_symbol = sq_decode_symbol(node.tree, sq_node_get(node, N_GRAMMAR));
  out->type = ts_language_symbol_name(language, raw);
  out->grammar_type = ts_language_symbol_name(language, out->grammar_symbol);
  out->start_byte = sq_group_get(node.tree, G_BYTE, group) + sq_node_get(node, N_BYTE);
  out->end_byte = sq_group_get(node.tree, G_END_BYTE, group) - sq_node_get(node, N_END_BYTE);
  out->start_point = (TSPoint){sq_group_get(node.tree, G_ROW, group) + sq_node_get(node, N_ROW),
                             sq_group_get(node.tree, G_COL, group) + sq_node_get(node, N_COL)};
  out->end_point =
      (TSPoint){sq_group_get(node.tree, G_END_ROW, group) - sq_node_get(node, N_END_ROW),
                sq_group_get(node.tree, G_END_COL, group) - sq_node_get(node, N_END_COL)};
  out->is_named = ts_language_symbol_metadata(language, raw).named;
  out->is_extra = sq_node_get(node, N_EXTRA);
  out->is_missing = sq_node_get(node, N_MISSING);
  out->is_error = out->symbol == ts_builtin_sym_error;
  out->has_error = sq_node_get(node, N_ERROR);
  out->field_id = sq_node_get(node, N_FIELD);
  out->child_count = sq_node_child_count(node);
  out->named_child_count = sq_node_named_child_count(node);
  out->descendant_count = sq_node_descendant_count(node);
}
