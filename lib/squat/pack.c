#include "internal.h"
#include "../src/tree.h"

// Only the current group's absolute values are staged. Frame coordinates are
// Point positions are computed once left-to-right, then consumed right-to-left
// because multiline extents lose the starting column. Point-free frames
// subtract byte lengths while traversing instead. No recursive C calls.
typedef struct {
  uint32_t span;
  uint32_t start_byte;
  uint32_t end_byte;
  uint32_t start_row;
  uint32_t end_row;
  uint32_t start_column;
  uint32_t end_column;
} PackValues;

typedef struct {
  PackValues values;
  uint16_t super;
} Pending;

// Write position in an ID column. Slab growth invalidates the address;
// cursors are recomputed when a group opens.
typedef struct {
  uint8_t *address;
} LaneCursor;

typedef Length PackPosition;

typedef struct {
  SQTree *tree;
  Pending pending[SQ_GROUP_SIZE];
  uint32_t count;

  // First slot of the open group. Mirrors group_count * SQ_GROUP_SIZE so the
  // per-node physical distance does not chase tree->data and the header.
  uint32_t slot_base;
  PackValues base, max;
  uint32_t words;
  PackPosition *positions;
  uint32_t position_count, position_capacity;
  const TSFieldId *fields;
  const DirectFieldSlice *production_fields;
  uint64_t *masks;
  uint32_t mask_count, mask_capacity;
  const uint16_t *supertype_indexes, *public_index;
  uint32_t symbol_space;

  // Language facts read for every frame, kept here so the hot paths do not
  // chase builder->tree->language each time.
  const TSLanguage *language;
  uint32_t symbol_count;
  bool small_supertypes, points;

  // Packed IDs and flags are written as each node is accepted rather than when
  // its group closes; see open_group.
  LaneCursor symbol_lane, grammar_lane, field_lane;
  uint64_t last_flags, extra_flags, missing_flags;
  bool group_has_error;
  uint32_t optional_flags;
  SQError *error;
} Builder;

// Everything emit reads. These are the raw values that ts_node_new would place
// in a TSNode; keeping them separately avoids constructing a node and calling
// exported accessors for every visible subtree during this internal traversal.
// A childless subtree fills only this, never a whole traversal frame.
typedef struct {
  const Subtree *subtree;
  PackPosition position;
  uint64_t mask;
  uint32_t mask_offset;

  // Lower physical boundary of this subtree. Reverse preorder lets the
  // builder append groups; growth never changes existing slot indexes.
  uint32_t boundary;
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

struct SQPackContext {
  // Retain only storage, never pending nodes or pointers into a completed slab.
  struct {
    PackPosition *positions;
    uint64_t *masks;
    uint32_t position_capacity, mask_capacity;
  } scratch;
  Frame *stack;
  size_t stack_capacity;
  uint8_t *presence;
  size_t presence_capacity;
};

// The traversal reads several fields of every raw subtree, and each subtree.h
// accessor repeats the inline/heap test. Decode the needed fields once.
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
  sq_fail(builder->error, SQ_ERROR_ALLOCATION);
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
  sq_fail(builder->error, SQ_ERROR_ALLOCATION);
  return false;
}

