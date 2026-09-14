#include <tree_sitter/api.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv) {
  if (argc < 4) return 1;
  void *handle = dlopen(argv[1], RTLD_NOW);
  if (!handle) return 2;
  const TSLanguage *(*language)(void) = dlsym(handle, argv[2]);
  if (!language) return 3;
  TSParser *parser = ts_parser_new();
  if (!ts_parser_set_language(parser, language())) return 4;
  for (int i = 3; i < argc; i++) {
    FILE *file = fopen(argv[i], "rb"); if (!file) return 5;
    fseek(file, 0, SEEK_END); long length = ftell(file); rewind(file);
    char *text = malloc(length + 1); if (!text) return 6;
    if (fread(text, 1, length, file) != (size_t)length) return 7;
    fclose(file);
    TSTree *tree = ts_parser_parse_string(parser, NULL, text, length);
    if (!tree) return 8;
    printf("%d\n", ts_node_has_error(ts_tree_root_node(tree)));
    ts_tree_delete(tree); free(text);
  }
  ts_parser_delete(parser);
}
