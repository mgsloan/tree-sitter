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

#if SQ_INCLUDE_POINTS
typedef Length PackPosition;
#else
typedef uint32_t PackPosition;
#endif

typedef struct {
  SQTree *tree;
  Pending pending[SQ_GROUP_SIZE];
  uint32_t count;
  PackValues base, max;
  uint64_t *dictionary;
  uint32_t dictionary_count, dictionary_capacity, words;
  PackPosition *positions;
  uint32_t position_count, position_capacity;
  TSFieldId *fields;
  uint32_t field_count, field_capacity;
  uint64_t *masks;
  uint32_t mask_count, mask_capacity;
  uint16_t *supertype_indexes;
  SQError *error;
} Builder;

typedef struct {
  TSNode node;
  const Subtree *children;
  const TSSymbol *aliases;
  PackPosition inline_position;
  uint64_t mask, child_mask;
  uint32_t position_mark, position_offset;
  uint32_t field_mark, field_offset;
  uint32_t mask_mark, mask_offset, child_mask_offset;
  uint32_t remaining, structural;

  // Lower physical boundary of this subtree. Reverse preorder lets the
  // builder append groups; growth never changes existing slot indexes.
  uint32_t boundary;
  TSFieldId field;
  bool visible, later, child_later;
} Frame;

static bool reserve_positions(Builder *builder, uint32_t count, uint32_t *offset) {
  uint64_t needed = (uint64_t)builder->position_count + count;
  if (needed > UINT32_MAX || needed > SIZE_MAX / sizeof(PackPosition)) goto allocation;
  if (needed > builder->position_capacity) {
    uint32_t capacity = builder->position_capacity ? builder->position_capacity : 32;
    while (capacity < needed) {
      if (capacity > UINT32_MAX / 2) {
        capacity = (uint32_t)needed;
        break;
      }
      capacity *= 2;
    }

    PackPosition *next = realloc(builder->positions, (size_t)capacity * sizeof(PackPosition));
    if (!next) goto allocation;
    builder->positions = next;
    builder->position_capacity = capacity;
  }

  *offset = builder->position_count;
  builder->position_count += count;
  return true;
allocation:
  sq_fail(builder->error, SQ_ERROR_ALLOCATION);
  return false;
}

static bool reserve_fields(Builder *builder, uint32_t count, uint32_t *offset) {
  uint64_t needed = (uint64_t)builder->field_count + count;
  if (needed > UINT32_MAX || needed > SIZE_MAX / sizeof(TSFieldId)) goto allocation;
  if (needed > builder->field_capacity) {
    uint32_t capacity = builder->field_capacity ? builder->field_capacity : 32;
    while (capacity < needed) {
      if (capacity > UINT32_MAX / 2) {
        capacity = (uint32_t)needed;
        break;
      }
      capacity *= 2;
    }

    TSFieldId *next = realloc(builder->fields, (size_t)capacity * sizeof(TSFieldId));
    if (!next) goto allocation;
    builder->fields = next;
    builder->field_capacity = capacity;
  }

  *offset = builder->field_count;
  memset(builder->fields + builder->field_count, 0, (size_t)count * sizeof(TSFieldId));
  builder->field_count += count;
  return true;
allocation:
  sq_fail(builder->error, SQ_ERROR_ALLOCATION);
  return false;
}

static bool reserve_masks(Builder *builder, uint32_t count, uint32_t *offset) {
  uint64_t needed = (uint64_t)builder->mask_count + count;
  if (needed > UINT32_MAX || needed > SIZE_MAX / sizeof(uint64_t)) goto allocation;
  if (needed > builder->mask_capacity) {
    uint32_t capacity = builder->mask_capacity ? builder->mask_capacity : 32;
    while (capacity < needed) {
      if (capacity > UINT32_MAX / 2) {
        capacity = (uint32_t)needed;
        break;
      }
      capacity *= 2;
    }

    uint64_t *next = realloc(builder->masks, (size_t)capacity * sizeof(uint64_t));
    if (!next) goto allocation;
    builder->masks = next;
    builder->mask_capacity = capacity;
  }

  *offset = builder->mask_count;
  builder->mask_count += count;
  return true;
allocation:
  sq_fail(builder->error, SQ_ERROR_ALLOCATION);
  return false;
}