static uint32_t distance(const Builder *builder) {
  return builder->slot_base + builder->count;
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

static void start_lanes(LaneCursor *cursor, uint8_t *column, uint8_t bits, uint32_t slot) {
  (void)bits;
  cursor->address = column + (size_t)slot * 2;
}

static inline void put_lane(LaneCursor *cursor, uint32_t value) {
  sq_set_u16(cursor->address, 0, 0, (uint16_t)value);
  cursor->address += 2;
}

// Groups are filled once, in slot order, into zeroed storage, so every lane of
// the group being opened is still zero. Growth happens here instead of when the
// group closes: it is the same group index either way, so the capacity sequence
// and the final bytes are unchanged.
static bool open_group(Builder *builder) {
  SQHeader header = sq_read_header(builder->tree->data);
  if (header.group_count == header.group_capacity) {
    uint32_t capacity = header.group_capacity;
    if (capacity > UINT32_MAX / 2 || !sq_resize(&builder->tree, capacity * 2, builder->error)) {
      return false;
    }
  }

  SQTree *tree = builder->tree;
  start_lanes(&builder->symbol_lane, tree->data + tree->layout.symbol, 16,
              builder->slot_base);
  if (tree->grammar->symbols.separate)
    start_lanes(&builder->grammar_lane, tree->data + tree->layout.grammar, 16, builder->slot_base);
  start_lanes(&builder->field_lane, tree->data + tree->layout.field, 16,
              builder->slot_base);
  return true;
}

static bool close_group(Builder *builder) {
  if (!builder->count) {
    return true;
  }

  SQTree *tree = builder->tree;

  // Track actual extrema until the group closes so base selection cannot change
  // group boundaries. Zero spans avoid addition when the absolute values fit in
  // u8; start columns retain their minima for use as bounds. Revisit either
  // choice if zeroing or retaining the bound becomes useful to another operation.
  // End columns keep their actual maxima for the base-minus-delta encoding.
  if (builder->max.span <= UINT8_MAX) builder->base.span = 0;

  uint32_t group = sq_header_get(tree, group_count);
  sq_header_set(tree, group_count, group + 1);
  uint32_t first = group * SQ_GROUP_SIZE;
  sq_set_packed(tree->data, tree->layout.waste, group, SQ_WASTE_BITS,
                SQ_GROUP_SIZE - builder->count);
  sq_set_u32(tree->data, tree->layout.span_base, group, builder->base.span);
  sq_set_u32(tree->data, tree->layout.start_byte_base, group, builder->base.start_byte);
  sq_set_u32(tree->data, tree->layout.end_byte_base, group, builder->max.end_byte);
  if (builder->points) {
    TSPoint start_base = {builder->base.start_row, builder->base.start_column};
    TSPoint end_base = {builder->max.end_row, builder->max.end_column};
    sq_set_u64(tree->data, tree->layout.start_point_base, group, sq_point_key(start_base));
    sq_set_u64(tree->data, tree->layout.end_point_base, group, sq_point_key(end_base));
  }

  set_group_flags(tree->data, tree->layout.last, group, builder->last_flags);
  set_group_flags(tree->data, tree->layout.extra, group, builder->extra_flags);
  sq_set_bit(tree->data, tree->layout.error, group, builder->group_has_error);
  set_group_flags(tree->data, tree->layout.missing, group, builder->missing_flags);
  if (builder->extra_flags) builder->optional_flags |= SQ_EXTRAS;
  if (builder->group_has_error) builder->optional_flags |= SQ_ERRORS;
  if (builder->missing_flags) builder->optional_flags |= SQ_MISSING;
  builder->last_flags = builder->extra_flags = builder->missing_flags = 0;
  builder->group_has_error = false;

  // Stores through the slab's byte pointer may alias the tree, so the column
  // offsets and the destination base are read once rather than per slot.
  uint8_t *data = tree->data;
  uint8_t *span_delta = data + tree->layout.span_delta + first;
  uint8_t *start_byte_delta = data + tree->layout.start_byte_delta + first;
  uint8_t *end_byte_delta = data + tree->layout.end_byte_delta + (size_t)first * 2;
  uint32_t supertype_offset = tree->layout.supertype;
  uint8_t supertype_bits = 16;
  bool points = builder->points;
  uint8_t *start_point = points ? data + tree->layout.start_point + (size_t)first * 2 : NULL;
  uint8_t *end_point = points ? data + tree->layout.end_point + (size_t)first * 2 : NULL;
  const PackValues *base = &builder->base, *max = &builder->max;
  for (uint32_t i = 0; i < builder->count; i++) {
    const PackValues *values = &builder->pending[i].values;

    span_delta[i] = (uint8_t)(values->span - base->span);
    start_byte_delta[i] = (uint8_t)(values->start_byte - base->start_byte);
    uint16_t end_byte = (uint16_t)(max->end_byte - values->end_byte);
    sq_set_u16(end_byte_delta, 0, i, end_byte);
    if (points) {
      uint16_t start = (uint16_t)((values->start_row - base->start_row) << 8) |
                       (uint16_t)(values->start_column - base->start_column);
      uint16_t end = (uint16_t)((max->end_row - values->end_row) << 8) |
                     (uint16_t)(max->end_column - values->end_column);
      sq_set_u16(start_point, 0, i, start);
      sq_set_u16(end_point, 0, i, end);
    }

    if (supertype_bits == 16) {
      sq_set_u16(data, supertype_offset, first + i, builder->pending[i].super);
    } else if (supertype_bits == 8) {
      sq_set_u8(data, supertype_offset, first + i, (uint8_t)builder->pending[i].super);
    } else if (supertype_bits) {
      sq_set_packed(data, supertype_offset, first + i, supertype_bits, builder->pending[i].super);
    }
  }

  builder->count = 0;
  builder->slot_base += SQ_GROUP_SIZE;
  return true;
}

static bool intern_mask(Builder *builder, const uint64_t *mask, uint16_t *result) {
  if (builder->small_supertypes) {
    *result = mask ? (uint8_t)mask[0] : 0;
    return true;
  }

  uint32_t id = sq_supertype_mask_id(builder->tree->supertype_grammar, mask);
  if (id == SQ_NONE) {
    // Never introduce order-dependent IDs if a grammar/runtime combination
    // violates the conservative analysis.
    sq_fail(builder->error, SQ_ERROR_LANGUAGE);
    return false;
  }
  *result = (uint16_t)id;
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
  // emit closes a full group before staging the candidate.
  if (!builder->count) {
    base->span = max->span = value->span;
    base->start_byte = max->start_byte = value->start_byte;
    base->end_byte = max->end_byte = value->end_byte;
    if (builder->points) {
      base->start_row = max->start_row = value->start_row;
      base->end_row = max->end_row = value->end_row;
      base->start_column = max->start_column = value->start_column;
      base->end_column = max->end_column = value->end_column;
    }
    return true;
  }

  if (!extend_range(value->span, builder->base.span, builder->max.span, UINT8_MAX, &base->span,
                    &max->span))
    return false;
  // Reverse preorder visits later siblings before earlier ones, then their
  // parent. Starts therefore never increase, including empty/missing nodes.
  // The candidate is the new minimum and the first accepted start stays maximal.
  base->start_byte = value->start_byte;
  max->start_byte = builder->max.start_byte;
  if (max->start_byte - base->start_byte > UINT8_MAX) return false;
  if (!extend_range(value->end_byte, builder->base.end_byte, builder->max.end_byte, UINT16_MAX,
                    &base->end_byte, &max->end_byte))
    return false;
  if (builder->points) {
    base->start_row = value->start_row;
    max->start_row = builder->max.start_row;
    if (max->start_row - base->start_row > UINT8_MAX) return false;
    if (!extend_range(value->end_row, builder->base.end_row, builder->max.end_row, UINT8_MAX,
                      &base->end_row, &max->end_row))
      return false;
    if (!extend_range(value->start_column, builder->base.start_column, builder->max.start_column,
                      UINT8_MAX, &base->start_column, &max->start_column))
      return false;
    if (!extend_range(value->end_column, builder->base.end_column, builder->max.end_column,
                      UINT8_MAX, &base->end_column, &max->end_column))
      return false;
  }
  return true;
}

static inline uint32_t encode_symbol(const Builder *builder, TSSymbol symbol) {
  return symbol == ts_builtin_sym_error          ? builder->symbol_space - 2
         : symbol == ts_builtin_sym_error_repeat ? builder->symbol_space - 1
                                                 : symbol;
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

  // ts_subtree_error_cost reports a positive constant for a missing subtree
  // regardless of the stored cost, so the recorded flag is the same predicate.
  bool has_error = missing || error_cost > 0;
  uint32_t start_byte = frame->position.bytes;
  Length end = {0};
  if (builder->points) end = length_add(frame->position, size);
  uint16_t super;
  const uint64_t *mask = builder->words == 1 ? &frame->mask
                         : builder->words > 1 ? builder->masks + frame->mask_offset
                                              : NULL;
  if (!intern_mask(builder, mask, &super)) {
    return false;
  }

  // Fill the staged slot in place. Assembling a local Pending and copying it
  // here stalled on store forwarding: narrow field stores were immediately
  // reloaded as wide vectors. A rejected candidate is simply rewritten at the
  // reopened group's first slot.
  for (;;) {
    // Staging needs a free slot, so a full group closes before the candidate is
    // written. Closing a full group leaves the physical distance unchanged.
    if (builder->count == SQ_GROUP_SIZE && !close_group(builder)) {
      return false;
    }

    if (distance(builder) >= UINT32_MAX - SQ_GROUP_SIZE) {
      sq_fail(builder->error, SQ_ERROR_OVERFLOW);
      return false;
    }

    Pending *slot = &builder->pending[builder->count];
    slot->values.start_byte = start_byte;
    slot->values.end_byte = start_byte + size.bytes;
    if (builder->points) {
      slot->values.start_row = frame->position.extent.row;
      slot->values.end_row = end.extent.row;
      slot->values.start_column = frame->position.extent.column;
      slot->values.end_column = end.extent.column;
    }

    // Retrying after close_group includes newly abandoned slots in the span.
    // The saved boundary still marks the same lower physical slot.
    slot->values.span = distance(builder) - frame->boundary;
    PackValues base, max;
    if (group_fits(builder, &slot->values, &base, &max)) {
      if (!builder->count && !open_group(builder)) {
        return false;
      }

      if (builder->points) {
        builder->base = base;
        builder->max = max;
      } else {
        builder->base.span = base.span;
        builder->base.start_byte = base.start_byte;
        builder->base.end_byte = base.end_byte;
        builder->max.span = max.span;
        builder->max.start_byte = max.start_byte;
        builder->max.end_byte = max.end_byte;
      }
      uint32_t bit = builder->count;
      builder->last_flags |= (uint64_t)!frame->later << bit;
      builder->extra_flags |= (uint64_t)extra << bit;
      builder->group_has_error |= has_error;
      builder->missing_flags |= (uint64_t)missing << bit;
      uint32_t grammar_id = encode_symbol(builder, grammar);
      const SQGrammar *prepared = builder->tree->grammar;
      uint32_t code = frame->alias
          ? sq_symbol_code(prepared, builder->public_index[frame->alias], grammar_id)
          : prepared->symbols.default_codes[grammar_id];
      if (code == SQ_NONE) {
        sq_fail(builder->error, SQ_ERROR_LANGUAGE);
        return false;
      }
      put_lane(&builder->symbol_lane, code);
      if (prepared->symbols.separate) {
        put_lane(&builder->grammar_lane, grammar_id);
        if (grammar_id != code) builder->optional_flags |= SQ_SEPARATE_GRAMMAR;
      }
      put_lane(&builder->field_lane, frame->field);
      slot->super = super;
      builder->count++;
      return true;
    }

    if (!close_group(builder)) {
      return false;
    }
  }
}

static bool init_frame(Builder *builder, Frame *frame, const Subtree *subtree_pointer,
                       PackPosition position, TSSymbol alias, TSFieldId field, bool visible,
                       bool later, uint64_t mask, uint32_t mask_offset) {
  frame->node.subtree = subtree_pointer;
  frame->node.position = position;
  frame->node.mask = mask;
  frame->node.mask_offset = mask_offset;
  frame->node.boundary = distance(builder);
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
    frame->aliases =
        ts_language_alias_sequence(builder->language, subtree.ptr->production_id);
    PackPosition *positions = NULL;
    if (builder->points) {
      if (count > 1 && !reserve_positions(builder, count, &frame->position_offset)) return false;
      // Extents cannot be recovered by subtracting a multiline child's size.
      positions = count == 1 ? &frame->inline_position
                             : builder->positions + frame->position_offset;
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
        memcpy(child_mask, builder->masks + mask_offset,
               (size_t)builder->words * sizeof(uint64_t));
      }

      TSSymbol own = alias ? alias : ts_subtree_symbol(subtree);
      if (own < builder->symbol_count && builder->supertype_indexes[own]) {
        uint32_t index = builder->supertype_indexes[own] - 1;
        child_mask[index / 64] |= UINT64_C(1) << (index % 64);
      }
    }

    frame->remaining = count;
  }

  return true;
}

