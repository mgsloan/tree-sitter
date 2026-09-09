#ifndef SQUAT_ATTRIBUTES_H_
#define SQUAT_ATTRIBUTES_H_
#include "internal.h"

/* Shared snapshot construction keeps cursor and iterator semantics identical.
 * Only the three variable-width IDs may come from an iterator's unpack cache. */
static inline void sq_attributes_with_ids(SQNode node, uint32_t symbol, uint32_t grammar,
                                          TSFieldId field, SQCursorAttributes *out) {
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  const TSLanguage *language = node.tree->language;
  TSSymbol raw = sq_decode_symbol(node.tree, symbol);
  out->symbol = ts_language_public_symbol(language, raw);
  out->grammar_symbol = sq_decode_symbol(node.tree, grammar);
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
  out->field_id = field;
  out->child_count = sq_node_child_count(node);
  out->named_child_count = sq_node_named_child_count(node);
  out->descendant_count = sq_node_descendant_count(node);
}

#endif
