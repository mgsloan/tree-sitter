#include "internal.h"
#include "../src/tree.h"

// Only the current group's absolute values are staged. Frame coordinates are
// computed once left-to-right, then consumed right-to-left (columns cannot be
// recovered by subtracting a multiline child's extent). No recursive C calls.
typedef struct {
  uint32_t span;
  uint32_t start_byte;
  uint32_t end_byte;
#if SQ_INCLUDE_POINTS
  uint32_t start_row;
  uint32_t end_row;
  uint32_t start_column;
  uint32_t end_column;
#endif
} PackValues;

typedef struct {
  PackValues values;
  uint32_t symbol, grammar, field;
  uint8_t flags, super;
} Pending;

typedef struct {
  SQTree *tree;
  Pending pending[SQ_GROUP_SIZE];
  uint32_t count;
  PackValues base, max;
  uint64_t *dictionary;
  uint32_t dictionary_count, words;
  SQError *error;
} Builder;

#if SQ_INCLUDE_POINTS
typedef Length PackPosition;
#else
typedef uint32_t PackPosition;
#endif

typedef struct {
  TSNode node;
  PackPosition *positions;
  uint64_t *mask;
  uint32_t remaining, structural;

  // Lower physical boundary of this subtree. Reverse preorder lets the
  // builder append groups; growth never changes existing slot indexes.
  uint32_t boundary;
  TSFieldId field;
  bool visible, later, child_later;
} Frame;

static uint32_t distance(const Builder *builder) {
  return sq_header(builder->tree)->group_count * SQ_GROUP_SIZE + builder->count;
}

static bool close_group(Builder *builder) {
  if (!builder->count) {
    return true;
  }

  SQTree *tree = builder->tree;
  SQHeader *header = sq_header(tree);
  if (header->group_count == header->group_capacity) {
    uint32_t capacity = header->group_capacity;
    if (capacity > UINT32_MAX / 2 || !sq_resize(&builder->tree, capacity * 2, builder->error)) {
      return false;
    }

    tree = builder->tree;
    header = sq_header(tree);
  }

  // Track actual extrema until the group closes so zero-base selection cannot
  // change group boundaries. These bases need not retain actual minima: revisit
  // this choice if minimum subtree spans or column positions become useful.
  // End columns keep their actual maxima for the base-minus-delta encoding.
  if (builder->max.span <= UINT8_MAX) builder->base.span = 0;
#if SQ_INCLUDE_POINTS
  if (builder->max.start_column <= UINT8_MAX) builder->base.start_column = 0;
#endif

  uint32_t group = header->group_count++;
  sq_set_packed(tree->data, tree->layout.waste, group, SQ_WASTE_BITS,
                SQ_GROUP_SIZE - builder->count);
  sq_set_u32(tree->data, tree->layout.span_base, group, builder->base.span);
  sq_set_u32(tree->data, tree->layout.start_byte_base, group, builder->base.start_byte);
  sq_set_u32(tree->data, tree->layout.end_byte_base, group, builder->max.end_byte);
#if SQ_INCLUDE_POINTS
  sq_set_u32(tree->data, tree->layout.start_row_base, group, builder->base.start_row);
  sq_set_u32(tree->data, tree->layout.end_row_base, group, builder->max.end_row);
  sq_set_u32(tree->data, tree->layout.start_column_base, group, builder->base.start_column);
  sq_set_u32(tree->data, tree->layout.end_column_base, group, builder->max.end_column);
#endif

  for (uint32_t i = 0; i < builder->count; i++) {
    const Pending *pending = &builder->pending[i];
    uint32_t slot = group * SQ_GROUP_SIZE + i;

    sq_set_bit(tree->data, tree->layout.last, slot, pending->flags & 1);
    sq_set_bit(tree->data, tree->layout.extra, slot, pending->flags & 2);
    sq_set_bit(tree->data, tree->layout.error, slot, pending->flags & 4);
    sq_set_bit(tree->data, tree->layout.missing, slot, pending->flags & 8);

    sq_set_u8(tree->data, tree->layout.span_delta, slot, pending->values.span - builder->base.span);
    sq_set_u8(tree->data, tree->layout.start_byte_delta, slot,
              pending->values.start_byte - builder->base.start_byte);
    sq_set_u16(tree->data, tree->layout.end_byte_delta, slot,
               builder->max.end_byte - pending->values.end_byte);
#if SQ_INCLUDE_POINTS
    sq_set_u8(tree->data, tree->layout.start_row_delta, slot,
              pending->values.start_row - builder->base.start_row);
    sq_set_u8(tree->data, tree->layout.end_row_delta, slot,
              builder->max.end_row - pending->values.end_row);
    sq_set_u8(tree->data, tree->layout.start_column_delta, slot,
              pending->values.start_column - builder->base.start_column);
    sq_set_u8(tree->data, tree->layout.end_column_delta, slot,
              builder->max.end_column - pending->values.end_column);
#endif

    sq_set_u8(tree->data, tree->layout.supertype, slot, pending->super);
    sq_set_packed(tree->data, tree->layout.symbol, slot, tree->layout.symbol_bits, pending->symbol);
    sq_set_packed(tree->data, tree->layout.grammar_symbol, slot, tree->layout.symbol_bits,
                  pending->grammar);
    sq_set_packed(tree->data, tree->layout.field, slot, tree->layout.field_bits, pending->field);
  }

  builder->count = 0;
  return true;
}

