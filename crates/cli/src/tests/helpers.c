#include "alloc.h"
#include "tree.h"

char *ts_test_tree_string(const TSTree *tree) {
  return ts_subtree_string(tree->root, 0, false, tree->language, true);
}

void ts_test_free_string(char *string) {
  ts_free(string);
}
