#ifndef SQUAT_TEST_FIELD_LOOKUP_H_
#define SQUAT_TEST_FIELD_LOOKUP_H_
#include <tree_sitter/api.h>

/* Only accept disagreement with the field-lookup API when the packed result
 * still agrees with mainline's visible-child cursor. This independently checks
 * the stored field IDs instead of excusing every field mismatch. */
static TSNode visible_child_by_field(TSNode parent, TSFieldId field) {
  TSNode result = {0};
  if (!field || ts_node_is_error(parent)) {
    return result;
  }
  TSTreeCursor cursor = ts_tree_cursor_new(parent);
  if (ts_tree_cursor_goto_first_child(&cursor)) {
    do {
      if (ts_tree_cursor_current_field_id(&cursor) == field) {
        result = ts_tree_cursor_current_node(&cursor);
        break;
      }
    } while (ts_tree_cursor_goto_next_sibling(&cursor));
  }
  ts_tree_cursor_delete(&cursor);
  return result;
}

#endif
