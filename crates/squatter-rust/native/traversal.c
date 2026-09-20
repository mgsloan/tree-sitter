#include "internal.h"
#include "reductions.h"
#include "tree.h"

typedef Length PackPosition;

typedef struct {
  SQGrammar *grammar;
  uint32_t words, visible_depth;
  PackPosition *positions;
  uint32_t position_count, position_capacity;
  const TSFieldId *fields;
  const DirectFieldSlice *production_fields;
  uint64_t *masks;
  uint32_t mask_count, mask_capacity;
  const uint16_t *supertype_indexes, *public_index;
  uint32_t symbol_space;
  const TSLanguage *language;
  uint32_t symbol_count;
  bool small_supertypes, points;
  SQEvent *output;
  uint32_t written;
  SQError *error;
} Builder;

typedef struct {
  const Subtree *subtree;
  PackPosition position;
  uint64_t mask;
  uint32_t mask_offset;

  TSFieldId field;
  TSSymbol alias;
  bool later;
} EmitNode;

// Leading member so a visible frame emits without copying its node state.
typedef struct {
  EmitNode node;
  const Subtree *children;
  const TSSymbol *aliases;
  PackPosition inline_position;
  uint32_t child_end_byte;
  uint64_t child_mask;
  uint32_t position_mark, position_offset;
  uint32_t field_offset, field_length;
  uint32_t mask_mark, child_mask_offset;
  uint32_t remaining, structural;
  bool visible, child_later;
} Frame;

typedef struct {
  uint32_t child_count, visible_child_count;
  uint32_t size_bytes, padding_bytes;
  bool visible, extra;
} ChildFacts;

static inline ChildFacts child_facts(Subtree subtree) {
  ChildFacts facts;
  if (subtree.data.is_inline) {
    facts.child_count = 0;
    facts.visible_child_count = 0;
    facts.size_bytes = subtree.data.size_bytes;
    facts.padding_bytes = subtree.data.padding_bytes;
    facts.visible = subtree.data.visible;
    facts.extra = subtree.data.extra;
  } else {
    const SubtreeHeapData *data = subtree.ptr;
    facts.child_count = data->child_count;

    // visible_child_count shares a union with terminal-only members.
    facts.visible_child_count = data->child_count ? data->visible_child_count : 0;
    facts.size_bytes = data->size.bytes;
    facts.padding_bytes = data->padding.bytes;
    facts.visible = data->visible;
    facts.extra = data->extra;
  }
  return facts;
}

static uint32_t grown_capacity(uint32_t capacity, uint64_t needed, size_t element_size) {
  if (needed > UINT32_MAX || needed > SIZE_MAX / element_size) return 0;
  if (!capacity) capacity = 32;
  while (capacity < needed) {
    if (capacity > UINT32_MAX / 2) return (uint32_t)needed;
    capacity *= 2;
  }
  return capacity > SIZE_MAX / element_size ? (uint32_t)needed : capacity;
}

static bool reserve_positions(Builder *builder, uint32_t count, uint32_t *offset) {
  uint64_t needed = (uint64_t)builder->position_count + count;
  if (needed > builder->position_capacity) {
    uint32_t capacity = grown_capacity(builder->position_capacity, needed, sizeof(PackPosition));
    if (!capacity) goto allocation;

    PackPosition *next = realloc(builder->positions, (size_t)capacity * sizeof(PackPosition));
    if (!next) goto allocation;
    builder->positions = next;
    builder->position_capacity = capacity;
  }

  *offset = builder->position_count;
  builder->position_count += count;
  return true;
allocation:
  sq_native_fail(builder->error, SQ_ERROR_ALLOCATION);
  return false;
}

