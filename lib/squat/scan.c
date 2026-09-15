#include "internal.h"

// Each lane contributes one low bit and one high bit. Only complete lanes
// participate: for nine-bit fields, bit 63 must remain outside the mask.
uint64_t sq_lane_starts(uint8_t bits) {
  unsigned used_bits = (64 / bits) * bits;
  uint64_t used_mask = used_bits == 64 ? UINT64_MAX : (UINT64_C(1) << used_bits) - 1;
  return used_mask / ((UINT64_C(1) << bits) - 1);
}

uint64_t sq_equal_lanes(uint64_t word, uint32_t value, uint8_t bits) {
  uint64_t starts = sq_lane_starts(bits);
  uint64_t high_bits = starts << (bits - 1);
  uint64_t low_bits = high_bits - starts;
  uint64_t difference = word ^ (starts * value);

  // A zero lane leaves its high bit clear after adding the low-bit mask.
  // A nonzero low part carries into that high bit. The original high bit
  // handles values whose only difference is there. Clearing high bits before
  // addition prevents carries between lanes, unlike the common has-zero trick
  // that only answers whether ANY lane is zero. Here every result bit is exact.
  return ~(((difference & low_bits) + low_bits) | difference | low_bits) & high_bits;
}

static uint64_t group_equal(const SQTree *tree, uint32_t group, uint32_t offset, uint8_t bits,
                            uint32_t value) {
  if (group >= sq_tree_group_count(tree)) {
    return 0;
  }

  if ((uint64_t)value >= (UINT64_C(1) << bits)) {
    return 0;
  }

  if (!bits) {
    uint32_t used = SQ_GROUP_SIZE - sq_group_waste(tree, group);
    return UINT64_MAX >> (64 - used);
  }
  uint32_t lanes = 64 / bits;
  uint32_t first_slot = group * SQ_GROUP_SIZE;
  uint32_t last_slot = first_slot + SQ_GROUP_SIZE;
  uint64_t matches = 0;
  for (uint32_t word_index = first_slot / lanes; word_index <= (last_slot - 1) / lanes;
       word_index++) {
    uint64_t word = sq_get_u64(tree->data, offset, word_index);
    uint64_t equal = sq_equal_lanes(word, value, bits);
    while (equal) {
      unsigned bit = (unsigned)__builtin_ctzll(equal);
      uint32_t slot = word_index * lanes + bit / bits;
      if (slot >= first_slot && slot < last_slot) {
        matches |= UINT64_C(1) << (slot - first_slot);
      }

      equal &= equal - 1;
    }
  }

  uint32_t waste = sq_group_waste(tree, group);
  uint32_t used = SQ_GROUP_SIZE - waste;
  return matches & (used == 64 ? UINT64_MAX : (UINT64_C(1) << used) - 1);
}

uint64_t sq_tree_group_span_delta_equal(const SQTree *tree, uint32_t group, uint32_t value) {
  return tree ? group_equal(tree, group, tree->layout.span_delta, 8, value) : 0;
}

uint64_t sq_tree_group_start_byte_delta_equal(const SQTree *tree, uint32_t group, uint32_t value) {
  return tree ? group_equal(tree, group, tree->layout.start_byte_delta, 8, value) : 0;
}

uint64_t sq_tree_group_end_byte_delta_equal(const SQTree *tree, uint32_t group, uint32_t value) {
  return tree ? group_equal(tree, group, tree->layout.end_byte_delta, 16, value) : 0;
}

uint64_t sq_tree_group_start_point_equal(const SQTree *tree, uint32_t group, uint32_t value) {
  return tree && sq_tree_has_points(tree)
             ? group_equal(tree, group, tree->layout.start_point, 16, value)
             : 0;
}

uint64_t sq_tree_group_end_point_equal(const SQTree *tree, uint32_t group, uint32_t value) {
  return tree && sq_tree_has_points(tree)
             ? group_equal(tree, group, tree->layout.end_point, 16, value)
             : 0;
}

uint64_t sq_tree_group_supertype_equal(const SQTree *tree, uint32_t group, uint32_t value) {
  return tree ? group_equal(tree, group, tree->layout.supertype, tree->layout.supertype_bits, value) : 0;
}

uint64_t sq_tree_group_symbol_equal(const SQTree *tree, uint32_t group, uint32_t value) {
  return tree ? group_equal(tree, group, tree->layout.symbol, tree->layout.symbol_bits, value) : 0;
}

uint64_t sq_tree_group_grammar_symbol_equal(const SQTree *tree, uint32_t group, uint32_t value) {
  uint64_t matches = sq_tree_group_symbol_equal(tree, group, value);
  if (!tree || group >= sq_tree_group_count(tree) ||
      !(sq_header_get(tree, format_flags) & SQ_GRAMMAR_OVERRIDES)) return matches;
  uint32_t first = group * SQ_GROUP_SIZE;
  uint64_t overrides = sq_get_u64(tree->data, sq_grammar_offset(tree) + 8, first / 64)
                       >> (first % 64);
  overrides &= UINT64_MAX >> (64 - SQ_GROUP_SIZE);
  while (overrides) {
    unsigned lane = (unsigned)__builtin_ctzll(overrides);
    uint64_t bit = UINT64_C(1) << lane;
    matches &= ~bit;
    if (sq_node_grammar_id((SQNode){tree, first + lane}) == value) matches |= bit;
    overrides &= overrides - 1;
  }
  return matches;
}

uint64_t sq_tree_group_field_equal(const SQTree *tree, uint32_t group, uint32_t value) {
  return tree ? group_equal(tree, group, tree->layout.field, tree->layout.field_bits, value) : 0;
}
