#include "internal.h"
#include "../src/tree.h"

/* Only the current group's absolute values are staged. Frame coordinates are
 * computed once left-to-right, then consumed right-to-left (columns cannot be
 * recovered by subtracting a multiline child's extent). No recursive C calls. */
typedef struct {
  uint32_t values[7];
  uint32_t symbol, grammar, field;
  uint8_t flags, super;
} Pending;
typedef struct {
  TSFieldId field;
  uint32_t target;
} FieldTarget;
typedef struct {
  FieldTarget *entries;
  uint32_t count, capacity;
} FieldTargets;
typedef struct {
  SQTree *tree;
  Pending pending[SQ_GROUP_SIZE];
  uint32_t count, min[7], max[7];
  uint64_t *dictionary;
  uint32_t dictionary_count, words;
  SQFieldException *exceptions;
  uint32_t exception_count, exception_capacity;
  SQError *error;
} Builder;
typedef struct {
  TSNode node;
  Length *positions;
  uint64_t *mask;
  uint32_t remaining, structural;
  /* Distance to the first node outside this subtree, measured from the right.
   * This stays valid when groups grow or padding is inserted to the left. */
  uint32_t boundary;
  TSFieldId field;
  FieldTargets lookup_fields, visible_fields;
  uint32_t first_visible_distance;
  bool visible, later, child_later;
} Frame;

static uint32_t field_target(const FieldTargets *targets, TSFieldId field) {
  for (uint32_t i = 0; i < targets->count; i++) {
    if (targets->entries[i].field == field) {
      return targets->entries[i].target;
    }
  }
  return 0;
}

static bool set_field_target(Builder *builder, FieldTargets *targets, TSFieldId field,
                             uint32_t target) {
  if (!field || !target) {
    return true;
  }
  for (uint32_t i = 0; i < targets->count; i++) {
    if (targets->entries[i].field == field) {
      targets->entries[i].target = target;
      return true;
    }
  }
  if (targets->count == targets->capacity) {
    uint64_t capacity = targets->capacity ? (uint64_t)targets->capacity * 2 : 4;
    if (capacity > UINT32_MAX || capacity * sizeof(FieldTarget) > SIZE_MAX) {
      sq_fail(builder->error, SQ_ERROR_OVERFLOW);
      return false;
    }
    FieldTarget *entries = realloc(targets->entries, (size_t)capacity * sizeof(FieldTarget));
    if (!entries) {
      sq_fail(builder->error, SQ_ERROR_ALLOCATION);
      return false;
    }
    targets->entries = entries;
    targets->capacity = (uint32_t)capacity;
  }
  targets->entries[targets->count++] = (FieldTarget){field, target};
  return true;
}

/* Mainline's field lookup can inherit through a node made visible by aliasing,
 * returning a grandchild. Its cursor field is still just the nearest assignment.
 * Compute both meanings bottom-up and persist only their disagreements. */
static bool update_parent_fields(Builder *builder, Frame *parent, const Frame *child) {
  if (child->first_visible_distance) {
    parent->first_visible_distance = child->first_visible_distance;
  }
  if (child->visible) {
    if (!set_field_target(builder, &parent->visible_fields, child->field,
                          child->first_visible_distance)) {
      return false;
    }
  } else {
    for (uint32_t i = 0; i < child->visible_fields.count; i++) {
      FieldTarget target = child->visible_fields.entries[i];
      if (!set_field_target(builder, &parent->visible_fields, target.field, target.target)) {
        return false;
      }
    }
  }
  if (ts_node_is_extra(child->node)) {
    return true;
  }
  Subtree subtree = *(const Subtree *)parent->node.id;
  const TSFieldMapEntry *entry, *end;
  ts_language_field_map(builder->tree->language, subtree.ptr->production_id, &entry, &end);
  for (; entry < end; entry++) {
    if (entry->child_index != parent->structural) {
      continue;
    }
    uint32_t target = entry->inherited ? field_target(&child->lookup_fields, entry->field_id)
                                       : child->first_visible_distance;
    if (!set_field_target(builder, &parent->lookup_fields, entry->field_id, target)) {
      return false;
    }
  }
  return true;
}

