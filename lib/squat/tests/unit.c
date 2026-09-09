#include "../internal.h"
#include <assert.h>
#include <stdio.h>

int main(void) {
  for (uint32_t symbols = 2; symbols <= 32768; symbols *= 2) {
    TSSymbolMetadata *metadata = calloc(symbols, sizeof(TSSymbolMetadata));
    assert(metadata);
    TSLanguage language = {.abi_version = TREE_SITTER_LANGUAGE_VERSION,
                           .symbol_count = symbols,
                           .symbol_metadata = metadata};
    SQError error;
    SQTree *tree = sq_allocate(&language, 3, &error);
    assert(tree && error == SQ_OK);
    sq_header(tree)->group_count = 2;
    for (unsigned region = 0; region < 2; region++) {
      uint32_t scale = region ? SQ_GROUP_SIZE : 1;
      unsigned columns = region ? N_COLUMNS : G_COLUMNS;
      for (unsigned c = 0; c < columns; c++) {
        uint8_t bits = region ? sq_node_width(&tree->layout, c) : sq_group_width(c);
        uint32_t offset = region ? tree->layout.nodes[c] : tree->layout.groups[c];
        for (uint32_t i = 0; i < 2 * scale; i++) {
          sq_set(tree->data, offset, scale + i, bits,
                 (uint32_t)((i * UINT64_C(31337) + c) & ((UINT64_C(1) << bits) - 1)));
        }
      }
    }
    const uint32_t capacities[] = {7, 19, 2, 31, 2};
    for (unsigned k = 0; k < sizeof(capacities) / sizeof(capacities[0]); k++) {
      assert(sq_resize(tree, capacities[k], &error));
      for (unsigned region = 0; region < 2; region++) {
        uint32_t scale = region ? SQ_GROUP_SIZE : 1;
        unsigned columns = region ? N_COLUMNS : G_COLUMNS;
        for (unsigned c = 0; c < columns; c++) {
          uint8_t bits = region ? sq_node_width(&tree->layout, c) : sq_group_width(c);
          uint32_t offset = region ? tree->layout.nodes[c] : tree->layout.groups[c];
          for (uint32_t i = 0; i < 2 * scale; i++) {
            assert(sq_get(tree->data, offset, (capacities[k] - 2) * scale + i, bits) ==
                   ((i * UINT64_C(31337) + c) & ((UINT64_C(1) << bits) - 1)));
          }
          for (uint32_t i = 0; i < (capacities[k] - 2) * scale; i++) {
            assert(sq_get(tree->data, offset, i, bits) == 0);
          }
        }
      }
    }
    assert(!sq_resize(tree, UINT32_MAX, &error) && error == SQ_ERROR_OVERFLOW);
    sq_tree_delete(tree);
    free(metadata);
  }
  puts("ok: lane relocation for symbol widths 3 through 16, growth, compaction, overflow");
  return 0;
}
