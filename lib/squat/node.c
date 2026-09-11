#include "internal.h"
#if defined(__SSE2__)
#include <emmintrin.h>
#endif

SQNode sq_null(void) {
  return (SQNode){NULL, 0};
}

bool sq_node_is_null(SQNode node) {
  return !node.tree;
}

bool sq_node_eq(SQNode left, SQNode right) {
  return left.tree == right.tree && (!left.tree || left.slot == right.slot);
}

uint32_t sq_previous_slot(const SQTree *tree, uint32_t slot) {
  if (slot >= sq_tree_slot_count(tree)) return SQ_NONE;
  uint32_t group = slot / SQ_GROUP_SIZE;
  uint32_t end = (group + 1) * SQ_GROUP_SIZE - sq_group_waste(tree, group);
  return slot >= end ? end - 1 : slot;
}

uint32_t sq_next_position(const SQTree *tree, uint32_t position) {
  uint32_t slots = sq_tree_slot_count(tree);
  if (position >= slots) return slots;
  uint32_t physical = sq_previous_slot(tree, slots - 1 - position);
  return slots - 1 - physical;
}

SQNode sq_tree_node_at_slot(const SQTree *tree, uint32_t slot) {
  return tree && slot < sq_tree_slot_count(tree) && sq_previous_slot(tree, slot) == slot
             ? (SQNode){tree, slot}
             : sq_null();
}

SQNode sq_tree_root_node(const SQTree *tree) {
  return tree && sq_tree_group_count(tree)
             ? sq_tree_node_at_slot(tree, sq_previous_slot(tree, sq_tree_slot_count(tree) - 1))
             : sq_null();
}

uint32_t sq_node_first_slot(SQNode node) {
  return node.slot - sq_group_span_base(node.tree, node.slot / SQ_GROUP_SIZE) -
         sq_node_span_delta(node);
}

uint32_t sq_node_end_slot(SQNode node) {
  return sq_tree_slot_count(node.tree) - sq_node_first_slot(node);
}

static TSSymbol raw_symbol(SQNode node) {
  return sq_decode_symbol(node.tree, sq_node_symbol_id(node));
}

TSSymbol sq_node_symbol(SQNode node) {
  return node.tree ? ts_language_public_symbol(node.tree->language, raw_symbol(node)) : 0;
}

TSSymbol sq_node_grammar_symbol(SQNode node) {
  return node.tree ? sq_decode_symbol(node.tree, sq_node_grammar_id(node)) : 0;
}

const char *sq_node_type(SQNode node) {
  return node.tree ? ts_language_symbol_name(node.tree->language, raw_symbol(node)) : NULL;
}

const char *sq_node_grammar_type(SQNode node) {
  return node.tree ? ts_language_symbol_name(node.tree->language, sq_node_grammar_symbol(node))
                   : NULL;
}

uint32_t sq_node_start_byte(SQNode node) {
  return node.tree ? sq_group_start_byte_base(node.tree, node.slot / SQ_GROUP_SIZE) +
                         sq_node_start_byte_delta(node)
                   : 0;
}

uint32_t sq_node_end_byte(SQNode node) {
  return node.tree ? sq_group_end_byte_base(node.tree, node.slot / SQ_GROUP_SIZE) -
                         sq_node_end_byte_delta(node)
                   : 0;
}

#if SQ_INCLUDE_POINTS
TSPoint sq_node_start_point(SQNode node) {
  if (!node.tree) return (TSPoint){0, 0};
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  uint64_t point = sq_group_start_point_base(node.tree, group) +
                   sq_expand_point_key((uint16_t)sq_node_start_point_key(node));
  return sq_point_from_key(point);
}

TSPoint sq_node_end_point(SQNode node) {
  if (!node.tree) return (TSPoint){0, 0};
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  uint64_t point = sq_group_end_point_base(node.tree, group) -
                   sq_expand_point_key((uint16_t)sq_node_end_point_key(node));
  return sq_point_from_key(point);
}
#endif

bool sq_node_is_named(SQNode node) {
  return node.tree && ts_language_symbol_metadata(node.tree->language, raw_symbol(node)).named;
}