SQPackOptions sq_pack_options_default(void) {
  return (SQPackOptions){.repack = false, .symbol_presence = true, .points = true};
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
__attribute__((noinline))
static void descend_hidden(const Builder *builder, const TSLanguage *language, uint32_t symbols,
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

void sq_pack_context_trim(SQPackContext *context) {
  if (!context) return;
  free(context->stack);
  free(context->scratch.positions);
  free(context->scratch.masks);
  free(context->presence);
  memset(&context->scratch, 0, sizeof(context->scratch));
  context->stack = NULL;
  context->stack_capacity = 0;
  context->presence = NULL;
  context->presence_capacity = 0;
}

void sq_pack_context_delete(SQPackContext *context) {
  if (!context) return;
  sq_pack_context_trim(context);
  free(context);
}

static SQGrammar *grammar_new(const TSLanguage *language, const void *grammar_cache,
                               size_t grammar_cache_length, SQError *error) {
  sq_fail(error, SQ_OK);
  if (language && ((uint64_t)language->symbol_count + language->alias_count + 1 > UINT16_MAX ||
                   language->field_count > UINT16_MAX)) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    return NULL;
  }
  if (!sq_language_compatible(language)) {
    sq_fail(error, SQ_ERROR_LANGUAGE);
    return NULL;
  }
  SQGrammar *grammar = calloc(1, sizeof(SQGrammar));
  if (grammar) atomic_init(&grammar->references, 1);
  if (!grammar) goto allocation;
  grammar->language = ts_language_copy(language);
  if (!sq_symbol_table_init(language, &grammar->symbols, error)) {
    sq_grammar_delete(grammar);
    return NULL;
  }
  uint32_t symbols = language->symbol_count + language->alias_count;
  size_t space = (size_t)symbols + 2;
  grammar->supertypes = calloc(3 * space, sizeof(uint16_t));
  if (!grammar->supertypes) {
    sq_grammar_delete(grammar);
    goto allocation;
  }
  grammar->supertype_indexes = grammar->supertypes + space;
  grammar->public_index = grammar->supertypes + 2 * space;
  for (uint32_t symbol = 0; symbol < symbols; symbol++) {
    if (language->symbol_metadata[symbol].supertype) {
      grammar->supertypes[grammar->supertype_count++] = (TSSymbol)symbol;
      grammar->supertype_indexes[symbol] = (uint16_t)grammar->supertype_count;
    }
    TSSymbol public = ts_language_public_symbol(language, (TSSymbol)symbol);
    grammar->public_index[symbol] = public == ts_builtin_sym_error ? symbols
        : public == ts_builtin_sym_error_repeat ? symbols + 1 : public;
  }
  if (grammar->supertype_count > 8) {
    grammar->supertype_grammar = grammar_cache
        ? sq_supertype_grammar_new_cached(language, grammar->supertype_count, grammar_cache,
                                          grammar_cache_length, error)
        : sq_supertype_grammar_new(language, grammar->supertype_count, error);
    if (!grammar->supertype_grammar) {
      sq_grammar_delete(grammar);
      return NULL;
    }
  } else if (grammar_cache_length) {
    sq_grammar_delete(grammar);
    sq_fail(error, SQ_ERROR_INVALID_SLAB);
    return NULL;
  }
  grammar->public_index[symbols] = (uint16_t)symbols;
  grammar->public_index[symbols + 1] = (uint16_t)(symbols + 1);
  if (language->field_count && language->production_id_count) {
    grammar->production_fields = calloc(language->production_id_count, sizeof(DirectFieldSlice));
    if (!grammar->production_fields) goto grammar_allocation;
    uint64_t total = 0;
    for (uint32_t id = 0; id < language->production_id_count; id++) {
      const TSFieldMapEntry *map, *end;
      ts_language_field_map(language, id, &map, &end);
      uint32_t length = 0;
      for (; map < end; map++) {
        if (!map->inherited && (uint32_t)map->child_index + 1 > length)
          length = (uint32_t)map->child_index + 1;
      }
      grammar->production_fields[id] = (DirectFieldSlice){(uint32_t)total, length};
      total += length;
      if (total > UINT32_MAX || total > SIZE_MAX / sizeof(TSFieldId))
        goto grammar_allocation;
    }
    if (total) {
      grammar->direct_fields = calloc((size_t)total, sizeof(TSFieldId));
      if (!grammar->direct_fields) goto grammar_allocation;
      for (uint32_t id = 0; id < language->production_id_count; id++) {
        const TSFieldMapEntry *map, *end;
        ts_language_field_map(language, id, &map, &end);
        uint32_t offset = grammar->production_fields[id].offset;
        for (; map < end; map++) {
          if (!map->inherited && !grammar->direct_fields[offset + map->child_index])
            grammar->direct_fields[offset + map->child_index] = map->field_id;
        }
      }
    }
  }
  return grammar;
grammar_allocation:
  sq_grammar_delete(grammar);
allocation:
  sq_fail(error, SQ_ERROR_ALLOCATION);
  return NULL;
}

SQGrammar *sq_grammar_new(const TSLanguage *language, SQError *error) {
  return grammar_new(language, NULL, 0, error);
}

SQGrammar *sq_grammar_new_with_cache(const TSLanguage *language, const void *bytes,
                                    size_t length, SQError *error) {
  if (!bytes) {
    sq_fail(error, SQ_ERROR_INVALID_SLAB);
    return NULL;
  }
  return grammar_new(language, bytes, length, error);
}

SQGrammar *sq_grammar_copy(SQGrammar *grammar) {
  if (grammar && atomic_fetch_add_explicit(&grammar->references, 1, memory_order_relaxed) >= SIZE_MAX / 2)
    abort();
  return grammar;
}

void sq_grammar_delete(SQGrammar *grammar) {
  if (!grammar || atomic_fetch_sub_explicit(&grammar->references, 1, memory_order_acq_rel) != 1)
    return;
  sq_supertype_grammar_delete(grammar->supertype_grammar);
  ts_language_delete(grammar->language);
  sq_symbol_table_delete(&grammar->symbols);
  free(grammar->supertypes);
  free(grammar->production_fields);
  free(grammar->direct_fields);
  free(grammar);
}

const TSLanguage *sq_grammar_language(const SQGrammar *grammar) {
  return grammar ? grammar->language : NULL;
}

uint32_t sq_grammar_cache_size(const SQGrammar *grammar) {
  size_t size = grammar ? sq_supertype_grammar_cache_size(grammar->supertype_grammar) : 0;
  return size <= UINT32_MAX ? (uint32_t)size : 0;
}

bool sq_grammar_copy_cache(const SQGrammar *grammar, void *destination,
                           size_t length, SQError *error) {
  if (!grammar) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }
  return sq_supertype_grammar_copy_cache(grammar->supertype_grammar, destination, length, error);
}

