// Diagnostic for the vendored upstream engine, including its raw hidden leaves.
// Build with that engine's tree.h/subtree.h; do not link a packed engine here.
#include <tree_sitter/api.h>
#include "tree.h"
#include "subtree.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static void raw(Subtree subtree, const TSLanguage *language, uint32_t start, unsigned depth) {
  start += ts_subtree_padding(subtree).bytes;
  printf("%*s%s [%u,%u) visible=%d children=%u\n", depth * 2, "",
         ts_language_symbol_name(language, ts_subtree_symbol(subtree)), start,
         start + ts_subtree_size(subtree).bytes, ts_subtree_visible(subtree),
         ts_subtree_child_count(subtree));
  uint32_t position = start;
  for (uint32_t i = 0; i < ts_subtree_child_count(subtree); i++) {
    Subtree child = ts_subtree_children(subtree)[i];

    // Composite padding is its first child's padding, already included above.
    uint32_t padding = ts_subtree_padding(child).bytes;
    raw(child, language, position - (i == 0 ? padding : 0), depth + 1);
    position += ts_subtree_size(child).bytes + (i == 0 ? 0 : padding);
  }
}

static void show(const char *name, TSNode node) {
  printf("%s %s [%u,%u)\n", name, ts_node_is_null(node) ? "null" : ts_node_type(node),
         ts_node_start_byte(node), ts_node_end_byte(node));
}

int main(int argc, char **argv) {
  assert(argc == 2);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) {
    fprintf(stderr, "%s\n", dlerror());
    return 1;
  }

  const TSLanguage *(*get_language)(void) = dlsym(library, "tree_sitter_css");
  assert(get_language);
  const TSLanguage *language = get_language();
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language));
  const char *source = "a b {}";
  TSTree *tree = ts_parser_parse_string(parser, NULL, source, strlen(source));
  assert(tree);
  TSNode root = ts_tree_root_node(tree);
  assert(!ts_node_has_error(root));
  char *sexp = ts_node_string(root);
  puts(sexp);
  free(sexp);
  raw(tree->root, language, 0, 0);
  for (uint32_t i = 0; i <= strlen(source); i++) {
    printf("offset %u\n", i);
    show("  bytes", ts_node_descendant_for_byte_range(root, i, i));
    show("  named bytes", ts_node_named_descendant_for_byte_range(root, i, i));
    show("  points", ts_node_descendant_for_point_range(root, (TSPoint){0, i}, (TSPoint){0, i}));
    show("  named points",
         ts_node_named_descendant_for_point_range(root, (TSPoint){0, i}, (TSPoint){0, i}));
  }

  show("nonempty bytes 2..3", ts_node_descendant_for_byte_range(root, 2, 3));
  show("nonempty points (0,2)..(0,3)",
       ts_node_descendant_for_point_range(root, (TSPoint){0, 2}, (TSPoint){0, 3}));
  ts_tree_delete(tree);
  ts_parser_delete(parser);
  dlclose(library);
}