bool sq_node_is_extra(SQNode node) {
  return node.tree && sq_node_extra_flag(node);
}

bool sq_node_is_missing(SQNode node) {
  return node.tree && sq_node_missing_flag(node);
}

bool sq_node_is_error(SQNode node) {
  return node.tree && sq_node_symbol(node) == ts_builtin_sym_error;
}

bool sq_node_has_error(SQNode node) {
  return node.tree && sq_node_error_flag(node);
}

bool sq_node_has_changes(SQNode node) {
  (void)node;
  return false;
}

TSFieldId sq_node_field_id(SQNode node) {
  return node.tree ? (TSFieldId)sq_node_field_value(node) : 0;
}

const char *sq_node_field_name(SQNode node) {
  return node.tree ? ts_language_field_name_for_id(node.tree->language, sq_node_field_id(node))
                   : NULL;
}

bool sq_node_has_supertype(SQNode node, TSSymbol symbol) {
  if (!node.tree) {
    return false;
  }

  const SQTree *tree = node.tree;
  for (uint32_t i = 0; i < tree->supertype_count; i++) {
    if (tree->supertypes[i] != symbol) {
      continue;
    }

    uint32_t value = sq_node_supertype(node);
    if (tree->supertype_count <= 8) {
      return (value >> i) & 1;
    }

    uint32_t words = (tree->supertype_count + 63) / 64;
    uint64_t word;
    memcpy(&word, tree->data + sq_dictionary_offset(tree) + ((size_t)value * words + i / 64) * 8,
           8);
    return (word >> (i % 64)) & 1;
  }

  return false;
}

uint32_t sq_node_descendant_count(SQNode node) {
  if (!node.tree) return 0;
  uint32_t first = sq_node_first_slot(node), count = node.slot - first + 1;
  for (uint32_t group = first / SQ_GROUP_SIZE; group < node.slot / SQ_GROUP_SIZE; group++) {
    count -= sq_group_waste(node.tree, group);
  }

  return count;
}

SQNode sq_node_next_preorder(SQNode node) {
  return node.tree ? sq_tree_node_at_slot(node.tree, sq_previous_slot(node.tree, node.slot - 1))
                   : sq_null();
}

SQNode sq_node_prev_preorder(SQNode node) {
  if (!node.tree || node.slot == sq_tree_root_node(node.tree).slot) return sq_null();
  uint32_t slot = node.slot + 1, group = slot / SQ_GROUP_SIZE;
  uint32_t end = (group + 1) * SQ_GROUP_SIZE - sq_group_waste(node.tree, group);
  if (slot >= end) slot = (group + 1) * SQ_GROUP_SIZE;
  return sq_tree_node_at_slot(node.tree, slot);
}

static SQNode first_child(SQNode node) {
  if (!node.tree) return sq_null();

  // Normalization already establishes an occupied slot. Avoid repeating its
  // bounds/waste checks through the public node-at-slot constructor.
  uint32_t slot = sq_previous_slot(node.tree, node.slot - 1);
  return slot != SQ_NONE && slot >= sq_node_first_slot(node) ? (SQNode){node.tree, slot}
                                                             : sq_null();
}

SQNode sq_node_next_sibling_including_empty(SQNode node) {
  return node.tree && !sq_node_last_flag(node)
             ? sq_tree_node_at_slot(node.tree, sq_node_first_slot(node) - 1)
             : sq_null();
}

SQNode sq_node_parent(SQNode node) {
  if (!node.tree) return sq_null();
  uint32_t slots = sq_tree_slot_count(node.tree), slot = node.slot + 1;

  // Earlier preorder nodes occupy higher slots. Reject a group when even its
  // largest possible span cannot reach this node, then inspect nearby parents.
  while (slot < slots) {
    uint32_t group = slot / SQ_GROUP_SIZE;
    uint32_t end = (group + 1) * SQ_GROUP_SIZE - sq_group_waste(node.tree, group);
    uint32_t base = sq_group_span_base(node.tree, group);
    if ((uint64_t)slot <= (uint64_t)node.slot + base + UINT8_MAX) {
      for (; slot < end; slot++) {
        SQNode candidate = {node.tree, slot};
        if ((uint64_t)slot <= (uint64_t)node.slot + base + sq_node_span_delta(candidate)) {
          return candidate;
        }
      }
    }

    slot = (group + 1) * SQ_GROUP_SIZE;
  }

  return sq_null();
}

