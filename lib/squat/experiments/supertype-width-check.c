// Compile with the generated probe header selected by -I.
#include "internal.h"
#include <assert.h>
#include <stdio.h>

int main(void) {
  uint8_t data[512] = {0};
  uint32_t expected[97];
  SQTree tree = {.data = data};
  for (unsigned bits = 0; bits <= 16; bits++) {
    tree.layout.supertype_bits = bits;
    tree.layout.supertype_lanes = bits ? 64 / bits : 0;
    tree.layout.supertype_mask = (1u << bits) - 1;
    for (unsigned pattern = 0; pattern < 4; pattern++) {
      for (unsigned i = 0; i < 97; i++) {
        uint32_t value = pattern < 2 ? 0 : i * 137u + 57u;
        if (pattern & 1) value = ~value;
        expected[i] = value & tree.layout.supertype_mask;
        if (bits) sq_set_packed(data, 0, i, bits, expected[i]);
      }
      for (unsigned i = 0; i < 97; i++) {
        assert(sq_node_supertype((SQNode){&tree, i}) == expected[i]);
      }
    }
  }
  puts("ok: supertype getters at widths 0 through 16, including word boundaries");
}