static bool reserve_masks(Builder *builder, uint32_t count, uint32_t *offset) {
  uint64_t needed = (uint64_t)builder->mask_count + count;
  if (needed > builder->mask_capacity) {
    uint32_t capacity = grown_capacity(builder->mask_capacity, needed, sizeof(uint64_t));
    if (!capacity) goto allocation;

    uint64_t *next = realloc(builder->masks, (size_t)capacity * sizeof(uint64_t));
    if (!next) goto allocation;
    builder->masks = next;
    builder->mask_capacity = capacity;
  }

  *offset = builder->mask_count;
  builder->mask_count += count;
  return true;
allocation:
  sq_native_fail(builder->error, SQ_ERROR_ALLOCATION);
  return false;
}

static bool intern_mask(Builder *builder, const uint64_t *mask, uint16_t *result) {
  if (builder->small_supertypes) {
    *result = mask ? (uint8_t)mask[0] : 0;
    return true;
  }

  uint32_t id = sq_native_supertype_mask_id(builder->grammar->supertype_grammar, mask);
  if (id == SQ_NONE) {
    // Never introduce order-dependent IDs if a grammar/runtime combination
    // violates the conservative analysis.
    sq_native_fail(builder->error, SQ_ERROR_LANGUAGE);
    return false;
  }
  *result = (uint16_t)id;
  return true;
}

static inline uint32_t encode_symbol(const Builder *builder, TSSymbol symbol) {
  return symbol == ts_builtin_sym_error          ? builder->symbol_space - 2
         : symbol == ts_builtin_sym_error_repeat ? builder->symbol_space - 1
                                                 : symbol;
}

static bool emit_values(Builder *builder, const EmitNode *frame, PackPosition end, TSSymbol grammar,
                        bool extra, bool missing, bool has_error) {
  uint16_t supertype;
  const uint64_t *mask = builder->words == 1  ? &frame->mask
                         : builder->words > 1 ? builder->masks + frame->mask_offset
                                              : NULL;
  if (!intern_mask(builder, mask, &supertype)) return false;
  uint16_t original = (uint16_t)encode_symbol(builder, grammar);
  builder->output[builder->written++] = (SQEvent){
      .depth = builder->visible_depth,
      .start_byte = frame->position.bytes,
      .end_byte = end.bytes,
      .start_point = builder->points ? frame->position.extent : (TSPoint){0},
      .end_point = builder->points ? end.extent : (TSPoint){0},
      .symbol = builder->public_index[frame->alias ? frame->alias : original],
      .grammar = original,
      .field = frame->field,
      .supertype = supertype,
      .flags = !frame->later | (extra << 1) | (missing << 2) | (has_error << 3),
  };
  return true;
}

static bool emit(Builder *builder, const EmitNode *frame) {
  Subtree subtree = *frame->subtree;
  Length size;
  TSSymbol grammar;
  uint32_t error_cost;
  bool extra, missing;
  if (subtree.data.is_inline) {
    size = (Length){subtree.data.size_bytes, {0, subtree.data.size_bytes}};
    grammar = subtree.data.symbol;
    error_cost = 0;
    extra = subtree.data.extra;
    missing = subtree.data.is_missing;
  } else {
    const SubtreeHeapData *data = subtree.ptr;
    size = data->size;
    grammar = data->symbol;
    error_cost = data->error_cost;
    extra = data->extra;
    missing = data->is_missing;
  }
  PackPosition end = {.bytes = frame->position.bytes + size.bytes};
  if (builder->points) end = length_add(frame->position, size);
  // Missing subtrees report an error even when their stored cost is zero.
  return emit_values(builder, frame, end, grammar, extra, missing, missing || error_cost > 0);
}