static bool intern_mask(Builder *builder, const uint64_t *mask, uint8_t *result) {
  if (builder->tree->supertype_count <= 8) {
    *result = mask ? (uint8_t)mask[0] : 0;
    return true;
  }

  size_t bytes = (size_t)builder->words * 8;
  for (uint32_t i = 0; i < builder->dictionary_count; i++) {
    if (!memcmp(builder->dictionary + (size_t)i * builder->words, mask, bytes)) {
      *result = (uint8_t)i;
      return true;
    }
  }

  if (builder->dictionary_count == 256) {
    sq_fail(builder->error, SQ_ERROR_DICTIONARY_FULL);
    return false;
  }

  uint64_t *next = realloc(builder->dictionary, (builder->dictionary_count + 1) * bytes);
  if (!next) {
    sq_fail(builder->error, SQ_ERROR_ALLOCATION);
    return false;
  }

  builder->dictionary = next;
  memcpy(next + (size_t)builder->dictionary_count * builder->words, mask, bytes);
  *result = (uint8_t)builder->dictionary_count++;
  return true;
}

// Stage candidate extrema separately: a rejected node must not change the
// accepted group's bases. Only end-byte deltas have a wider, u16 range.
static bool extend_range(uint32_t value, uint32_t previous_base, uint32_t previous_max,
                         uint32_t limit, uint32_t *base, uint32_t *max) {
  *base = value < previous_base ? value : previous_base;
  *max = value > previous_max ? value : previous_max;
  return *max - *base <= limit;
}

static bool group_fits(const Builder *builder, const PackValues *value, PackValues *base,
                       PackValues *max) {
  if (builder->count == SQ_GROUP_SIZE) return false;
  if (!builder->count) {
    *base = *max = *value;
    return true;
  }

  if (!extend_range(value->span, builder->base.span, builder->max.span, UINT8_MAX, &base->span,
                    &max->span))
    return false;
  if (!extend_range(value->start_byte, builder->base.start_byte, builder->max.start_byte, UINT8_MAX,
                    &base->start_byte, &max->start_byte))
    return false;
  if (!extend_range(value->end_byte, builder->base.end_byte, builder->max.end_byte, UINT16_MAX,
                    &base->end_byte, &max->end_byte))
    return false;
#if SQ_INCLUDE_POINTS
  if (!extend_range(value->start_row, builder->base.start_row, builder->max.start_row, UINT8_MAX,
                    &base->start_row, &max->start_row))
    return false;
  if (!extend_range(value->end_row, builder->base.end_row, builder->max.end_row, UINT8_MAX,
                    &base->end_row, &max->end_row))
    return false;
  if (!extend_range(value->start_column, builder->base.start_column, builder->max.start_column,
                    UINT8_MAX, &base->start_column, &max->start_column))
    return false;
  if (!extend_range(value->end_column, builder->base.end_column, builder->max.end_column, UINT8_MAX,
                    &base->end_column, &max->end_column))
    return false;
#endif
  return true;
}

