#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>

static double now(void) {
  struct timespec time;
  clock_gettime(CLOCK_MONOTONIC, &time);
  return time.tv_sec + time.tv_nsec * 1e-9;
}

static int compare_double(const void *left, const void *right) {
  double a = *(const double *)left, b = *(const double *)right;
  return (a > b) - (a < b);
}

int main(int argc, char **argv) {
  if (argc < 4) {
    fprintf(stderr, "usage: layout-bench LIBRARY SYMBOL SOURCE...\n");
    return 2;
  }

  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) {
    fprintf(stderr, "%s\n", dlerror());
    return 2;
  }

  const TSLanguage *(*language_function)(void) =
      (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  if (!language_function) {
    return 2;
  }

  const TSLanguage *language = language_function();
  SQError grammar_error;
  double grammar_start = now();
  SQGrammar *grammar = sq_grammar_new(language, &grammar_error);
  double grammar_ms = (now() - grammar_start) * 1000;
  if (!grammar) return 1;
  TSParser *parser = ts_parser_new();
  if (!ts_parser_set_language(parser, language)) {
    return 2;
  }

  puts("file_index,group_size,alignment,source_bytes,nodes,groups,slots,slab_bytes,"
       "presence_bytes,dictionary_bytes,median_pack_ms,grammar_prepare_ms");
  for (int file_index = 3; file_index < argc; file_index++) {
    FILE *file = fopen(argv[file_index], "rb");
    if (!file || fseek(file, 0, SEEK_END)) {
      return 2;
    }

    long length = ftell(file);
    if (length < 0 || length > 16 * 1024 * 1024) {
      return 2;
    }

    rewind(file);
    char *source = malloc((size_t)length + 1);
    if (!source || fread(source, 1, (size_t)length, file) != (size_t)length) {
      return 2;
    }

    fclose(file);
    TSTree *parsed = ts_parser_parse_string(parser, NULL, source, (uint32_t)length);
    if (!parsed) {
      return 2;
    }

    SQTree *tree = NULL;
    double timings[7];
    for (unsigned repeat = 0; repeat < 7; repeat++) {
      sq_tree_delete(tree);
      SQError error;
      SQPackOptions options = sq_pack_options_default();
      options.repack = true;
      double start = now();
      tree = sq_tree_pack(grammar, parsed, options, &error);
      timings[repeat] = (now() - start) * 1000;
      if (!tree) {
        fprintf(stderr, "%s: %s\n", argv[file_index], sq_error_string(error));
        return 1;
      }
    }

    qsort(timings, 7, sizeof(double), compare_double);
    SQHeader header = sq_read_header(tree->data);
    uint32_t slots = sq_tree_slot_count(tree);
    uint32_t presence_bytes = sq_presence_offset(tree) ? (uint32_t)sq_presence_size(tree) : 0;
    uint32_t dictionary_bytes = header.supertype_dictionary_count *
                                  ((tree->supertype_count + 63) / 64) * 8;
    printf("%d,%u,%u,%ld,%u,%u,%u,%u,%u,%u,%.6f,%.6f\n", file_index - 3,
           SQ_GROUP_SIZE, SQ_COLUMN_ALIGNMENT, length,
           sq_node_descendant_count(sq_tree_root_node(tree)), header.group_count, slots,
           tree->size, presence_bytes, dictionary_bytes, timings[3], grammar_ms);
    sq_tree_delete(tree);
    ts_tree_delete(parsed);
    free(source);
  }

  ts_parser_delete(parser);
  sq_grammar_delete(grammar);
  dlclose(library);
  return 0;
}