static bool init_frame(Builder *builder, Frame *frame, const Subtree *subtree_pointer,
                       PackPosition position, TSSymbol alias, TSFieldId field, bool visible,
                       bool later, uint64_t mask, uint32_t mask_offset) {
  frame->node.subtree = subtree_pointer;
  frame->node.position = position;
  frame->node.mask = mask;
  frame->node.mask_offset = mask_offset;
  frame->node.field = field;
  frame->node.alias = alias;
  frame->node.later = later;
  frame->visible = visible;
  frame->position_mark = builder->position_count;
  frame->position_offset = SQ_NONE;
  frame->field_offset = SQ_NONE;
  frame->field_length = 0;
  frame->mask_mark = builder->mask_count;
  frame->child_mask_offset = SQ_NONE;
  frame->remaining = 0;
  Subtree subtree = *subtree_pointer;
  uint32_t count = ts_subtree_child_count(subtree);
  if (count) {
    frame->child_later = false;
    frame->children = ts_subtree_children(subtree);
    frame->aliases = ts_language_alias_sequence(builder->language, subtree.ptr->production_id);
    PackPosition *positions = NULL;
    if (builder->points) {
      if (count > 1 && !reserve_positions(builder, count, &frame->position_offset)) return false;
      // Extents cannot be recovered by subtracting a multiline child's size.
      positions =
          count == 1 ? &frame->inline_position : builder->positions + frame->position_offset;
    } else {
      frame->child_end_byte = position.bytes + subtree.ptr->size.bytes;
    }
    uint32_t structural = 0;
    for (uint32_t i = 0; i < count; i++) {
      Subtree child = frame->children[i];
      bool extra;
      if (builder->points) {
        Length padding, size;
        if (child.data.is_inline) {
          padding = (Length){child.data.padding_bytes,
                             {child.data.padding_rows, child.data.padding_columns}};
          size = (Length){child.data.size_bytes, {0, child.data.size_bytes}};
          extra = child.data.extra;
        } else {
          padding = child.ptr->padding;
          size = child.ptr->size;
          extra = child.ptr->extra;
        }

        if (i) position = length_add(position, padding);
        positions[i] = position;
        position = length_add(position, size);
      } else {
        extra = ts_subtree_extra(child);
      }
      structural += !extra;
    }

    frame->structural = structural;

    if (builder->production_fields) {
      DirectFieldSlice slice = builder->production_fields[subtree.ptr->production_id];
      frame->field_offset = slice.offset;
      frame->field_length = slice.length;
    }

    if (builder->words == 1) {
      frame->child_mask = visible ? 0 : mask;
      TSSymbol own = alias ? alias : ts_subtree_symbol(subtree);
      if (own < builder->symbol_count && builder->supertype_indexes[own]) {
        frame->child_mask |= UINT64_C(1) << (builder->supertype_indexes[own] - 1);
      }
    } else if (builder->words > 1) {
      if (!reserve_masks(builder, builder->words, &frame->child_mask_offset)) return false;
      uint64_t *child_mask = builder->masks + frame->child_mask_offset;
      if (visible) {
        memset(child_mask, 0, (size_t)builder->words * sizeof(uint64_t));
      } else {
        memcpy(child_mask, builder->masks + mask_offset, (size_t)builder->words * sizeof(uint64_t));
      }

      TSSymbol own = alias ? alias : ts_subtree_symbol(subtree);
      if (own < builder->symbol_count && builder->supertype_indexes[own]) {
        uint32_t index = builder->supertype_indexes[own] - 1;
        child_mask[index / 64] |= UINT64_C(1) << (index % 64);
      }
    }

    frame->remaining = count;
  }

  builder->visible_depth += visible;
  return true;
}

typedef struct {
  const Subtree *child;
  uint64_t mask;
  uint32_t child_count;
  TSFieldId field;
  TSSymbol alias;
  bool visible;
} Descent;