static bool emit(Builder *builder, Frame *frame) {
  TSNode node = frame->node;
#if SQ_INCLUDE_POINTS
  TSPoint start = ts_node_start_point(node), end = ts_node_end_point(node);
#endif
  Pending pending = {
      .values =
          {
              .start_byte = ts_node_start_byte(node),
              .end_byte = ts_node_end_byte(node),
#if SQ_INCLUDE_POINTS
              .start_row = start.row,
              .end_row = end.row,
              .start_column = start.column,
              .end_column = end.column,
#endif
          },
      .symbol = sq_encode_symbol(builder->tree, node.context[3] ? (TSSymbol)node.context[3]
                                                                : ts_node_grammar_symbol(node)),
      .grammar = sq_encode_symbol(builder->tree, ts_node_grammar_symbol(node)),
      .field = frame->field,
      .flags = (!frame->later) | (ts_node_is_extra(node) << 1) | (ts_node_has_error(node) << 2) |
               (ts_node_is_missing(node) << 3),
  };
  if (!intern_mask(builder, frame->mask, &pending.super)) {
    return false;
  }

  for (;;) {
    if (distance(builder) >= UINT32_MAX - SQ_GROUP_SIZE) {
      sq_fail(builder->error, SQ_ERROR_OVERFLOW);
      return false;
    }

    // Retrying after close_group includes newly abandoned slots in the span.
    // The saved boundary still marks the same lower physical slot.
    pending.values.span = distance(builder) - frame->boundary;
    PackValues base, max;
    if (group_fits(builder, &pending.values, &base, &max)) {
      builder->base = base;
      builder->max = max;
      builder->pending[builder->count++] = pending;
      return true;
    }

    if (!close_group(builder)) {
      return false;
    }
  }
}

static bool init_frame(Builder *builder, Frame *frame, TSNode node, TSFieldId field, bool visible,
                       bool later, const uint64_t *mask) {
  *frame = (Frame){.node = node,
                   .field = field,
                   .visible = visible,
                   .later = later,
                   .boundary = distance(builder)};
  Subtree subtree = *(const Subtree *)node.id;
  uint32_t count = ts_subtree_child_count(subtree);
  if (builder->words) {
    frame->mask = calloc(builder->words, 8);
    if (!frame->mask) {
      goto allocation;
    }

    if (mask) {
      memcpy(frame->mask, mask, (size_t)builder->words * 8);
    }
  }

  if (count) {
    if ((uint64_t)count * sizeof(PackPosition) > SIZE_MAX) {
      goto allocation;
    }

    frame->positions = malloc((size_t)count * sizeof(PackPosition));
    if (!frame->positions) {
      goto allocation;
    }

#if SQ_INCLUDE_POINTS
    PackPosition position = {ts_node_start_byte(node), ts_node_start_point(node)};
#else
    PackPosition position = ts_node_start_byte(node);
#endif
    const Subtree *children = ts_subtree_children(subtree);
    for (uint32_t i = 0; i < count; i++) {
      if (i) {
#if SQ_INCLUDE_POINTS
        position = length_add(position, ts_subtree_padding(children[i]));
#else
        position += ts_subtree_padding(children[i]).bytes;
#endif
      }

      frame->positions[i] = position;
#if SQ_INCLUDE_POINTS
      position = length_add(position, ts_subtree_size(children[i]));
#else
      position += ts_subtree_size(children[i]).bytes;
#endif
      frame->structural += !ts_subtree_extra(children[i]);
    }

    frame->remaining = count;
  }

  return true;
allocation:
  free(frame->positions);
  free(frame->mask);
  memset(frame, 0, sizeof(*frame));
  sq_fail(builder->error, SQ_ERROR_ALLOCATION);
  return false;
}

SQPackOptions sq_pack_options_default(void) {
  return (SQPackOptions){.repack = false, .symbol_presence = true};
}

