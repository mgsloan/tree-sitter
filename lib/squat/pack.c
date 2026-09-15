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

// Write position in one packed column: the word holding the next slot's lane
// and that lane's shift. Valid until the slab grows, which only happens when a
// group opens, and cursors are recomputed there.
typedef struct {
  uint8_t *address;
  uint32_t shift, limit;
  uint8_t bits;
} LaneCursor;

typedef Length PackPosition;

typedef struct {
  uint32_t offset, length;
} DirectFieldSlice;

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
  TSFieldId *fields;
  const DirectFieldSlice *production_fields;
  uint32_t field_count, field_capacity;
  uint64_t *masks;
  uint32_t mask_count, mask_capacity;
  uint16_t *supertype_indexes;
  uint32_t symbol_space;

  // Language facts read for every frame, kept here so the hot paths do not
  // chase builder->tree->language each time.
  const TSLanguage *language;
  uint32_t symbol_count, language_field_count;
  bool small_supertypes, points;

  // Packed IDs and flags are written as each node is accepted rather than when
  // its group closes; see open_group.
  LaneCursor symbol_lane, field_lane;
  struct GrammarOverride { uint32_t slot, symbol; } *overrides;
  uint32_t override_count, override_capacity;
  uint64_t last_flags, extra_flags, error_flags, missing_flags;
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
  uint32_t field_mark, field_offset, field_length;
  uint32_t mask_mark, child_mask_offset;
  uint32_t remaining, structural;
  bool visible, child_later;
} Frame;

