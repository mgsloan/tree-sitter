// Synthetic slabs test the public group scanner against individual scalar reads.
// Includes partial groups, word boundaries, dense hits, and dirty unused bits.
#include "../internal.h"
#include <assert.h>
#include <stdio.h>

int main(void) {
  const uint32_t groups = 8, slots = groups * SQ_GROUP_SIZE;
  uint64_t random = 42;
  for (uint8_t bits = 1; bits <= 32; bits++) {
    uint32_t offset = sizeof(SQHeader) + sq_column_size(groups, SQ_WASTE_BITS);
    size_t size = offset + sq_column_size(slots, bits);
    uint8_t *data = malloc(size);
    assert(data);
    SQTree tree = {.data = data};
    tree.layout.waste = sizeof(SQHeader);
    tree.layout.symbol = offset;
    tree.layout.symbol_bits = bits;
    uint32_t mask = (uint32_t)((UINT64_C(1) << bits) - 1);
    for (unsigned trial = 0; trial < 16; trial++) {
      memset(data, 0xff, size);
      sq_header(&tree)->group_count = groups;
      for (uint32_t slot = 0; slot < slots; slot++) {
        random = random * UINT64_C(6364136223846793005) + 1;
        uint32_t value = trial == 0 ? 0 : trial == 1 ? mask : (uint32_t)(random >> 32) & mask;
        sq_set_packed(data, offset, slot, bits, value);
      }
      for (uint32_t waste = 0; waste < SQ_GROUP_SIZE; waste++) {
        for (uint32_t group = 0; group < groups; group++) {
          sq_set_packed(data, tree.layout.waste, group, SQ_WASTE_BITS, waste);
          for (uint32_t lane = 0; lane < SQ_GROUP_SIZE; lane++) {
            uint32_t target = sq_get_packed(data, offset, group * SQ_GROUP_SIZE + lane, bits);
            uint64_t expected = 0;
            for (uint32_t other = 0; other < SQ_GROUP_SIZE - waste; other++) {
              if (sq_get_packed(data, offset, group * SQ_GROUP_SIZE + other, bits) == target)
                expected |= UINT64_C(1) << other;
            }
            assert(sq_tree_group_symbol_equal(&tree, group, target) == expected);
          }
          if (bits < 32) assert(!sq_tree_group_symbol_equal(&tree, group, mask + 1));
        }
      }
      assert(!sq_tree_group_symbol_equal(&tree, groups, 0));
    }
    free(data);
  }
  puts("ok: group equality, widths 1..32, every waste count, dense/sparse matches");
}