SQNode sq_node_prev_sibling(SQNode node) {
  SQNode child = first_child(sq_node_parent(node)), previous = sq_null();
  for (; child.tree && child.slot != node.slot;
       child = sq_node_next_sibling_including_empty(child)) {
    previous = child;
  }

  return previous;
}

SQNode sq_node_next_sibling(SQNode node) {
  uint32_t end_byte = sq_node_end_byte(node);
  do {
    node = sq_node_next_sibling_including_empty(node);
  } while (node.tree && sq_node_end_byte(node) <= end_byte);
  return node;
}

SQNode sq_node_next_named_sibling(SQNode node) {
  uint32_t end_byte = sq_node_end_byte(node);
  do {
    node = sq_node_next_sibling_including_empty(node);
  } while (node.tree && (sq_node_end_byte(node) <= end_byte || !sq_node_is_named(node)));
  return node;
}

SQNode sq_node_prev_named_sibling(SQNode node) {
  SQNode child = first_child(sq_node_parent(node)), previous = sq_null();
  for (; child.tree && child.slot != node.slot;
       child = sq_node_next_sibling_including_empty(child)) {
    if (sq_node_is_named(child)) {
      previous = child;
    }
  }

  return previous;
}

static SQNode child_at(SQNode node, uint32_t index, bool named) {
  for (SQNode child = first_child(node); child.tree;
       child = sq_node_next_sibling_including_empty(child)) {
    if (!named || sq_node_is_named(child)) {
      if (!index) {
        return child;
      }

      index--;
    }
  }

  return sq_null();
}

static uint32_t child_count(SQNode node, bool named) {
  uint32_t count = 0;
  for (SQNode child = first_child(node); child.tree;
       child = sq_node_next_sibling_including_empty(child)) {
    count += !named || sq_node_is_named(child);
  }

  return count;
}

SQNode sq_node_child(SQNode node, uint32_t i) {
  return child_at(node, i, false);
}

SQNode sq_node_named_child(SQNode node, uint32_t i) {
  return child_at(node, i, true);
}

uint32_t sq_node_child_count(SQNode node) {
  return child_count(node, false);
}

uint32_t sq_node_named_child_count(SQNode node) {
  return child_count(node, true);
}

SQNode sq_node_child_by_field_id(SQNode node, TSFieldId field) {
  // ERROR productions have no field map. Hidden children can still contribute
  // field names to enumeration, but mainline's field lookup stops at ERROR.
  if (sq_node_is_error(node)) {
    return sq_null();
  }

  if (field) {
    for (SQNode child = first_child(node); child.tree;
         child = sq_node_next_sibling_including_empty(child)) {
      if (sq_node_field_id(child) == field) {
        return child;
      }
    }
  }

  return sq_null();
}

SQNode sq_node_child_by_field_name(SQNode node, const char *name, uint32_t length) {
  return node.tree && name ? sq_node_child_by_field_id(node, ts_language_field_id_for_name(
                                                                 node.tree->language, name, length))
                           : sq_null();
}

const char *sq_node_field_name_for_child(SQNode node, uint32_t i) {
  return sq_node_field_name(sq_node_child(node, i));
}

const char *sq_node_field_name_for_named_child(SQNode node, uint32_t i) {
  return sq_node_field_name(sq_node_named_child(node, i));
}

SQNode sq_node_child_with_descendant(SQNode node, SQNode descendant) {
  if (!node.tree || node.tree != descendant.tree || descendant.slot >= node.slot ||
      descendant.slot < sq_node_first_slot(node)) {
    return sq_null();
  }

  for (SQNode child = first_child(node); child.tree;
       child = sq_node_next_sibling_including_empty(child)) {
    if (sq_node_first_slot(child) <= descendant.slot) {
      return child;
    }
  }

  return sq_null();
}

static SQNode first_for_byte(SQNode node, uint32_t byte, bool named) {
  for (SQNode child = first_child(node); child.tree;
       child = sq_node_next_sibling_including_empty(child)) {
    if (sq_node_end_byte(child) > byte && (!named || sq_node_is_named(child))) {
      return child;
    }
  }

  return sq_null();
}

