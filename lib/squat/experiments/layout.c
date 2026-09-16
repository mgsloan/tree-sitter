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

  uint64_t *override_counts = calloc(ts_language_symbol_count(language) + 2, sizeof(uint64_t));
  if (!override_counts) return 1;

  puts("file_index,group_size,alignment,source_bytes,nodes,groups,slots,slab_bytes,presence_bytes,"
       "dictionary_bytes,grammar_differences,separate_grammar_bytes,grammar_bytes,var_"
       "super_bytes,super_bytes,"
       "interleaved_symbol_field_bytes,separate_symbol_field_bytes,median_pack_ms,grammar_difference_kinds,symbol_variant_bits,symbol_dictionary_entries,symbol_pairs,max_symbol_variants,grammar_prepare_ms,symbol_encoding,symbol_table_bytes");
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
    uint32_t grammar_overrides = 0, override_kinds = 0;
    bool *seen = calloc(sq_symbols(tree), sizeof(bool));
    if (!seen) return 1;
    for (SQNode node = sq_tree_root_node(tree); node.tree; node = sq_node_next_preorder(node)) {
      uint32_t grammar_id = sq_node_grammar_id(node);
      if (sq_node_symbol_id(node) != grammar_id) {
        grammar_overrides++;
        override_counts[grammar_id]++;
        override_kinds += !seen[grammar_id];
        seen[grammar_id] = true;
      }
    }

    free(seen);

    SQHeader header = sq_read_header(tree->data);
    uint32_t slots = sq_tree_slot_count(tree);
    uint32_t presence_bytes = sq_presence_offset(tree) ? (uint32_t)sq_presence_size(tree) : 0;
    uint32_t dictionary_bytes = header.supertype_dictionary_count *
                                  ((tree->supertype_count + 63) / 64) * 8;
    uint64_t grammar_bytes = sq_column_size(slots, tree->layout.symbol_bits);

    // Fallback trees with identical display and grammar IDs omit the column.
    uint64_t separate_grammar_bytes = header.format_flags & SQ_SEPARATE_GRAMMAR ? grammar_bytes : 0;
    uint8_t super_bits = 0;
    if (tree->supertype_count > 8) {
      if (header.supertype_dictionary_count > 1) {
        super_bits = sq_width(header.supertype_dictionary_count - 1);
      }
    } else if (tree->supertype_count) {
      super_bits = tree->supertype_count < 2 ? 2 : (uint8_t)tree->supertype_count;
    }

    uint64_t super_bytes = super_bits ? sq_column_size(slots, super_bits) : 0;
    uint64_t interleaved =
        sq_column_size(slots, tree->layout.symbol_bits + tree->layout.field_bits);
    uint64_t separate = sq_column_size(slots, tree->layout.symbol_bits) +
                        sq_column_size(slots, tree->layout.field_bits);
    uint32_t pairs = 0, maximum = 0;
    if (grammar->symbols.counts) {
      for (uint32_t symbol = 0; symbol < sq_symbols(tree); symbol++) {
        uint32_t count = grammar->symbols.counts[symbol];
        pairs += count;
        if (count > maximum) maximum = count;
      }
    }
    printf("%d,%u,%u,%ld,%u,%u,%u,%u,%u,%u,%u,%llu,%llu,%llu,%llu,%llu,%llu,%.6f,%u,%u,%u,%u,%u,%.6f,%u,%llu\n", file_index - 3,
           SQ_GROUP_SIZE, SQ_COLUMN_ALIGNMENT, length,
           sq_node_descendant_count(sq_tree_root_node(tree)), header.group_count, slots,
           tree->size, presence_bytes, dictionary_bytes, grammar_overrides,
           (unsigned long long)separate_grammar_bytes,
           (unsigned long long)grammar_bytes, (unsigned long long)super_bytes,
           (unsigned long long)sq_column_size(slots, 8), (unsigned long long)interleaved,
           (unsigned long long)separate, timings[3], override_kinds, grammar->symbols.shift,
           grammar->symbols.length, pairs, maximum, grammar_ms,
           grammar->symbols.separate ? 3 : grammar->symbols.encoding,
           (unsigned long long)(2 * ((uint64_t)grammar->symbols.length + sq_symbols(tree) *
               (1u + !!grammar->symbols.counts + !!grammar->symbols.defaults +
                !!grammar->symbols.grammar_codes))));
    sq_tree_delete(tree);
    ts_tree_delete(parsed);
    free(source);
  }

  fprintf(stderr, "grammar_id,override_nodes\n");
  for (uint32_t symbol = 0; symbol < ts_language_symbol_count(language) + 2; symbol++) {
    if (override_counts[symbol]) {
      fprintf(stderr, "%u,%llu\n", symbol, (unsigned long long)override_counts[symbol]);
    }
  }
  free(override_counts);
  ts_parser_delete(parser);
  sq_grammar_delete(grammar);
  dlclose(library);
  return 0;
}