static uint32_t distance(const Builder *builder) {
  return sq_header(builder->tree)->group_count * SQ_GROUP_SIZE + builder->count;
}

static void set_group_flags(uint8_t *data, uint32_t offset, uint32_t group, uint64_t flags) {
#if SQ_GROUP_SIZE == 16
  sq_set_u16(data, offset, group, (uint16_t)flags);
#elif SQ_GROUP_SIZE == 32
  sq_set_u32(data, offset, group, (uint32_t)flags);
#else
  sq_set_u64(data, offset, group, flags);
#endif
}

typedef enum { PENDING_SYMBOL, PENDING_GRAMMAR, PENDING_FIELD } PendingColumn;

static inline uint32_t pending_id(const Pending *pending, PendingColumn column) {
  switch (column) {
  case PENDING_SYMBOL:
    return pending->symbol;
  case PENDING_GRAMMAR:
    return pending->grammar;
  case PENDING_FIELD:
    return pending->field;
  }
  return 0;
}

// A group usually contributes several lanes to the same packed word. Keep that
// word in a register and commit it once, including for the fixed-width cases.
// Partial boundary words retain lanes written by adjacent groups.
static void set_pending_column(uint8_t *data, uint32_t offset, uint32_t first, uint32_t count,
                               uint8_t bits, const Pending *pending, PendingColumn column) {
  uint32_t lanes = 64 / bits;
  uint32_t word_index = first / lanes;
  uint8_t *address = data + offset + (uint64_t)word_index * 8;
  uint64_t word;
  memcpy(&word, address, sizeof(word));

  uint64_t value_mask = (UINT64_C(1) << bits) - 1;
  for (uint32_t i = 0; i < count; i++) {
    uint32_t index = first + i;
    uint32_t next_word_index = index / lanes;
    if (next_word_index != word_index) {
      memcpy(address, &word, sizeof(word));
      word_index = next_word_index;
      address = data + offset + (uint64_t)word_index * 8;
      memcpy(&word, address, sizeof(word));
    }

    uint32_t shift = index % lanes * bits;
    uint64_t mask = value_mask << shift;
    word = (word & ~mask) | ((uint64_t)pending_id(&pending[i], column) << shift);
  }
  memcpy(address, &word, sizeof(word));
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
  TSPoint start_base = {builder->base.start_row, builder->base.start_column};
  TSPoint end_base = {builder->max.end_row, builder->max.end_column};
  sq_set_u64(tree->data, tree->layout.start_point_base, group, sq_point_key(start_base));
  sq_set_u64(tree->data, tree->layout.end_point_base, group, sq_point_key(end_base));
#endif

  uint64_t last = 0, extra = 0, error = 0, missing = 0;
  for (uint32_t i = 0; i < builder->count; i++) {
    uint64_t bit = UINT64_C(1) << i;
    last |= (builder->pending[i].flags & 1) ? bit : 0;
    extra |= (builder->pending[i].flags & 2) ? bit : 0;
    error |= (builder->pending[i].flags & 4) ? bit : 0;
    missing |= (builder->pending[i].flags & 8) ? bit : 0;
  }
  set_group_flags(tree->data, tree->layout.last, group, last);
  set_group_flags(tree->data, tree->layout.extra, group, extra);
  set_group_flags(tree->data, tree->layout.error, group, error);
  set_group_flags(tree->data, tree->layout.missing, group, missing);

  uint32_t first = group * SQ_GROUP_SIZE;
  set_pending_column(tree->data, tree->layout.symbol, first, builder->count,
                     tree->layout.symbol_bits, builder->pending, PENDING_SYMBOL);
  set_pending_column(tree->data, tree->layout.grammar_symbol, first, builder->count,
                     tree->layout.symbol_bits, builder->pending, PENDING_GRAMMAR);
  set_pending_column(tree->data, tree->layout.field, first, builder->count,
                     tree->layout.field_bits, builder->pending, PENDING_FIELD);

  for (uint32_t i = 0; i < builder->count; i++) {
    const Pending *pending = &builder->pending[i];
    uint32_t slot = group * SQ_GROUP_SIZE + i;

    sq_set_u8(tree->data, tree->layout.span_delta, slot, pending->values.span - builder->base.span);
    sq_set_u8(tree->data, tree->layout.start_byte_delta, slot,
              pending->values.start_byte - builder->base.start_byte);
    sq_set_u16(tree->data, tree->layout.end_byte_delta, slot,
               builder->max.end_byte - pending->values.end_byte);
#if SQ_INCLUDE_POINTS
    uint16_t start_point =
        (uint16_t)((pending->values.start_row - builder->base.start_row) << 8) |
        (uint16_t)(pending->values.start_column - builder->base.start_column);
    uint16_t end_point = (uint16_t)((builder->max.end_row - pending->values.end_row) << 8) |
                         (uint16_t)(builder->max.end_column - pending->values.end_column);
    sq_set_u16(tree->data, tree->layout.start_point, slot, start_point);
    sq_set_u16(tree->data, tree->layout.end_point, slot, end_point);
#endif

    sq_set_u8(tree->data, tree->layout.supertype, slot, pending->super);
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

  if (builder->dictionary_count == builder->dictionary_capacity) {
    uint32_t capacity = builder->dictionary_capacity ? builder->dictionary_capacity * 2 : 8;
    if (capacity > 256) capacity = 256;
    uint64_t *next = realloc(builder->dictionary, (size_t)capacity * bytes);
    if (!next) {
      sq_fail(builder->error, SQ_ERROR_ALLOCATION);
      return false;
    }

    builder->dictionary = next;
    builder->dictionary_capacity = capacity;
  }

  memcpy(builder->dictionary + (size_t)builder->dictionary_count * builder->words, mask, bytes);
  *result = (uint8_t)builder->dictionary_count++;
  return true;
}

// Stage candidate extrema separately: a rejected node must not change the
// accepted group's bases. End bytes use a u16 delta; each point uses two u8
// component deltas joined into one lexicographically ordered u16 key.
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
  TSSymbol raw_symbol =
      node.context[3] ? (TSSymbol)node.context[3] : ts_node_grammar_symbol(node);
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
      .symbol = sq_encode_symbol(builder->tree, raw_symbol),
      .grammar = sq_encode_symbol(builder->tree, ts_node_grammar_symbol(node)),
      .field = frame->field,
      .flags = (!frame->later) | (ts_node_is_extra(node) << 1) | (ts_node_has_error(node) << 2) |
               (ts_node_is_missing(node) << 3),
  };
  const uint64_t *mask = builder->words == 1 ? &frame->mask
                         : builder->words > 1 ? builder->masks + frame->mask_offset
                                              : NULL;
  if (!intern_mask(builder, mask, &pending.super)) {
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
                       bool later, uint64_t mask, uint32_t mask_offset) {
  *frame = (Frame){.node = node,
                   .field = field,
                   .visible = visible,
                   .later = later,
                   .boundary = distance(builder),
                   .position_mark = builder->position_count,
                   .position_offset = SQ_NONE,
                   .field_mark = builder->field_count,
                   .field_offset = SQ_NONE,
                   .mask_mark = builder->mask_count,
                   .mask = mask,
                   .mask_offset = mask_offset,
                   .child_mask_offset = SQ_NONE};
  Subtree subtree = *(const Subtree *)node.id;
  uint32_t count = ts_subtree_child_count(subtree);
  if (count) {
    frame->children = ts_subtree_children(subtree);
    frame->aliases =
        ts_language_alias_sequence(builder->tree->language, subtree.ptr->production_id);
    if (count > 1 && !reserve_positions(builder, count, &frame->position_offset)) return false;

#if SQ_INCLUDE_POINTS
    PackPosition position = {ts_node_start_byte(node), ts_node_start_point(node)};
#else
    PackPosition position = ts_node_start_byte(node);
#endif
    for (uint32_t i = 0; i < count; i++) {
      if (i) {
#if SQ_INCLUDE_POINTS
        position = length_add(position, ts_subtree_padding(frame->children[i]));
#else
        position += ts_subtree_padding(frame->children[i]).bytes;
#endif
      }

      if (count == 1) {
        frame->inline_position = position;
      } else {
        builder->positions[frame->position_offset + i] = position;
      }
#if SQ_INCLUDE_POINTS
      position = length_add(position, ts_subtree_size(frame->children[i]));
#else
      position += ts_subtree_size(frame->children[i]).bytes;
#endif
      frame->structural += !ts_subtree_extra(frame->children[i]);
    }

    if (frame->structural && builder->tree->language->field_count) {
      const TSFieldMapEntry *map, *end;
      ts_language_field_map(builder->tree->language, subtree.ptr->production_id, &map, &end);
      const TSFieldMapEntry *first = map;
      while (first < end && first->inherited) first++;
      if (first < end) {
        if (!reserve_fields(builder, frame->structural, &frame->field_offset)) return false;
        for (map = first; map < end; map++) {
          if (!map->inherited && map->child_index < frame->structural &&
              !builder->fields[frame->field_offset + map->child_index]) {
            builder->fields[frame->field_offset + map->child_index] = map->field_id;
          }
        }
      }
    }

    if (builder->words == 1) {
      frame->child_mask = visible ? 0 : mask;
      TSSymbol own = node.context[3] ? (TSSymbol)node.context[3] : ts_node_grammar_symbol(node);
      uint32_t symbols =
          builder->tree->language->symbol_count + builder->tree->language->alias_count;
      if (own < symbols && builder->supertype_indexes[own]) {
        frame->child_mask |= UINT64_C(1) << (builder->supertype_indexes[own] - 1);
      }
    } else if (builder->words > 1) {
      if (!reserve_masks(builder, builder->words, &frame->child_mask_offset)) return false;
      uint64_t *child_mask = builder->masks + frame->child_mask_offset;
      if (visible) {
        memset(child_mask, 0, (size_t)builder->words * sizeof(uint64_t));
      } else {
        memcpy(child_mask, builder->masks + mask_offset,
               (size_t)builder->words * sizeof(uint64_t));
      }

      TSSymbol own = node.context[3] ? (TSSymbol)node.context[3] : ts_node_grammar_symbol(node);
      uint32_t symbols =
          builder->tree->language->symbol_count + builder->tree->language->alias_count;
      if (own < symbols && builder->supertype_indexes[own]) {
        uint32_t index = builder->supertype_indexes[own] - 1;
        child_mask[index / 64] |= UINT64_C(1) << (index % 64);
      }
    }

    frame->remaining = count;
  }

  return true;
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

  Builder builder = {
      .tree = result, .words = (result->supertype_count + 63) / 64, .error = error};
  size_t depth = 0, stack_capacity = 32;
  Frame *stack = malloc(stack_capacity * sizeof(Frame));
  uint32_t symbols = result->language->symbol_count + result->language->alias_count;
  if (builder.words) {
    builder.supertype_indexes = calloc(symbols, sizeof(uint16_t));
  }
  if (!stack || (builder.words && !builder.supertype_indexes)) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    goto failure;
  }
  for (uint32_t i = 0; i < result->supertype_count; i++) {
    builder.supertype_indexes[result->supertypes[i]] = (uint16_t)(i + 1);
  }

  uint32_t zero_mask_offset = SQ_NONE;
  if (builder.words > 1) {
    if (!reserve_masks(&builder, builder.words, &zero_mask_offset)) goto failure;
    memset(builder.masks + zero_mask_offset, 0, (size_t)builder.words * sizeof(uint64_t));
  }

  if (!init_frame(&builder, &stack[0], root, 0, true, false, 0, zero_mask_offset)) {
    goto failure;
  }

  depth = 1;
  while (depth) {
    Frame *frame = &stack[depth - 1];
    if (frame->remaining) {
      uint32_t index = --frame->remaining;
      const Subtree *child = &frame->children[index];
      bool extra = ts_subtree_extra(*child);
      if (!extra) {
        --frame->structural;
      }

      TSSymbol alias = extra || !frame->aliases ? 0 : frame->aliases[frame->structural];
      bool visible = alias || ts_subtree_visible(*child);
      bool later = frame->child_later || (!frame->visible && frame->later);
      frame->child_later |= visible || ts_subtree_visible_child_count(*child) > 0;

      // Hidden wrappers carry their incoming field; visible nodes start a new
      // child relationship. Extras interrupt field inheritance.
      TSFieldId field = frame->visible || extra ? 0 : frame->field;
      if (!extra && frame->field_offset != SQ_NONE) {
        TSFieldId direct = builder.fields[frame->field_offset + frame->structural];
        if (direct) field = direct;
      }

      uint32_t child_count = ts_subtree_child_count(*child);
      if (!child_count && !visible) {
        continue;
      }

#if SQ_INCLUDE_POINTS
      Length position = frame->position_offset != SQ_NONE
                            ? builder.positions[frame->position_offset + index]
                            : frame->inline_position;
#else
      uint32_t child_position = frame->position_offset != SQ_NONE
                                    ? builder.positions[frame->position_offset + index]
                                    : frame->inline_position;
      Length position = {.bytes = child_position};
#endif
      TSNode node = ts_node_new(tree, child, position, alias);
      uint64_t child_mask = frame->child_mask;
      uint32_t child_mask_offset = frame->child_mask_offset;
      if (!child_count) {
        Frame leaf = {.node = node,
                      .mask = child_mask,
                      .mask_offset = child_mask_offset,
                      .boundary = distance(&builder),
                      .field = field,
                      .visible = true,
                      .later = later};
        if (!emit(&builder, &leaf)) goto failure;
        continue;
      }

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

      if (!init_frame(&builder, &stack[depth], node, field, visible, later, child_mask,
                      child_mask_offset)) {
        goto failure;
      }

      depth++;
    } else {
      if (frame->visible && !emit(&builder, frame)) {
        goto failure;
      }

      builder.position_count = frame->position_mark;
      builder.field_count = frame->field_mark;
      builder.mask_count = frame->mask_mark;
      depth--;
    }
  }

  if (!close_group(&builder)) {
    goto failure;
  }

  uint64_t presence_bytes = options.symbol_presence ? sq_presence_size(builder.tree) : 0;
  if (sq_header(builder.tree)->group_count <= 32) presence_bytes = 0;
  uint64_t dictionary_bytes = builder.tree->supertype_count > 8
                                  ? (uint64_t)builder.dictionary_count * builder.words * 8
                                  : 0;
  uint64_t trailing_bytes = presence_bytes + dictionary_bytes;
  if (trailing_bytes > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    goto failure;
  }

  uint32_t final_capacity = options.repack ? sq_header(builder.tree)->group_count
                                           : sq_header(builder.tree)->group_capacity;
  if (!sq_prepare_final(&builder.tree, final_capacity, (uint32_t)trailing_bytes, error)) {
    goto failure;
  }

  if (options.symbol_presence && !sq_build_presence(builder.tree, error)) {
    goto failure;
  }

  if (builder.tree->supertype_count > 8 &&
      !sq_append_dictionary(builder.tree, builder.dictionary, builder.dictionary_count, error)) {
    goto failure;
  }

  free(stack);
  free(builder.positions);
  free(builder.fields);
  free(builder.masks);
  free(builder.supertype_indexes);
  free(builder.dictionary);
  return builder.tree;
failure:
  free(stack);
  free(builder.positions);
  free(builder.fields);
  free(builder.masks);
  free(builder.supertype_indexes);
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
