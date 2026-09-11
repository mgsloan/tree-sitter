// Public-API field-lookup probe, linked separately against each engine.
// See field-lookup-review.md for builds, expected results, and provenance.
#include <tree_sitter/api.h>
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef WITH_SQUAT
#include "../internal.h"
#endif
static const char *source = "type Example = typeof object.property;";
static void show(const char *prefix, TSNode node) {
  printf("%s%s [%u,%u) `%.*s`\n", prefix, ts_node_type(node), ts_node_start_byte(node),
         ts_node_end_byte(node), (int)(ts_node_end_byte(node) - ts_node_start_byte(node)),
         source + ts_node_start_byte(node));
}

static void walk(TSNode node, const TSLanguage *language) {
  show("node: ", node);
  for (TSFieldId field = 1; field <= ts_language_field_count(language); field++) {
    TSNode result = ts_node_child_by_field_id(node, field);
    if (!ts_node_is_null(result)) {
      printf("  lookup %s -> ", ts_language_field_name_for_id(language, field));
      show("", result);
    }
  }

  TSTreeCursor cursor = ts_tree_cursor_new(node);
  if (ts_tree_cursor_goto_first_child(&cursor)) {
    do {
      TSNode child = ts_tree_cursor_current_node(&cursor);
      const char *field = ts_tree_cursor_current_field_name(&cursor);
      printf("  child field=%s: ", field ? field : "none");
      show("", child);
    } while (ts_tree_cursor_goto_next_sibling(&cursor));
  }

  ts_tree_cursor_delete(&cursor);
  for (uint32_t i = 0; i < ts_node_named_child_count(node); i++) {
    walk(ts_node_named_child(node, i), language);
  }
}

int main(int argc, char **argv) {
  assert(argc == 2 || argc == 3);
  if (argc == 3) {
    source = argv[2];
  }

  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) {
    fprintf(stderr, "%s\n", dlerror());
    return 1;
  }

  const TSLanguage *(*get_language)(void) = dlsym(library, "tree_sitter_typescript");
  assert(get_language);
  const TSLanguage *language = get_language();
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language));
  TSTree *tree = ts_parser_parse_string(parser, NULL, source, strlen(source));
  assert(tree);
  walk(ts_tree_root_node(tree), language);
#ifdef WITH_SQUAT
  SQError error;
  SQTree *packed = sq_tree_pack(tree, sq_pack_options_default(), &error);
  assert(packed && error == SQ_OK);
  puts("SQUAT FIELD LOOKUPS:");
  for (SQNode node = sq_tree_root_node(packed); !sq_node_is_null(node);
       node = sq_node_next_preorder(node)) {
    for (TSFieldId field = 1; field <= ts_language_field_count(language); field++) {
      SQNode result = sq_node_child_by_field_id(node, field);
      if (!sq_node_is_null(result)) {
        printf("%s [%u,%u) lookup %s -> %s [%u,%u)\n", sq_node_type(node), sq_node_start_byte(node),
               sq_node_end_byte(node), ts_language_field_name_for_id(language, field),
               sq_node_type(result), sq_node_start_byte(result), sq_node_end_byte(result));
      }
    }
  }

  sq_tree_delete(packed);
#endif
  ts_tree_delete(tree);
  ts_parser_delete(parser);
  dlclose(library);
}