SQNode sq_node_first_child_for_byte(SQNode node, uint32_t right) {
  return first_for_byte(node, right, false);
}

SQNode sq_node_first_named_child_for_byte(SQNode node, uint32_t right) {
  return first_for_byte(node, right, true);
}

// The equal-start walks stop at their subtree root, so they need only skip
// unused group lanes; the public preorder API's tree bounds checks are redundant.
static uint32_t seek_previous_slot(SQNode node) {
  uint32_t group = node.slot / SQ_GROUP_SIZE;
  uint32_t limit = (group + 1) * SQ_GROUP_SIZE - sq_group_waste(node.tree, group);
  return node.slot + 1 < limit ? node.slot + 1 : (group + 1) * SQ_GROUP_SIZE;
}

#if defined(__SSE2__)
// Match unsigned start deltas at or below the threshold. A group occupies
// complete 16-byte chunks, including its unused physical lanes.
// Callers clip the resulting mask to live lanes within the requested subtree.
static uint64_t seek_start_mask(const uint8_t *deltas, uint32_t threshold) {
  if (threshold >= UINT8_MAX) return UINT64_MAX >> (64 - SQ_GROUP_SIZE);
  __m128i value = _mm_set1_epi8((char)threshold);
  uint64_t mask = 0;
  for (unsigned offset = 0; offset < SQ_GROUP_SIZE; offset += 16) {
    __m128i lanes = _mm_loadu_si128((const __m128i *)(deltas + offset));
    __m128i match = _mm_cmpeq_epi8(_mm_min_epu8(lanes, value), lanes);
    mask |= (uint64_t)(unsigned)_mm_movemask_epi8(match) << offset;
  }

  return mask;
}

static uint32_t seek_mask_slot(uint64_t mask, uint32_t slot, uint32_t limit) {
  mask >>= slot % SQ_GROUP_SIZE;
  uint32_t found = mask ? slot + (uint32_t)__builtin_ctzll(mask) : limit;
  return found < limit ? found : limit;
}

#endif

#if SQ_INCLUDE_POINTS
enum { SEEK_POINT_SCAN_GROUPS = 512 };

static int point_cmp(TSPoint left, TSPoint right) {
  return left.row != right.row ? (left.row > right.row ? 1 : -1)
                               : (left.column > right.column) - (left.column < right.column);
}

// Return the largest encoded start delta whose reconstructed point is at or
// before target. All smaller keys qualify because row occupies the high byte.
static bool seek_start_point_threshold(uint64_t base_key, TSPoint target, uint32_t *threshold) {
  TSPoint base = sq_point_from_key(base_key);
  if (target.row < base.row) {
    return false;
  }

  uint32_t row = target.row - base.row;
  if (row > UINT8_MAX) {
    *threshold = UINT16_MAX;
    return true;
  }

  if (target.column < base.column) {
    if (!row) {
      return false;
    }

    *threshold = (row << 8) - 1;
    return true;
  }

  uint32_t column = target.column - base.column;
  if (column > UINT8_MAX) {
    column = UINT8_MAX;
  }

  *threshold = row << 8 | column;
  return true;
}

// End deltas run backward from the group base, so the same key ordering is
// reversed: every key at or below the returned threshold satisfies the bound.
static bool seek_end_point_threshold(uint64_t base_key, TSPoint target, bool inclusive,
                                     uint32_t *threshold) {
  TSPoint base = sq_point_from_key(base_key);
  if (base.row < target.row) {
    return false;
  }

  uint32_t row = base.row - target.row;
  if (row > UINT8_MAX) {
    *threshold = UINT16_MAX;
    return true;
  }

  bool equal_row_possible =
      inclusive ? base.column >= target.column : base.column > target.column;
  if (!equal_row_possible) {
    if (!row) {
      return false;
    }

    *threshold = (row << 8) - 1;
    return true;
  }

  uint32_t column = base.column - target.column - !inclusive;
  if (column > UINT8_MAX) {
    column = UINT8_MAX;
  }

  *threshold = row << 8 | column;
  return true;
}