// A hidden subtree with one child keeps nothing past that child: the child starts
// at the same position, no scratch is reserved, and later passes through
// unchanged. Descend without a frame, computing what init_frame and the single
// pop of its child would: the production's alias and direct field for structural
// child 0, and this subtree's supertype bit. Out of line so grammars with few
// such wrappers keep the traversal loop compact.
__attribute__((noinline)) static void descend_hidden(const Builder *builder,
                                                     const TSLanguage *language, uint32_t symbols,
                                                     Descent *descent) {
  do {
    const SubtreeHeapData *data = descent->child->ptr;
    const Subtree *grandchild = ts_subtree_children(*descent->child);
    ChildFacts inner = child_facts(*grandchild);
    TSSymbol inner_alias = 0;
    TSFieldId inner_field = 0;
    if (!inner.extra) {
      const TSSymbol *aliases = ts_language_alias_sequence(language, data->production_id);
      if (aliases) inner_alias = aliases[0];
      inner_field = descent->field;
      if (builder->production_fields) {
        DirectFieldSlice slice = builder->production_fields[data->production_id];
        if (slice.length && builder->fields[slice.offset])
          inner_field = builder->fields[slice.offset];
      }
    }

    if (builder->words == 1 && data->symbol < symbols && builder->supertype_indexes[data->symbol]) {
      descent->mask |= UINT64_C(1) << (builder->supertype_indexes[data->symbol] - 1);
    }

    descent->child = grandchild;
    descent->alias = inner_alias;
    descent->visible = inner_alias || inner.visible;
    descent->field = inner_field;
    descent->child_count = inner.child_count;
  } while (!descent->visible && descent->child_count == 1);
}

typedef struct ReductionFrame {
  EmitNode node;
  uint32_t index, next_child;
  uint32_t mask_mark, child_mask_offset;
  uint64_t child_mask;
  bool visible, child_later;
} ReductionFrame;

static bool init_reduction_frame(Builder *builder, ReductionFrame *frame, const SQReduction *nodes,
                                 uint32_t index, TSSymbol alias, TSFieldId field, bool visible,
                                 bool later, uint64_t mask, uint32_t mask_offset) {
  const SQReduction *node = &nodes[index];
  *frame = (ReductionFrame){
      .node = {.position = {node->start_byte, node->start_point},
               .mask = mask,
               .mask_offset = mask_offset,
               .alias = alias,
               .field = field,
               .later = later},
      .index = index,
      .next_child = node->first_child,
      .mask_mark = builder->mask_count,
      .child_mask_offset = SQ_NONE,
      .visible = visible,
  };
  TSSymbol own = alias ? alias : node->symbol;
  if (builder->words == 1) {
    frame->child_mask = visible ? 0 : mask;
    if (own < builder->symbol_count && builder->supertype_indexes[own]) {
      frame->child_mask |= UINT64_C(1) << (builder->supertype_indexes[own] - 1);
    }
  } else if (builder->words > 1) {
    if (!reserve_masks(builder, builder->words, &frame->child_mask_offset)) return false;
    uint64_t *child_mask = builder->masks + frame->child_mask_offset;
    if (visible) memset(child_mask, 0, builder->words * sizeof(uint64_t));
    else memcpy(child_mask, builder->masks + mask_offset, builder->words * sizeof(uint64_t));
    if (own < builder->symbol_count && builder->supertype_indexes[own]) {
      uint32_t supertype = builder->supertype_indexes[own] - 1;
      child_mask[supertype / 64] |= UINT64_C(1) << (supertype % 64);
    }
  }
  builder->visible_depth += visible;
  return true;
}

static bool emit_reduction(Builder *builder, const EmitNode *frame, const SQReduction *node) {
  PackPosition end = {node->end_byte, node->end_point};
  return emit_values(builder, frame, end, node->symbol, node->extra, false, false);
}

struct SQTraversal {
  Builder builder;
  Frame *stack;
  ReductionFrame *reduction_stack;
  size_t depth, stack_capacity, reduction_stack_capacity;
  const SQReduction *reductions;
  uint32_t expected_nodes;
  SQError error;
};

SQTraversal *sq_native_traversal_new(void) {
  return calloc(1, sizeof(SQTraversal));
}

void sq_native_traversal_end(SQTraversal *context) {
  context->depth = 0;
  context->reductions = NULL;
  context->builder.position_count = context->builder.mask_count = 0;
  context->builder.grammar = NULL;
  context->builder.output = NULL;
}

