#include "internal.h"

/* Each lane contributes one low bit and one high bit. Only complete lanes
 * participate: for nine-bit fields, bit 63 must remain outside the mask. */
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

  /* A zero lane leaves its high bit clear after adding the low-bit mask.
   * A nonzero low part carries into that high bit. The original high bit
   * handles values whose only difference is there. Clearing high bits before
   * addition prevents carries between lanes, unlike the common has-zero trick
   * that only answers whether ANY lane is zero. Here every result bit is exact. */
  return ~(((difference & low_bits) + low_bits) | difference | low_bits) & high_bits;
}

uint64_t sq_tree_group_equal(const SQTree *tree, uint32_t group, SQColumn column, uint32_t value) {
  if (!tree || group >= sq_tree_group_count(tree) || (unsigned)column >= SQ_COLUMN_COUNT) {
    return 0;
  }
  unsigned node_column = N_SPAN + (unsigned)column;
  uint8_t bits = sq_node_width(&tree->layout, node_column);
  if ((uint64_t)value >= (UINT64_C(1) << bits)) {
    return 0;
  }

  uint32_t lanes = 64 / bits;
  uint32_t first_slot = group * SQ_GROUP_SIZE;
  uint32_t last_slot = first_slot + SQ_GROUP_SIZE;
  uint64_t matches = 0;
  for (uint32_t word_index = first_slot / lanes; word_index <= (last_slot - 1) / lanes;
       word_index++) {
    uint64_t word;
    memcpy(&word, tree->data + tree->layout.nodes[node_column] + (size_t)word_index * 8, 8);
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
  uint32_t waste = sq_group_get(tree, G_WASTE, group);
  uint32_t used = SQ_GROUP_SIZE - waste;
  return matches & (used == 64 ? UINT64_MAX : (UINT64_C(1) << used) - 1);
}