// Keep the shared-boundary fallback out of the indexed search's hot code.
#if defined(__GNUC__) || defined(__clang__)
__attribute__((noinline))
#endif
static SQNode seek_point_descent(SQNode node, TSPoint start, TSPoint end, bool named) {
  SQNode result = node;
  for (;;) {
    SQNode found = sq_null();
    for (SQNode child = first_child(node); child.tree;
         child = sq_node_next_sibling_including_empty(child)) {
      TSPoint child_start = sq_node_start_point(child);
      TSPoint child_end = sq_node_end_point(child);
      if (point_cmp(child_end, end) < 0) {
        continue;
      }

      int past = point_cmp(child_end, start);
      if (point_cmp(child_start, child_end) == 0 ? past < 0 : past <= 0) {
        continue;
      }

      if (point_cmp(start, child_start) < 0) {
        break;
      }

      found = child;
      break;
    }

    if (!found.tree) {
      return result;
    }

    node = found;
    if (!named || sq_node_is_named(node)) {
      result = node;
    }
  }
}

static SQNode seek_point(SQNode node, TSPoint range_start, TSPoint range_end, bool named) {
  uint64_t range_start_key = sq_point_key(range_start);
  uint64_t range_end_key = sq_point_key(range_end);
  if (!node.tree || range_start_key > range_end_key) {
    return sq_null();
  }

  const SQTree *tree = node.tree;
  uint32_t node_group = node.slot / SQ_GROUP_SIZE;
  uint64_t node_start = sq_group_start_point_base(tree, node_group) +
                        sq_expand_point_key((uint16_t)sq_node_start_point_key(node));
  uint64_t node_end = sq_group_end_point_base(tree, node_group) -
                      sq_expand_point_key((uint16_t)sq_node_end_point_key(node));
  if (range_start_key < node_start || range_end_key > node_end) {
    return node;
  }

  uint32_t first = sq_node_first_slot(node);

  // Search group start rows first. Only reconstruct the earliest preorder
  // point when the row ties; the column base need not be an actual minimum.
  uint32_t low = first / SQ_GROUP_SIZE, high = node.slot / SQ_GROUP_SIZE;
  while (low < high) {
    uint32_t middle = low + (high - low) / 2;
    uint64_t base = sq_group_start_point_base(tree, middle);
    uint32_t row = (uint32_t)(base >> 32);
    bool after = row > range_start.row;
    if (row == range_start.row) {
      uint32_t earliest = (middle + 1) * SQ_GROUP_SIZE - sq_group_waste(tree, middle) - 1;
      uint64_t start = base +
                       sq_expand_point_key(
                           (uint16_t)sq_node_start_point_key((SQNode){tree, earliest}));
      after = start > range_start_key;
    }

    low = after ? middle + 1 : low;
    high = after ? high : middle;
  }

  // Clip to live slots in this subtree, then find its last qualifying start.
  uint32_t slot = low * SQ_GROUP_SIZE;
  if (slot < first) slot = first;
  uint32_t limit = (low + 1) * SQ_GROUP_SIZE - sq_group_waste(tree, low);
  if (limit > node.slot + 1) limit = node.slot + 1;
  uint64_t base = sq_group_start_point_base(tree, low);
  uint32_t threshold;
  bool any = seek_start_point_threshold(base, range_start, &threshold);

  // A scalar key comparison can stop at the first qualifying lane. For these
  // short groups that is cheaper than constructing a complete SIMD lane mask.
  while (slot < limit && (!any || sq_node_start_point_key((SQNode){tree, slot}) > threshold)) {
    slot++;
  }

  // The subtree boundary may exclude this group's qualifying nodes.
  if (slot == limit) {
    slot = (low + 1) * SQ_GROUP_SIZE;
    if (slot > node.slot) return node;
  }

  SQNode candidate = {tree, slot};

  // Preserve sibling-order selection when empty nodes share the query point.
  if (point_cmp(range_start, range_end) == 0) {
    for (SQNode previous = candidate; previous.slot <= node.slot &&
         point_cmp(sq_node_start_point(previous), range_start) == 0;
         previous.slot = seek_previous_slot(previous)) {
      if (point_cmp(sq_node_end_point(previous), range_start) == 0) {
        return seek_point_descent(node, range_start, range_end, named);
      }
    }
  }

  // Parent traversal is cheaper when a direct end scan would cross many groups.
  // The slot distance estimates that work after the start search has selected
  // its candidate, and is also meaningful for seeks rooted inside a large tree.
  uint32_t distance = node.slot - candidate.slot;
  if (distance > SEEK_POINT_SCAN_GROUPS * SQ_GROUP_SIZE) {
    while (candidate.slot < node.slot) {
      uint32_t group = candidate.slot / SQ_GROUP_SIZE;
      uint64_t end = sq_group_end_point_base(tree, group) -
                     sq_expand_point_key((uint16_t)sq_node_end_point_key(candidate));
      if (end >= range_end_key && end > range_start_key &&
          (!named || sq_node_is_named(candidate))) return candidate;
      candidate = sq_node_parent(candidate);
      if (!candidate.tree) return node;
    }

    return node;
  }

  // Most start candidates already contain the range. Check that node directly
  // before paying to derive thresholds and scan the rest of its group.
  if (candidate.slot < node.slot) {
    uint32_t group = candidate.slot / SQ_GROUP_SIZE;
    uint64_t end = sq_group_end_point_base(tree, group) -
                   sq_expand_point_key((uint16_t)sq_node_end_point_key(candidate));
    if (end >= range_end_key && end > range_start_key &&
        (!named || sq_node_is_named(candidate))) return candidate;
    candidate.slot++;
  }

  // For nearby candidates, use the stricter end bound: nonempty ranges need
  // end >= range_end, while empty ranges need end > range_start. Earlier
  // preorder siblings end before the query, so the first match is an ancestor.
  bool inclusive = range_start_key != range_end_key;
  TSPoint end_bound = inclusive ? range_end : range_start;
  while (candidate.slot < node.slot) {
    uint32_t group = candidate.slot / SQ_GROUP_SIZE;
    uint32_t limit = (group + 1) * SQ_GROUP_SIZE - sq_group_waste(tree, group);
    if (limit > node.slot) limit = node.slot;
    uint64_t end_base = sq_group_end_point_base(tree, group);
    uint32_t threshold;
    if (seek_end_point_threshold(end_base, end_bound, inclusive, &threshold)) {
      for (; candidate.slot < limit; candidate.slot++) {
        if (sq_node_end_point_key(candidate) <= threshold &&
            (!named || sq_node_is_named(candidate))) return candidate;
      }
    }

    candidate.slot = (group + 1) * SQ_GROUP_SIZE;
  }

  return node;
}
#endif