static bool save_field_exception(Builder *builder, uint32_t parent, TSFieldId field,
                                 uint32_t target) {
  if (builder->exception_count == builder->exception_capacity) {
    uint64_t capacity =
        builder->exception_capacity ? (uint64_t)builder->exception_capacity * 2 : 16;
    if (capacity > UINT32_MAX || capacity * sizeof(SQFieldException) > SIZE_MAX) {
      sq_fail(builder->error, SQ_ERROR_OVERFLOW);
      return false;
    }
    SQFieldException *entries =
        realloc(builder->exceptions, (size_t)capacity * sizeof(SQFieldException));
    if (!entries) {
      sq_fail(builder->error, SQ_ERROR_ALLOCATION);
      return false;
    }
    builder->exceptions = entries;
    builder->exception_capacity = (uint32_t)capacity;
  }
  builder->exceptions[builder->exception_count++] = (SQFieldException){parent, field, target};
  return true;
}

static bool save_field_exceptions(Builder *builder, const Frame *frame, uint32_t parent_distance) {
  bool error = ts_node_is_error(frame->node);
  for (uint32_t i = 0; i < frame->lookup_fields.count; i++) {
    FieldTarget lookup = frame->lookup_fields.entries[i];
    uint32_t ordinary = error ? 0 : field_target(&frame->visible_fields, lookup.field);
    if (lookup.target != ordinary &&
        !save_field_exception(builder, parent_distance, lookup.field, lookup.target)) {
      return false;
    }
  }
  if (!error) {
    for (uint32_t i = 0; i < frame->visible_fields.count; i++) {
      FieldTarget ordinary = frame->visible_fields.entries[i];
      if (!field_target(&frame->lookup_fields, ordinary.field) &&
          !save_field_exception(builder, parent_distance, ordinary.field, 0)) {
        return false;
      }
    }
  }
  return true;
}

static int compare_field_exceptions(const void *a, const void *b) {
  const SQFieldException *left = a, *right = b;
  if (left->parent != right->parent) {
    return left->parent < right->parent ? -1 : 1;
  }
  return (left->field > right->field) - (left->field < right->field);
}