SQPackContext *sq_pack_context_new(SQError *error) {
  sq_fail(error, SQ_OK);
  SQPackContext *context = calloc(1, sizeof(SQPackContext));
  if (!context) sq_fail(error, SQ_ERROR_ALLOCATION);
  return context;
}

static SQTree *pack_tree(SQPackContext *context, SQGrammar *grammar, const TSTree *tree,
                         SQPackOptions options, SQError *error) {
  sq_fail(error, SQ_OK);
  if (!grammar || !tree) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }
  if (grammar->language != ts_tree_language(tree)) {
    sq_fail(error, SQ_ERROR_LANGUAGE);
    return NULL;
  }

  TSNode root = ts_tree_root_node(tree);
  uint32_t capacity = options.initial_group_capacity;
  if (!capacity) {
    // Reserve for 75% occupancy; scale the estimate with experimental groups.
    uint32_t expected_nodes_per_group = SQ_GROUP_SIZE * 3 / 4;
    capacity = ts_node_descendant_count(root) / expected_nodes_per_group + 1;
  }

  SQTree *result = sq_allocate(grammar, capacity, options.points, error);
  if (!result) {
    return NULL;
  }

  Builder builder = {.tree = result,
                     .words = (result->supertype_count + 63) / 64,
                     .symbol_space = sq_symbols(result),
                     .language = result->language,
                     .symbol_count = result->language->symbol_count + result->language->alias_count,
                     .small_supertypes = result->supertype_count <= 8,
                     .points = options.points,
                     .error = error};
  size_t depth = 0, stack_capacity = 32;
  Frame *stack = NULL;
  builder.positions = context->scratch.positions;
  builder.position_capacity = context->scratch.position_capacity;
  builder.fields = grammar->direct_fields;
  builder.production_fields = grammar->production_fields;
  builder.masks = context->scratch.masks;
  builder.mask_capacity = context->scratch.mask_capacity;
  builder.supertype_indexes = grammar->supertype_indexes;
  builder.public_index = grammar->public_index;
  stack = context->stack;
  if (stack) stack_capacity = context->stack_capacity;
  if (!stack) stack = malloc(stack_capacity * sizeof(Frame));
  const TSLanguage *language = result->language;
  uint32_t symbols = language->symbol_count + language->alias_count;
  if (!stack) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    goto failure;
  }

  uint32_t zero_mask_offset = SQ_NONE;
  if (builder.words > 1) {
    if (!reserve_masks(&builder, builder.words, &zero_mask_offset)) goto failure;
    memset(builder.masks + zero_mask_offset, 0, (size_t)builder.words * sizeof(uint64_t));
  }

  PackPosition root_position = {
      .bytes = root.context[0],
      .extent = {root.context[1], root.context[2]},
  };
  if (!init_frame(&builder, &stack[0], (const Subtree *)root.id, root_position,
                  (TSSymbol)root.context[3], 0, true, false, 0, zero_mask_offset)) {
    goto failure;
  }

  depth = 1;
  while (depth) {
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

      PackPosition position =
          builder.points
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
                         .boundary = distance(&builder),
                         .field = field,
                         .alias = alias,
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

      if (!init_frame(&builder, &stack[depth], child, position, alias, field, visible, later,
                      child_mask, child_mask_offset)) {
        goto failure;
      }

      depth++;
    } else {
      if (frame->visible && !emit(&builder, &frame->node)) {
        goto failure;
      }

      builder.position_count = frame->position_mark;
      builder.mask_count = frame->mask_mark;
      depth--;
    }
  }

  if (!close_group(&builder)) {
    goto failure;
  }

  uint64_t presence_bytes = options.symbol_presence ? sq_presence_size(builder.tree) : 0;
  if (sq_header_get(builder.tree, group_count) <= 32) presence_bytes = 0;
  uint64_t trailing_bytes = presence_bytes;
  if (trailing_bytes > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    goto failure;
  }

  uint32_t final_capacity = options.repack ? sq_header_get(builder.tree, group_count)
                                           : sq_header_get(builder.tree, group_capacity);
  if (!sq_prepare_final(&builder.tree, final_capacity, (uint32_t)trailing_bytes,
                         builder.optional_flags, error)) {
    goto failure;
  }

  if (options.symbol_presence) {
    bool ok = sq_build_presence_cached(builder.tree, &context->presence,
                                       &context->presence_capacity, error);
    if (!ok) goto failure;
  }

  goto cleanup;
failure:
  sq_tree_delete(builder.tree);
  builder.tree = NULL;
cleanup:
  result = builder.tree;
  context->scratch.positions = builder.positions;
  context->scratch.position_capacity = builder.position_capacity;
  context->scratch.masks = builder.masks;
  context->scratch.mask_capacity = builder.mask_capacity;
  context->stack = stack;
  context->stack_capacity = stack_capacity;
  return result;
}

SQTree *sq_tree_pack(SQGrammar *grammar, const TSTree *tree, SQPackOptions options, SQError *error) {
  if (!grammar) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }
  SQPackContext context = {0};
  SQTree *result = pack_tree(&context, grammar, tree, options, error);
  sq_pack_context_trim(&context);
  return result;
}

SQTree *sq_pack_context_pack(SQPackContext *context, SQGrammar *grammar, const TSTree *tree,
                             SQPackOptions options, SQError *error) {
  if (!context) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }
  return pack_tree(context, grammar, tree, options, error);
}

SQTree *sq_tree_parse(SQGrammar *grammar, TSParser *parser, const char *source, uint32_t length, SQPackOptions options,
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

  SQTree *packed = sq_tree_pack(grammar, tree, options, error);
  ts_tree_delete(tree);
  return packed;
}