// Descend in sibling order to preserve the first match at shared empty boundaries.
// seek_byte has already checked the node and range before taking this fallback.
// Keep the shared-boundary fallback out of the indexed search's hot code.
#if defined(__GNUC__) || defined(__clang__)
__attribute__((noinline))
#endif
static SQNode seek_byte_descent(SQNode node, uint32_t start, uint32_t end, bool named) {
  SQNode result = node;
  for (;;) {
    SQNode found = sq_null();
    for (SQNode child = first_child(node); child.tree;
         child = sq_node_next_sibling_including_empty(child)) {
      uint32_t child_start = sq_node_start_byte(child);
      uint32_t child_end = sq_node_end_byte(child);
      if (child_end < end) {
        continue;
      }

      if (child_start == child_end ? child_end < start : child_end <= start) {
        continue;
      }

      if (start < child_start) {
        break;
      }

      found = child;
      break;
    }

    if (!found.tree) {
      return result;
    }

    node = found;
    if (!named || sq_node_is_named(node)) {
      result = node;
    }
  }
}

static SQNode seek_byte(SQNode node, uint32_t range_start, uint32_t range_end, bool named) {
  if (!node.tree || range_start > range_end) return sq_null();
  if (range_start < sq_node_start_byte(node) || range_end > sq_node_end_byte(node)) return node;
  const SQTree *tree = node.tree;
  uint32_t first = sq_node_first_slot(node);

  // Use binary search to locate the last preorder group that can contain a
  // start at or before range_start.
  uint32_t low = first / SQ_GROUP_SIZE, high = node.slot / SQ_GROUP_SIZE;
  while (low < high) {
    uint32_t middle = low + (high - low) / 2;
    bool after = sq_group_start_byte_base(tree, middle) > range_start;
    low = after ? middle + 1 : low;
    high = after ? high : middle;
  }

  // Restrict the group to live slots inside this subtree, then scan its start
  // deltas for the last preorder node starting at or before range_start.
  uint32_t base = sq_group_start_byte_base(tree, low);
  uint32_t slot = low * SQ_GROUP_SIZE;
  if (slot < first) slot = first;
  uint32_t limit = (low + 1) * SQ_GROUP_SIZE - sq_group_waste(tree, low);
  if (limit > node.slot + 1) limit = node.slot + 1;
#if defined(__SSE2__)
  const uint8_t *deltas = tree->data + tree->layout.start_byte_delta + low * SQ_GROUP_SIZE;
  slot = seek_mask_slot(seek_start_mask(deltas, range_start - base), slot, limit);
#else
  while (slot < limit && base + sq_node_start_byte_delta((SQNode){tree, slot}) > range_start) slot++;
#endif

  // A subtree boundary can cut off the qualifying part of its first group
  if (slot == limit) {
    slot = (low + 1) * SQ_GROUP_SIZE;
    if (slot > node.slot) return node;
  }

  SQNode candidate = {tree, slot};

  // At a shared boundary the original descent prefers the first empty sibling,
  // not the last node with that start. Keep that rare ambiguity on its existing
  // path; ordinary equal-start ancestor chains can use the indexed result.
  if (range_start == range_end) {
    for (SQNode previous = candidate; previous.slot <= node.slot &&
         sq_node_start_byte(previous) == range_start; previous.slot = seek_previous_slot(previous)) {
      if (sq_node_end_byte(previous) == range_start) {
        return seek_byte_descent(node, range_start, range_end, named);
      }
    }
  }

  // Most selected nodes already cover the range, so avoid group-scan setup.
  if (candidate.slot < node.slot) {
    uint32_t end = sq_node_end_byte(candidate);
    if (end >= range_end && end > range_start && (!named || sq_node_is_named(candidate))) return candidate;
    candidate.slot++;
  }

  // Earlier preorder siblings end before the query; the first qualifying end
  // belongs to an enclosing ancestor. Reuse each group's end base and skip
  // groups whose maximum end cannot cover the range. Unlike point ends, this
  // needs only one base and delta comparison, so long scans remain competitive
  // with parent traversal and do not need a distance-based fallback.
  while (candidate.slot < node.slot) {
    uint32_t group = candidate.slot / SQ_GROUP_SIZE;
    uint32_t limit = (group + 1) * SQ_GROUP_SIZE - sq_group_waste(tree, group);
    if (limit > node.slot) limit = node.slot;
    uint32_t base = sq_group_end_byte_base(tree, group);
    if (base >= range_end && base > range_start) {
      uint32_t threshold = base - range_end;
      if (threshold >= base - range_start) threshold = base - range_start - 1;
      for (; candidate.slot < limit; candidate.slot++) {
        if (sq_node_end_byte_delta(candidate) <= threshold &&
            (!named || sq_node_is_named(candidate))) return candidate;
      }
    }

    candidate.slot = (group + 1) * SQ_GROUP_SIZE;
  }

  return node;
}

SQNode sq_node_descendant_for_byte_range(SQNode node, uint32_t range_start, uint32_t range_end) {
  return seek_byte(node, range_start, range_end, false);
}

SQNode sq_node_named_descendant_for_byte_range(SQNode node, uint32_t range_start,
                                               uint32_t range_end) {
  return seek_byte(node, range_start, range_end, true);
}

#if SQ_INCLUDE_POINTS
SQNode sq_node_descendant_for_point_range(SQNode node, TSPoint range_start, TSPoint range_end) {
  return seek_point(node, range_start, range_end, false);
}

SQNode sq_node_named_descendant_for_point_range(SQNode node, TSPoint range_start,
                                                TSPoint range_end) {
  return seek_point(node, range_start, range_end, true);
}
#endif