void sq_native_traversal_trim(SQTraversal *context) {
  sq_native_traversal_end(context);
  free(context->builder.positions);
  free(context->builder.masks);
  free(context->stack);
  free(context->reduction_stack);
  *context = (SQTraversal){0};
}

void sq_native_traversal_delete(SQTraversal *context) {
  if (context) {
    sq_native_traversal_trim(context);
    free(context);
  }
}

uint32_t sq_native_traversal_node_count(const SQTraversal *context) {
  return context->expected_nodes;
}

static bool begin(SQTraversal *context, SQGrammar *grammar, bool points, uint32_t *zero_mask) {
  Builder *builder = &context->builder;
  builder->grammar = grammar;
  builder->words = (grammar->supertype_count + 63) / 64;
  builder->symbol_count = grammar->language->symbol_count + grammar->language->alias_count;
  builder->symbol_space = builder->symbol_count + 2;
  builder->language = grammar->language;
  builder->small_supertypes = grammar->supertype_count <= 8;
  builder->points = points;
  builder->visible_depth = 0;
  builder->position_count = builder->mask_count = 0;
  builder->fields = grammar->direct_fields;
  builder->production_fields = grammar->production_fields;
  builder->supertype_indexes = grammar->supertype_indexes;
  builder->public_index = grammar->public_index;
  builder->error = &context->error;
  context->error = SQ_OK;
  *zero_mask = SQ_NONE;
  if (builder->words > 1) {
    if (!reserve_masks(builder, builder->words, zero_mask)) return false;
    memset(builder->masks + *zero_mask, 0, builder->words * sizeof(uint64_t));
  }
  return true;
}

bool sq_native_traversal_begin_tree(SQTraversal *context, SQGrammar *grammar, const TSTree *tree,
                                    bool points, SQError *error) {
  sq_native_traversal_end(context);
  if (!tree || grammar->language != ts_tree_language(tree)) {
    sq_native_fail(error, SQ_ERROR_LANGUAGE);
    return false;
  }
  uint32_t zero_mask;
  if (!begin(context, grammar, points, &zero_mask)) goto failure;
  if (!context->stack) {
    context->stack_capacity = 32;
    context->stack = malloc(context->stack_capacity * sizeof(Frame));
    if (!context->stack) {
      context->error = SQ_ERROR_ALLOCATION;
      goto failure;
    }
  }
  TSNode root = ts_tree_root_node(tree);
  context->expected_nodes = ts_node_descendant_count(root);
  PackPosition position = {root.context[0], {root.context[1], root.context[2]}};
  if (!init_frame(&context->builder, context->stack, (const Subtree *)root.id, position,
                  (TSSymbol)root.context[3], 0, true, false, 0, zero_mask))
    goto failure;
  context->depth = 1;
  sq_native_fail(error, SQ_OK);
  return true;
failure:
  sq_native_fail(error, context->error);
  sq_native_traversal_end(context);
  return false;
}

bool sq_native_traversal_begin_reductions(SQTraversal *context, SQGrammar *grammar,
                                          const SQReduction *nodes, uint32_t count, uint32_t root,
                                          bool points, SQError *error) {
  sq_native_traversal_end(context);
  if (!nodes || root >= count) {
    sq_native_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }
  uint32_t zero_mask;
  if (!begin(context, grammar, points, &zero_mask)) goto failure;
  if (!context->reduction_stack) {
    context->reduction_stack_capacity = 32;
    context->reduction_stack = malloc(context->reduction_stack_capacity * sizeof(ReductionFrame));
    if (!context->reduction_stack) {
      context->error = SQ_ERROR_ALLOCATION;
      goto failure;
    }
  }
  if (!init_reduction_frame(&context->builder, context->reduction_stack, nodes, root, 0, 0, true,
                            false, 0, zero_mask))
    goto failure;
  context->reductions = nodes;
  context->expected_nodes = nodes[root].visible_descendant_count + 1;
  context->depth = 1;
  sq_native_fail(error, SQ_OK);
  return true;
failure:
  sq_native_fail(error, context->error);
  sq_native_traversal_end(context);
  return false;
}