static bool finish_field_exceptions(Builder *builder) {
  uint32_t slots = sq_tree_slot_count(builder->tree);
  for (uint32_t i = 0; i < builder->exception_count; i++) {
    SQFieldException *entry = &builder->exceptions[i];
    entry->parent = slots - entry->parent;
    entry->target = entry->target ? slots - entry->target : SQ_NONE;
  }
  if (builder->exception_count) {
    qsort(builder->exceptions, builder->exception_count, sizeof(SQFieldException),
          compare_field_exceptions);
  }
  return sq_append_field_exceptions(builder->tree, builder->exceptions, builder->exception_count,
                                    builder->error);
}

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
    if (capacity > UINT32_MAX / 2 || !sq_resize(tree, capacity * 2, builder->error)) {
      return false;
    }
    header = sq_header(tree);
  }
  header->group_count++;
  uint32_t group = header->group_capacity - header->group_count;
  uint32_t bases[] = {SQ_GROUP_SIZE - builder->count,
                      builder->min[0],
                      builder->min[1],
                      builder->max[2],
                      builder->min[3],
                      builder->max[4],
                      builder->min[5],
                      builder->max[6]};
  for (unsigned c = 0; c < G_COLUMNS; c++) {
    sq_set(tree->data, tree->layout.groups[c], group, sq_group_width(c), bases[c]);
  }
  for (uint32_t i = 0; i < builder->count; i++) {
    Pending *pending = &builder->pending[i];
    uint32_t slot = (group + 1) * SQ_GROUP_SIZE - i - 1;
    uint32_t values[] = {
        !!(pending->flags & 1),
        !!(pending->flags & 2),
        !!(pending->flags & 4),
        !!(pending->flags & 8),
        pending->values[0] - builder->min[0],
        pending->values[1] - builder->min[1],
        builder->max[2] - pending->values[2],
        pending->values[3] - builder->min[3],
        builder->max[4] - pending->values[4],
        pending->values[5] - builder->min[5],
        builder->max[6] - pending->values[6],
        pending->super,
        pending->symbol,
        pending->grammar,
        pending->field,
    };
    for (unsigned c = 0; c < N_COLUMNS; c++) {
      sq_set(tree->data, tree->layout.nodes[c], slot, sq_node_width(&tree->layout, c), values[c]);
    }
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
static bool emit(Builder *builder, Frame *frame) {
  TSNode node = frame->node;
  TSPoint start = ts_node_start_point(node), end = ts_node_end_point(node);
  Pending pending = {
      .values = {0, ts_node_start_byte(node), ts_node_end_byte(node), start.row, end.row,
                 start.column, end.column},
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
    /* Retrying after close_group includes newly abandoned slots in the span.
     * The saved boundary still points to the same occupied node on the right. */
    pending.values[0] = distance(builder) - frame->boundary;
    uint32_t min[7], max[7];
    bool fits = builder->count < SQ_GROUP_SIZE;
    for (unsigned c = 0; c < 7; c++) {
      min[c] = !builder->count || pending.values[c] < builder->min[c] ? pending.values[c]
                                                                      : builder->min[c];
      max[c] = !builder->count || pending.values[c] > builder->max[c] ? pending.values[c]
                                                                      : builder->max[c];
      if (max[c] - min[c] > (c == 2 ? UINT16_MAX : UINT8_MAX)) {
        fits = false;
      }
    }
    if (fits) {
      memcpy(builder->min, min, sizeof(min));
      memcpy(builder->max, max, sizeof(max));
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
    if ((uint64_t)count * sizeof(Length) > SIZE_MAX) {
      goto allocation;
    }
    frame->positions = malloc((size_t)count * sizeof(Length));
    if (!frame->positions) {
      goto allocation;
    }
    Length position = {ts_node_start_byte(node), ts_node_start_point(node)};
    const Subtree *children = ts_subtree_children(subtree);
    for (uint32_t i = 0; i < count; i++) {
      if (i) {
        position = length_add(position, ts_subtree_padding(children[i]));
      }
      frame->positions[i] = position;
      position = length_add(position, ts_subtree_size(children[i]));
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
                             : ts_language_alias_at(result->language, parent.ptr->production_id,
                                                    frame->structural);
      bool visible = alias || ts_subtree_visible(*child);
      bool later = frame->child_later || (!frame->visible && frame->later);
      frame->child_later |= visible || ts_subtree_visible_child_count(*child) > 0;
      /* Hidden wrappers carry their incoming field; visible nodes start a new
       * child relationship. Extras interrupt field inheritance. */
      TSFieldId field = frame->visible || extra ? 0 : frame->field;
      if (!extra) {
        const TSFieldMapEntry *map, *end;
        ts_language_field_map(result->language, parent.ptr->production_id, &map, &end);
        for (; map < end; map++) {
          if (!map->inherited && map->child_index == frame->structural) {
            field = map->field_id;
            break;
          }
        }
      }
      /* Only omitted ancestors contribute the incoming supertype mask. A
       * visible node's own supertype, if any, applies to its children. */
      if (builder.words) {
        memset(child_mask, 0, (size_t)builder.words * 8);
        if (!frame->visible) {
          memcpy(child_mask, frame->mask, (size_t)builder.words * 8);
        }
        TSSymbol own = frame->node.context[3] ? (TSSymbol)frame->node.context[3]
                                              : ts_node_grammar_symbol(frame->node);
        for (uint32_t supertype_index = 0; supertype_index < result->supertype_count;
             supertype_index++) {
          if (result->supertypes[supertype_index] == own) {
            child_mask[supertype_index / 64] |= UINT64_C(1) << (supertype_index % 64);
          }
        }
      }
      TSNode node = ts_node_new(tree, child, frame->positions[index], alias);
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
      if (frame->visible) {
        frame->first_visible_distance = distance(&builder);
        if (!save_field_exceptions(&builder, frame, distance(&builder))) {
          goto failure;
        }
      }
      if (depth > 1 && !update_parent_fields(&builder, &stack[depth - 2], frame)) {
        goto failure;
      }
      free(frame->positions);
      free(frame->mask);
      free(frame->lookup_fields.entries);
      free(frame->visible_fields.entries);
      depth--;
    }
  }
  if (!close_group(&builder)) {
    goto failure;
  }
  if (options.repack && !sq_resize(result, sq_header(result)->group_count, error)) {
    goto failure;
  }
  if (options.symbol_presence && !sq_build_presence(result, error)) {
    goto failure;
  }
  if (result->supertype_count > 8 &&
      !sq_append_dictionary(result, builder.dictionary, builder.dictionary_count, error)) {
    goto failure;
  }
  if (!finish_field_exceptions(&builder)) {
    goto failure;
  }
  free(stack);
  free(child_mask);
  free(builder.dictionary);
  free(builder.exceptions);
  return result;
failure:
  for (size_t i = 0; i < depth; i++) {
    free(stack[i].positions);
    free(stack[i].mask);
    free(stack[i].lookup_fields.entries);
    free(stack[i].visible_fields.entries);
  }
  free(stack);
  free(child_mask);
  free(builder.dictionary);
  free(builder.exceptions);
  sq_tree_delete(result);
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
