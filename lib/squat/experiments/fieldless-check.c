// Compile with the generated fieldless probe header selected by -I.
#include "internal.h"
#include <assert.h>
#include <stdio.h>

int main(void) {
  const char *names[] = {"end", "node"};
  const TSSymbolMetadata metadata[] = {{0}, {.visible = true, .named = true}};
  const TSLanguage language = {.abi_version = TREE_SITTER_LANGUAGE_VERSION,
                               .symbol_count = 2,
                               .symbol_names = names,
                               .symbol_metadata = metadata};
  SQError error;
  SQTree *tree = sq_allocate(&language, 1, &error);
  assert(tree && !tree->layout.field_bits && !tree->layout.field_lanes);
  assert(sq_column_size(SQ_GROUP_SIZE, 0) == 0);
  assert(tree->layout.field == tree->layout.supertype);
  sq_header(tree)->group_count = 1;
  // A missing field column must not read bytes belonging to the next column.
  tree->data[tree->layout.field] = 0xff;
  for (unsigned waste = 0; waste < SQ_GROUP_SIZE; waste++) {
    sq_set_packed(tree->data, tree->layout.waste, 0, SQ_WASTE_BITS, waste);
    uint64_t used = UINT64_MAX >> (64 - (SQ_GROUP_SIZE - waste));
    assert(sq_tree_group_field_equal(tree, 0, 0) == used);
    assert(sq_tree_group_field_equal(tree, 0, 1) == 0);
    assert(sq_tree_group_field_equal(tree, 0, UINT32_MAX) == 0);
    assert(sq_tree_group_field_equal(tree, 1, 0) == 0);
    for (unsigned slot = 0; slot < SQ_GROUP_SIZE - waste; slot++) {
      assert(sq_node_field_value((SQNode){tree, slot}) == 0);
      assert(sq_node_field_id((SQNode){tree, slot}) == 0);
    }
  }
  sq_tree_delete(tree);
  puts("ok: omitted field storage, zero reads, and group masks for every waste count");
}