static bool fill_tree(SQTraversal *context, uint32_t capacity) {
  Builder builder = context->builder;
  size_t depth = context->depth, stack_capacity = context->stack_capacity;
  Frame *stack = context->stack;
  const TSLanguage *language = builder.language;
  uint32_t symbols = builder.symbol_count;
  SQError *error = builder.error;
  bool success = true;
  while (depth && builder.written < capacity) {
    Frame *frame = &stack[depth - 1];
    if (frame->remaining) {
      uint32_t index = --frame->remaining;
      const Subtree *child = &frame->children[index];
      ChildFacts facts = child_facts(*child);
      uint32_t position_byte = 0;
      if (!builder.points) {
        // The first child's padding belongs to the parent.
        position_byte = frame->child_end_byte - facts.size_bytes;
        frame->child_end_byte = index ? position_byte - facts.padding_bytes : position_byte;
      }
      bool extra = facts.extra;
      if (!extra) {
        --frame->structural;
      }

      TSSymbol alias = extra || !frame->aliases ? 0 : frame->aliases[frame->structural];
      bool visible = alias || facts.visible;
      bool later = frame->child_later || (!frame->visible && frame->node.later);
      frame->child_later |= visible || facts.visible_child_count > 0;

      // Hidden wrappers carry their incoming field; visible nodes start a new
      // child relationship. Extras interrupt field inheritance.
      TSFieldId field = frame->visible || extra ? 0 : frame->node.field;
      if (!extra && frame->structural < frame->field_length) {
        TSFieldId direct = builder.fields[frame->field_offset + frame->structural];
        if (direct) field = direct;
      }

      uint32_t child_count = facts.child_count;
      if (!child_count && !visible) {
        continue;
      }

      PackPosition position = builder.points
                                  ? (frame->position_offset != SQ_NONE
                                         ? builder.positions[frame->position_offset + index]
                                         : frame->inline_position)
                                  : (PackPosition){.bytes = position_byte};
      // Only the one-word mode initializes this value. Other modes carry no
      // mask, or use child_mask_offset in the arena.
      uint64_t child_mask = builder.words == 1 ? frame->child_mask : 0;
      uint32_t child_mask_offset = frame->child_mask_offset;

      if (!visible && child_count == 1 && builder.words <= 1) {
        Descent descent = {.child = child,
                           .mask = child_mask,
                           .child_count = child_count,
                           .field = field,
                           .alias = alias,
                           .visible = visible};
        descend_hidden(&builder, language, symbols, &descent);
        child = descent.child;
        child_mask = descent.mask;
        child_count = descent.child_count;
        field = descent.field;
        alias = descent.alias;
        visible = descent.visible;
      }

      if (!child_count) {
        if (!visible) continue;
        EmitNode leaf = {.subtree = child,
                         .position = position,
                         .mask = child_mask,
                         .mask_offset = child_mask_offset,
                         .field = field,
                         .alias = alias,
                         .later = later};
        if (!emit(&builder, &leaf)) goto failure;
        continue;
      }

      if (depth == stack_capacity) {
        if (stack_capacity > SIZE_MAX / 2 / sizeof(Frame)) {
          sq_native_fail(error, SQ_ERROR_OVERFLOW);
          goto failure;
        }

        Frame *next = realloc(stack, stack_capacity * 2 * sizeof(Frame));
        if (!next) {
          sq_native_fail(error, SQ_ERROR_ALLOCATION);
          goto failure;
        }

        stack = next;
        stack_capacity *= 2;
      }

      if (!init_frame(&builder, &stack[depth], child, position, alias, field, visible, later,
                      child_mask, child_mask_offset)) {
        goto failure;
      }

      depth++;
    } else {
      builder.visible_depth -= frame->visible;
      if (frame->visible && !emit(&builder, &frame->node)) {
        goto failure;
      }

      builder.position_count = frame->position_mark;
      builder.mask_count = frame->mask_mark;
      depth--;
    }
  }

  goto cleanup;
failure:
  success = false;
cleanup:
  context->builder = builder;
  context->depth = depth;
  context->stack = stack;
  context->stack_capacity = stack_capacity;
  return success;
}

