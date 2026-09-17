#ifndef SQUAT_ATTRIBUTES_H_
#define SQUAT_ATTRIBUTES_H_
#include "internal.h"

// Shared constant-time metadata construction keeps cursor and iterator semantics identical.
static inline void sq_attributes_finish(SQNode node, uint32_t symbol, uint32_t grammar,
                                        TSFieldId field, SQCursorAttributes *out) {
  const TSLanguage *language = node.tree->language;
  TSSymbol display = sq_decode_symbol(node.tree, symbol);
  out->symbol = display;
  out->grammar_symbol = sq_decode_symbol(node.tree, grammar);
  out->type = ts_language_symbol_name(language, display);
  out->grammar_type = ts_language_symbol_name(language, out->grammar_symbol);
  out->is_named = ts_language_symbol_metadata(language, display).named;
  out->is_error = out->symbol == ts_builtin_sym_error;
  out->field_id = field;
}

static inline void sq_attributes_with_ids(SQNode node, uint32_t symbol, uint32_t grammar,
                                          TSFieldId field, SQCursorAttributes *out) {
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  out->start_byte = sq_group_start_byte_base(node.tree, group) + sq_node_start_byte_delta(node);
  out->end_byte = sq_group_end_byte_base(node.tree, group) - sq_node_end_byte_delta(node);
  if (sq_tree_has_points(node.tree)) {
    uint64_t start_point = sq_group_start_point_base(node.tree, group) +
                           sq_expand_point_key((uint16_t)sq_node_start_point_key(node));
    uint64_t end_point = sq_group_end_point_base(node.tree, group) -
                         sq_expand_point_key((uint16_t)sq_node_end_point_key(node));
    out->start_point = sq_point_from_key(start_point);
    out->end_point = sq_point_from_key(end_point);
  } else {
    out->start_point = (TSPoint){0, out->start_byte};
    out->end_point = (TSPoint){0, out->end_byte};
  }
  out->is_extra = sq_node_extra_flag(node);
  out->is_missing = sq_node_missing_flag(node);
  out->has_error = sq_node_error_flag(node);
  sq_attributes_finish(node, symbol, grammar, field, out);
}

#endif