SQTree *sq_tree_pack(const TSTree *tree, SQPackOptions options, SQError *error) {
  sq_fail(error, SQ_OK);
  if (!tree) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }

  TSNode root = ts_tree_root_node(tree);
  uint32_t capacity = options.initial_group_capacity;
  if (!capacity) {
    // Reserve for 75% occupancy; scale the estimate with experimental groups.
    uint32_t expected_nodes_per_group = SQ_GROUP_SIZE * 3 / 4;
    capacity = ts_node_descendant_count(root) / expected_nodes_per_group + 1;
  }

  SQTree *result = sq_allocate(ts_tree_language(tree), capacity, error);
  if (!result) {
    return NULL;
  }

  Builder builder = {.tree = result, .words = (result->supertype_count + 63) / 64, .error = error};
  size_t depth = 0, stack_capacity = 32;
  Frame *stack = malloc(stack_capacity * sizeof(Frame));
  uint64_t *child_mask = builder.words ? calloc(builder.words, 8) : NULL;
  if (!stack || (builder.words && !child_mask)) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    goto failure;
  }

  if (!init_frame(&builder, &stack[0], root, 0, true, false, NULL)) {
    goto failure;
  }

  depth = 1;
  while (depth) {
    Frame *frame = &stack[depth - 1];
    if (frame->remaining) {
      Subtree parent = *(const Subtree *)frame->node.id;
      uint32_t index = --frame->remaining;
      const Subtree *child = &ts_subtree_children(parent)[index];
      bool extra = ts_subtree_extra(*child);
      if (!extra) {
        --frame->structural;
      }

      TSSymbol alias = extra ? 0
                             : ts_language_alias_at(builder.tree->language,
                                                    parent.ptr->production_id, frame->structural);
      bool visible = alias || ts_subtree_visible(*child);
      bool later = frame->child_later || (!frame->visible && frame->later);
      frame->child_later |= visible || ts_subtree_visible_child_count(*child) > 0;

      // Hidden wrappers carry their incoming field; visible nodes start a new
      // child relationship. Extras interrupt field inheritance.
      TSFieldId field = frame->visible || extra ? 0 : frame->field;
      if (!extra) {
        const TSFieldMapEntry *map, *end;
        ts_language_field_map(builder.tree->language, parent.ptr->production_id, &map, &end);
        for (; map < end; map++) {
          if (!map->inherited && map->child_index == frame->structural) {
            field = map->field_id;
            break;
          }
        }
      }

      // Only omitted ancestors contribute the incoming supertype mask. A
      // visible node's own supertype, if any, applies to its children.
      if (builder.words) {
        memset(child_mask, 0, (size_t)builder.words * 8);
        if (!frame->visible) {
          memcpy(child_mask, frame->mask, (size_t)builder.words * 8);
        }

        TSSymbol own = frame->node.context[3] ? (TSSymbol)frame->node.context[3]
                                              : ts_node_grammar_symbol(frame->node);
        for (uint32_t supertype_index = 0; supertype_index < builder.tree->supertype_count;
             supertype_index++) {
          if (builder.tree->supertypes[supertype_index] == own) {
            child_mask[supertype_index / 64] |= UINT64_C(1) << (supertype_index % 64);
          }
        }
      }

#if SQ_INCLUDE_POINTS
      Length position = frame->positions[index];
#else
      Length position = {.bytes = frame->positions[index]};
#endif
      TSNode node = ts_node_new(tree, child, position, alias);
      if (depth == stack_capacity) {
        if (stack_capacity > SIZE_MAX / 2 / sizeof(Frame)) {
          sq_fail(error, SQ_ERROR_OVERFLOW);
          goto failure;
        }

        Frame *next = realloc(stack, stack_capacity * 2 * sizeof(Frame));
        if (!next) {
          sq_fail(error, SQ_ERROR_ALLOCATION);
          goto failure;
        }

        stack = next;
        stack_capacity *= 2;
      }

      if (!init_frame(&builder, &stack[depth], node, field, visible, later, child_mask)) {
        goto failure;
      }

      depth++;
    } else {
      if (frame->visible && !emit(&builder, frame)) {
        goto failure;
      }

      free(frame->positions);
      free(frame->mask);
      depth--;
    }
  }

  if (!close_group(&builder)) {
    goto failure;
  }

  if (options.repack && !sq_resize(&builder.tree, sq_header(builder.tree)->group_count, error)) {
    goto failure;
  }

  if (options.symbol_presence && !sq_build_presence(&builder.tree, error)) {
    goto failure;
  }

  if (builder.tree->supertype_count > 8 &&
      !sq_append_dictionary(&builder.tree, builder.dictionary, builder.dictionary_count, error)) {
    goto failure;
  }

  free(stack);
  free(child_mask);
  free(builder.dictionary);
  return builder.tree;
failure:
  for (size_t i = 0; i < depth; i++) {
    free(stack[i].positions);
    free(stack[i].mask);
  }

  free(stack);
  free(child_mask);
  free(builder.dictionary);
  sq_tree_delete(builder.tree);
  return NULL;
}

SQTree *sq_tree_parse(TSParser *parser, const char *source, uint32_t length, SQPackOptions options,
                      SQError *error) {
  if (!parser || (!source && length)) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }

  TSTree *tree = ts_parser_parse_string(parser, NULL, source ? source : "", length);
  if (!tree) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }

  SQTree *packed = sq_tree_pack(tree, options, error);
  ts_tree_delete(tree);
  return packed;
}