static bool fill_reductions(SQTraversal *context, uint32_t capacity) {
  Builder builder = context->builder;
  size_t depth = context->depth, stack_capacity = context->reduction_stack_capacity;
  ReductionFrame *stack = context->reduction_stack;
  const SQReduction *nodes = context->reductions;
  SQError *error = builder.error;
  bool success = true;
  while (depth && builder.written < capacity) {
    ReductionFrame *frame = &stack[depth - 1];
    if (frame->next_child == SQ_NONE) {
      builder.visible_depth -= frame->visible;
      if (frame->visible && !emit_reduction(&builder, &frame->node, &nodes[frame->index]))
        goto failure;
      builder.mask_count = frame->mask_mark;
      depth--;
      continue;
    }
    uint32_t index = frame->next_child;
    const SQReduction *child = &nodes[index];
    frame->next_child = child->next_sibling;
    TSFieldId field = frame->visible || child->extra ? 0 : frame->node.field;
    if (child->field) field = child->field;
    bool later = frame->child_later || (!frame->visible && frame->node.later);
    frame->child_later = true;
    uint64_t mask = frame->child_mask;
    uint32_t mask_offset = frame->child_mask_offset;
    // Hidden unary nodes need no return frame. Their field, supertype mask,
    // and sibling flag pass through to the only child with visible output.
    if (builder.words <= 1) {
      while (!child->visible && nodes[child->first_child].next_sibling == SQ_NONE) {
        if (builder.words && builder.supertype_indexes[child->symbol]) {
          mask |= UINT64_C(1) << (builder.supertype_indexes[child->symbol] - 1);
        }
        index = child->first_child;
        child = &nodes[index];
        if (child->extra) field = 0;
        if (child->field) field = child->field;
      }
    }
    TSSymbol alias = child->alias;
    bool visible = child->visible;
    if (child->first_child == SQ_NONE) {
      EmitNode leaf = {.position = {child->start_byte, child->start_point},
                       .alias = alias,
                       .field = field,
                       .later = later,
                       .mask = mask,
                       .mask_offset = mask_offset};
      if (!emit_reduction(&builder, &leaf, child)) goto failure;
      continue;
    }
    if (depth == stack_capacity) {
      if (stack_capacity > SIZE_MAX / 2 / sizeof(*stack)) {
        sq_native_fail(error, SQ_ERROR_OVERFLOW);
        goto failure;
      }
      ReductionFrame *next = realloc(stack, stack_capacity * 2 * sizeof(*stack));
      if (!next) {
        sq_native_fail(error, SQ_ERROR_ALLOCATION);
        goto failure;
      }
      stack = next;
      stack_capacity *= 2;
    }
    if (!init_reduction_frame(&builder, &stack[depth], nodes, index, alias, field, visible, later,
                              mask, mask_offset))
      goto failure;
    depth++;
  }
  goto cleanup;
failure:
  success = false;
cleanup:
  context->builder = builder;
  context->depth = depth;
  context->reduction_stack = stack;
  context->reduction_stack_capacity = stack_capacity;
  return success;
}

bool sq_native_traversal_fill(SQTraversal *context, SQEvent *events, uint32_t capacity,
                              uint32_t *written, bool *done, SQError *error) {
  *written = 0;
  *done = false;
  if (!capacity || !events) {
    sq_native_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }
  context->builder.output = events;
  context->builder.written = 0;
  bool success =
      context->reductions ? fill_reductions(context, capacity) : fill_tree(context, capacity);
  *written = context->builder.written;
  *done = context->depth == 0;
  context->builder.output = NULL;
  sq_native_fail(error, context->error);
  return success;
}
