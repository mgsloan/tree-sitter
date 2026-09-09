/* Instantiated for both cursor types in cursor.c. Keep navigation identical;
 * only packed-column access and cache initialization differ. */
static uint32_t CURSOR_FN(end_slot)(CURSOR_TYPE *cursor, SQNode node) {
  return node.slot + 1 + CURSOR_GROUP(cursor, node, G_SPAN) + CURSOR_GET(cursor, node, N_SPAN);
}
static SQNode CURSOR_FN(first_child)(CURSOR_TYPE *cursor, SQNode node) {
  uint32_t slot = sq_next_slot(node.tree, node.slot + 1);
  return slot < CURSOR_FN(end_slot)(cursor, node) ? (SQNode){node.tree, slot} : sq_null();
}
static SQNode CURSOR_FN(next_sibling)(CURSOR_TYPE *cursor, SQNode node) {
  return CURSOR_GET(cursor, node, N_LAST)
             ? sq_null()
             : sq_tree_node_at_slot(node.tree, CURSOR_FN(end_slot)(cursor, node));
}
CURSOR_TYPE *CURSOR_FN(new)(SQNode node) {
  if (!node.tree) {
    return NULL;
  }
  CURSOR_TYPE *cursor = calloc(1, sizeof(*cursor));
  if (cursor) {
    cursor->node = node;
    CURSOR_INIT(cursor);
  }
  return cursor;
}

void CURSOR_FN(delete)(CURSOR_TYPE *cursor) {
  if (!cursor) {
    return;
  }
  for (uint32_t i = 0; i < cursor->depth; i++) {
    free(cursor->parents[i].children);
  }
  free(cursor->parents);
  free(cursor);
}

SQNode CURSOR_FN(node)(const CURSOR_TYPE *cursor) {
  return cursor ? cursor->node : sq_null();
}

SQNode CURSOR_FN(parent_node)(const CURSOR_TYPE *cursor) {
  return cursor && cursor->depth ? cursor->parents[cursor->depth - 1].parent : sq_null();
}

uint32_t CURSOR_FN(depth)(const CURSOR_TYPE *cursor) {
  return cursor ? cursor->depth : 0;
}

static bool CURSOR_FN(remember_child)(CursorFrame *frame, uint32_t slot) {
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

static bool CURSOR_FN(down)(CURSOR_TYPE *cursor, bool last) {
  if (!cursor) {
    return false;
  }
  SQNode child = CURSOR_FN(first_child)(cursor, cursor->node);
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
    if (!CURSOR_FN(remember_child)(&frame, child.slot)) {
      free(frame.children);
      return false;
    }
    if (!last || CURSOR_GET(cursor, child, N_LAST)) {
      break;
    }
    child = CURSOR_FN(next_sibling)(cursor, child);
  }
  frame.index = frame.count - 1;
  cursor->parents[cursor->depth++] = frame;
  cursor->node = child;
  return true;
}

bool CURSOR_FN(goto_first_child)(CURSOR_TYPE *cursor) {
  return CURSOR_FN(down)(cursor, false);
}

bool CURSOR_FN(goto_last_child)(CURSOR_TYPE *cursor) {
  return CURSOR_FN(down)(cursor, true);
}

bool CURSOR_FN(goto_next_sibling)(CURSOR_TYPE *cursor) {
  if (!cursor || !cursor->depth) {
    return false;
  }
  CursorFrame *frame = &cursor->parents[cursor->depth - 1];
  if (frame->index + 1 == frame->count) {
    SQNode next = CURSOR_FN(next_sibling)(cursor, cursor->node);
    if (!next.tree || !CURSOR_FN(remember_child)(frame, next.slot)) {
      return false;
    }
  }
  cursor->node.slot = frame->children[++frame->index];
  return true;
}

bool CURSOR_FN(goto_previous_sibling)(CURSOR_TYPE *cursor) {
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

bool CURSOR_FN(goto_parent)(CURSOR_TYPE *cursor) {
  if (!cursor || !cursor->depth) {
    return false;
  }
  CursorFrame *frame = &cursor->parents[--cursor->depth];
  cursor->node = frame->parent;
  free(frame->children);
  return true;
}

void CURSOR_FN(attributes)(CURSOR_TYPE *cursor, SQCursorAttributes *out) {
  if (!out) {
    return;
  }
  memset(out, 0, sizeof(*out));
  if (!cursor) {
    return;
  }
  SQNode node = cursor->node;
  const TSLanguage *language = node.tree->language;
  TSSymbol raw = sq_decode_symbol(node.tree, CURSOR_GET(cursor, node, N_SYMBOL));
  out->symbol = ts_language_public_symbol(language, raw);
  out->grammar_symbol = sq_decode_symbol(node.tree, CURSOR_GET(cursor, node, N_GRAMMAR));
  out->type = ts_language_symbol_name(language, raw);
  out->grammar_type = ts_language_symbol_name(language, out->grammar_symbol);
  out->start_byte = CURSOR_GROUP(cursor, node, G_BYTE) + CURSOR_GET(cursor, node, N_BYTE);
  out->end_byte = CURSOR_GROUP(cursor, node, G_END_BYTE) - CURSOR_GET(cursor, node, N_END_BYTE);
  out->start_point = (TSPoint){CURSOR_GROUP(cursor, node, G_ROW) + CURSOR_GET(cursor, node, N_ROW),
                               CURSOR_GROUP(cursor, node, G_COL) + CURSOR_GET(cursor, node, N_COL)};
  out->end_point =
      (TSPoint){CURSOR_GROUP(cursor, node, G_END_ROW) - CURSOR_GET(cursor, node, N_END_ROW),
                CURSOR_GROUP(cursor, node, G_END_COL) - CURSOR_GET(cursor, node, N_END_COL)};
  out->is_named = ts_language_symbol_metadata(language, raw).named;
  out->is_extra = CURSOR_GET(cursor, node, N_EXTRA);
  out->is_missing = CURSOR_GET(cursor, node, N_MISSING);
  out->is_error = out->symbol == ts_builtin_sym_error;
  out->has_error = CURSOR_GET(cursor, node, N_ERROR);
  out->field_id = CURSOR_GET(cursor, node, N_FIELD);
  // Counts scan related nodes, which would evict the current group's columns.
  // Keep them on the ordinary node path in both cursor variants.
  out->child_count = sq_node_child_count(node);
  out->named_child_count = sq_node_named_child_count(node);
  out->descendant_count = sq_node_descendant_count(node);
}