struct SQPackContext {
  const TSLanguage *language;
  TSSymbol *supertypes;
  uint32_t supertype_count;
  uint16_t *public_index;
  DirectFieldSlice *production_fields;
  TSFieldId *direct_fields;
  SQSupertypeGrammar *supertype_grammar;
  // Retain only storage, never pending nodes or pointers into a completed slab.
  struct {
    PackPosition *positions;
    uint64_t *masks;
    uint16_t *supertype_indexes;
    struct GrammarOverride *overrides;
    uint32_t override_capacity;
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
  uint32_t lanes = 64 / bits;
  cursor->address = column + (uint64_t)(slot / lanes) * 8;
  cursor->shift = slot % lanes * bits;
  cursor->limit = 64 - bits;
  cursor->bits = bits;
}

// Lanes of a not-yet-written group are zero, so a value is ORed in. Partial
// boundary words keep the lanes of the previous group. The next lane starts a
// new word once its shift would pass the last non-straddling position.
static inline void put_lane(LaneCursor *cursor, uint32_t value) {
  uint64_t word;
  memcpy(&word, cursor->address, sizeof(word));
  word |= (uint64_t)value << cursor->shift;
  memcpy(cursor->address, &word, sizeof(word));
  cursor->shift += cursor->bits;
  if (cursor->shift > cursor->limit) {
    cursor->shift = 0;
    cursor->address += 8;
  }
}

// Groups are filled once, in slot order, into zeroed storage, so every lane of
// the group being opened is still zero. Growth happens here instead of when the
// group closes: it is the same group index either way, so the capacity sequence
// and the final bytes are unchanged.
static bool open_group(Builder *builder) {
  SQHeader *header = sq_header(builder->tree);
  if (header->group_count == header->group_capacity) {
    uint32_t capacity = header->group_capacity;
    if (capacity > UINT32_MAX / 2 || !sq_resize(&builder->tree, capacity * 2, builder->error)) {
      return false;
    }
  }

  SQTree *tree = builder->tree;
  start_lanes(&builder->symbol_lane, tree->data + tree->layout.symbol, tree->layout.symbol_bits,
              builder->slot_base);
  if (tree->layout.field_bits) {
    start_lanes(&builder->field_lane, tree->data + tree->layout.field, tree->layout.field_bits,
                builder->slot_base);
  }
  return true;
}

static bool close_group(Builder *builder) {
  if (!builder->count) {
    return true;
  }

  SQTree *tree = builder->tree;
  SQHeader *header = sq_header(tree);

  // Track actual extrema until the group closes so zero-base selection cannot
  // change group boundaries. These bases need not retain actual minima: revisit
  // this choice if minimum subtree spans or column positions become useful.
  // End columns keep their actual maxima for the base-minus-delta encoding.
  if (builder->max.span <= UINT8_MAX) builder->base.span = 0;
  if (builder->points && builder->max.start_column <= UINT8_MAX) {
    builder->base.start_column = 0;
  }

  uint32_t group = header->group_count++;
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
  set_group_flags(tree->data, tree->layout.error, group, builder->error_flags);
  set_group_flags(tree->data, tree->layout.missing, group, builder->missing_flags);
  builder->last_flags = builder->extra_flags = builder->error_flags = builder->missing_flags = 0;

  // Stores through the slab's byte pointer may alias the tree, so the column
  // offsets and the destination base are read once rather than per slot.
  uint8_t *data = tree->data;
  uint8_t *span_delta = data + tree->layout.span_delta + first;
  uint8_t *start_byte_delta = data + tree->layout.start_byte_delta + first;
  uint8_t *end_byte_delta = data + tree->layout.end_byte_delta + (size_t)first * 2;
  uint32_t supertype_offset = tree->layout.supertype;
  uint8_t supertype_bits = tree->layout.supertype_bits;
  bool points = builder->points;
  uint8_t *start_point = points ? data + tree->layout.start_point + (size_t)first * 2 : NULL;
  uint8_t *end_point = points ? data + tree->layout.end_point + (size_t)first * 2 : NULL;
  const PackValues *base = &builder->base, *max = &builder->max;
  for (uint32_t i = 0; i < builder->count; i++) {
    const PackValues *values = &builder->pending[i].values;

    span_delta[i] = (uint8_t)(values->span - base->span);
    start_byte_delta[i] = (uint8_t)(values->start_byte - base->start_byte);
    uint16_t end_byte = (uint16_t)(max->end_byte - values->end_byte);
    memcpy(end_byte_delta + (size_t)i * 2, &end_byte, sizeof(end_byte));
    if (points) {
      uint16_t start = (uint16_t)((values->start_row - base->start_row) << 8) |
                       (uint16_t)(values->start_column - base->start_column);
      uint16_t end = (uint16_t)((max->end_row - values->end_row) << 8) |
                     (uint16_t)(max->end_column - values->end_column);
      memcpy(start_point + (size_t)i * 2, &start, sizeof(start));
      memcpy(end_point + (size_t)i * 2, &end, sizeof(end));
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
  TSSymbol raw_symbol = frame->alias ? frame->alias : grammar;
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
      builder->error_flags |= (uint64_t)has_error << bit;
      builder->missing_flags |= (uint64_t)missing << bit;
      if (raw_symbol != grammar) {
        if (builder->override_count == builder->override_capacity) {
          uint64_t capacity = builder->override_capacity ? (uint64_t)builder->override_capacity * 2 : 32;
          if (capacity > UINT32_MAX || capacity > SIZE_MAX / sizeof(*builder->overrides)) {
            sq_fail(builder->error, SQ_ERROR_OVERFLOW);
            return false;
          }
          void *next = realloc(builder->overrides, (size_t)capacity * sizeof(*builder->overrides));
          if (!next) {
            sq_fail(builder->error, SQ_ERROR_ALLOCATION);
            return false;
          }
          builder->overrides = next;
          builder->override_capacity = (uint32_t)capacity;
        }
        builder->overrides[builder->override_count++] =
            (struct GrammarOverride){distance(builder), encode_symbol(builder, grammar)};
      }
      put_lane(&builder->symbol_lane, encode_symbol(builder, raw_symbol));
      if (builder->tree->layout.field_bits) {
        put_lane(&builder->field_lane, frame->field);
      } else {
        ts_assert(frame->field == 0);
      }
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
  frame->field_mark = builder->field_count;
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
    } else if (frame->structural && builder->language_field_count) {
      const TSFieldMapEntry *map, *end;
      ts_language_field_map(builder->language, subtree.ptr->production_id, &map, &end);
      const TSFieldMapEntry *first = map;
      while (first < end && first->inherited) first++;
      if (first < end) {
        if (!reserve_fields(builder, frame->structural, &frame->field_offset)) return false;
        frame->field_length = frame->structural;
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
      } else if (language->field_count) {
        const TSFieldMapEntry *map, *end;
        ts_language_field_map(language, data->production_id, &map, &end);
        for (; map < end; map++) {
          if (!map->inherited && map->child_index == 0) {
            inner_field = map->field_id;
            break;
          }
        }
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
  free(context->scratch.overrides);
  free(context->presence);
  uint16_t *indexes = context->scratch.supertype_indexes;
  memset(&context->scratch, 0, sizeof(context->scratch));
  context->scratch.supertype_indexes = indexes;
  context->stack = NULL;
  context->stack_capacity = 0;
  context->presence = NULL;
  context->presence_capacity = 0;
}

void sq_pack_context_delete(SQPackContext *context) {
  if (!context) return;
  sq_pack_context_trim(context);
  ts_language_delete(context->language);
  free(context->supertypes);
  free(context->production_fields);
  free(context->direct_fields);
  sq_supertype_grammar_release(context->supertype_grammar);
  free(context);
}

SQPackContext *sq_pack_context_new(const TSLanguage *language, SQError *error) {
  sq_fail(error, SQ_OK);
  if (!sq_language_compatible(language)) {
    sq_fail(error, SQ_ERROR_LANGUAGE);
    return NULL;
  }
  SQPackContext *context = calloc(1, sizeof(SQPackContext));
  if (!context) goto allocation;
  context->language = ts_language_copy(language);
  uint32_t symbols = language->symbol_count + language->alias_count;
  size_t space = (size_t)symbols + 2;
  context->supertypes = calloc(3 * space, sizeof(uint16_t));
  if (!context->supertypes) {
    sq_pack_context_delete(context);
    goto allocation;
  }
  context->scratch.supertype_indexes = context->supertypes + space;
  context->public_index = context->supertypes + 2 * space;
  for (uint32_t symbol = 0; symbol < symbols; symbol++) {
    if (language->symbol_metadata[symbol].supertype) {
      context->supertypes[context->supertype_count++] = (TSSymbol)symbol;
      context->scratch.supertype_indexes[symbol] = (uint16_t)context->supertype_count;
    }
    TSSymbol public = ts_language_public_symbol(language, (TSSymbol)symbol);
    context->public_index[symbol] = public == ts_builtin_sym_error ? symbols
        : public == ts_builtin_sym_error_repeat ? symbols + 1 : public;
  }
  if (context->supertype_count > 8) {
    context->supertype_grammar = sq_supertype_grammar_acquire(language, context->supertype_count, error);
    if (!context->supertype_grammar) {
      sq_pack_context_delete(context);
      return NULL;
    }
  }
  context->public_index[symbols] = (uint16_t)symbols;
  context->public_index[symbols + 1] = (uint16_t)(symbols + 1);
  // Immutable grammar metadata survives trim. Ordinary one-shot packing keeps
  // its per-frame scratch so tiny trees do not pay for the entire grammar.
  if (language->field_count && language->production_id_count) {
    context->production_fields = calloc(language->production_id_count, sizeof(DirectFieldSlice));
    if (!context->production_fields) goto context_allocation;
    uint64_t total = 0;
    for (uint32_t id = 0; id < language->production_id_count; id++) {
      const TSFieldMapEntry *map, *end;
      ts_language_field_map(language, id, &map, &end);
      uint32_t length = 0;
      for (; map < end; map++) {
        if (!map->inherited && (uint32_t)map->child_index + 1 > length)
          length = (uint32_t)map->child_index + 1;
      }
      context->production_fields[id] = (DirectFieldSlice){(uint32_t)total, length};
      total += length;
      if (total > UINT32_MAX || total > SIZE_MAX / sizeof(TSFieldId))
        goto context_allocation;
    }
    if (total) {
      context->direct_fields = calloc((size_t)total, sizeof(TSFieldId));
      if (!context->direct_fields) goto context_allocation;
      for (uint32_t id = 0; id < language->production_id_count; id++) {
        const TSFieldMapEntry *map, *end;
        ts_language_field_map(language, id, &map, &end);
        uint32_t offset = context->production_fields[id].offset;
        for (; map < end; map++) {
          if (!map->inherited && !context->direct_fields[offset + map->child_index])
            context->direct_fields[offset + map->child_index] = map->field_id;
        }
      }
    }
  }
  return context;
context_allocation:
  sq_pack_context_delete(context);
allocation:
  sq_fail(error, SQ_ERROR_ALLOCATION);
  return NULL;
}

static SQTree *pack_tree(SQPackContext *context, const TSTree *tree,
                         SQPackOptions options, SQError *error) {
  sq_fail(error, SQ_OK);
  if (!tree) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }
  if (context && context->language != ts_tree_language(tree)) {
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

  SQTree *result = context
      ? sq_allocate_cached(context->language, capacity, context->supertypes,
                           context->supertype_count, options.points, error)
      : sq_allocate(ts_tree_language(tree), capacity, options.points, error);
  if (!result) {
    return NULL;
  }

  Builder builder = {.tree = result,
                     .words = (result->supertype_count + 63) / 64,
                     .symbol_space = sq_symbols(result),
                     .language = result->language,
                     .symbol_count = result->language->symbol_count + result->language->alias_count,
                     .language_field_count = result->language->field_count,
                     .small_supertypes = result->supertype_count <= 8,
                     .points = options.points,
                     .error = error};
  size_t depth = 0, stack_capacity = 32;
  Frame *stack = NULL;
  if (context) {
    builder.positions = context->scratch.positions;
    builder.position_capacity = context->scratch.position_capacity;
    builder.fields = context->direct_fields;
    builder.production_fields = context->production_fields;
    builder.masks = context->scratch.masks;
    builder.mask_capacity = context->scratch.mask_capacity;
    builder.overrides = context->scratch.overrides;
    builder.override_capacity = context->scratch.override_capacity;
    builder.supertype_indexes = context->scratch.supertype_indexes;
    stack = context->stack;
    if (stack) stack_capacity = context->stack_capacity;
  }
  if (!stack) stack = malloc(stack_capacity * sizeof(Frame));
  const TSLanguage *language = result->language;
  uint32_t symbols = language->symbol_count + language->alias_count;
  if (builder.words && !context) {
    builder.supertype_indexes = calloc(symbols, sizeof(uint16_t));
  }
  if (!stack || (builder.words && !builder.supertype_indexes)) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    goto failure;
  }
  for (uint32_t i = 0; !context && i < result->supertype_count; i++) {
    builder.supertype_indexes[result->supertypes[i]] = (uint16_t)(i + 1);
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
  uint64_t grammar_bytes = builder.override_count
      ? sq_grammar_size(builder.tree, builder.override_count) : 0;
  uint64_t trailing_bytes = presence_bytes + grammar_bytes;
  if (trailing_bytes > UINT32_MAX) {
    sq_fail(error, SQ_ERROR_OVERFLOW);
    goto failure;
  }

  uint32_t final_capacity = options.repack ? sq_header(builder.tree)->group_count
                                           : sq_header(builder.tree)->group_capacity;
  if (!sq_prepare_final(&builder.tree, final_capacity, (uint32_t)trailing_bytes, error)) {
    goto failure;
  }

  if (options.symbol_presence) {
    bool ok = context
        ? sq_build_presence_cached(builder.tree, context->public_index,
                                   &context->presence, &context->presence_capacity, error)
        : sq_build_presence(builder.tree, error);
    if (!ok) goto failure;
  }

  if (builder.override_count) {
    SQTree *packed = builder.tree;
    uint32_t offset = sq_grammar_offset(packed);
    uint32_t words = sq_grammar_words(packed);
    uint32_t bitmap = offset + 8, ranks = bitmap + words * 8;
    uint32_t values = ranks + (uint32_t)sq_array_size(words, 4);
    memset(packed->data + offset, 0, (size_t)grammar_bytes);
    sq_set_u32(packed->data, offset, 0, builder.override_count);
    for (uint32_t i = 0; i < builder.override_count; i++) {
      sq_set_bit(packed->data, bitmap, builder.overrides[i].slot, true);
      sq_set_packed(packed->data, values, i, packed->layout.symbol_bits, builder.overrides[i].symbol);
    }
    uint32_t rank = 0;
    for (uint32_t i = 0; i < words; i++) {
      sq_set_u32(packed->data, ranks, i, rank);
      rank += (uint32_t)__builtin_popcountll(sq_get_u64(packed->data, bitmap, i));
    }
    sq_header(packed)->format_flags |= SQ_GRAMMAR_OVERRIDES;
  }

  goto cleanup;
failure:
  sq_tree_delete(builder.tree);
  builder.tree = NULL;
cleanup:
  result = builder.tree;
  if (context) {
    context->scratch.overrides = builder.overrides;
    context->scratch.override_capacity = builder.override_capacity;
    context->scratch.positions = builder.positions;
    context->scratch.position_capacity = builder.position_capacity;
    context->scratch.masks = builder.masks;
    context->scratch.mask_capacity = builder.mask_capacity;
    context->stack = stack;
    context->stack_capacity = stack_capacity;
  } else {
    free(builder.overrides);
    free(stack);
    free(builder.positions);
    free(builder.fields);
    free(builder.masks);
    free(builder.supertype_indexes);
  }
  return result;
}

SQTree *sq_tree_pack(const TSTree *tree, SQPackOptions options, SQError *error) {
  return pack_tree(NULL, tree, options, error);
}

SQTree *sq_pack_context_pack(SQPackContext *context, const TSTree *tree,
                             SQPackOptions options, SQError *error) {
  if (!context) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return NULL;
  }
  return pack_tree(context, tree, options, error);
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
