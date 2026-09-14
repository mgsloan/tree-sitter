#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>
#ifdef BENCH_MEMORY
#define main original_memory_main
#include "memory.c"
#undef main
#endif

static volatile uint64_t checksum;
static const TSLanguage *language;
static TSTree *parsed;
static SQTree *keeper;
static SQPackContext *context;
static uint8_t *slab;
static uint32_t slab_length;
static SQPackOptions options;
static TSSymbol supers[65536];
static uint32_t super_count;

static void check(bool success, SQError error) {
  if (!success) { fprintf(stderr, "%s\n", sq_error_string(error)); exit(1); }
}
static void operation(unsigned op) {
  SQError error = SQ_OK;
  if (op == 0) {
    SQPackContext *created = sq_pack_context_new(language, &error);
    check(created != NULL, error);
    sq_pack_context_delete(created);
  } else if (op <= 3) {
    SQTree *tree = op == 3 ? sq_pack_context_pack(context, parsed, options, &error)
                          : sq_tree_pack(parsed, options, &error);
    check(tree != NULL, error);
    checksum += sq_tree_group_count(tree);
    sq_tree_delete(tree);
  } else if (op <= 5) {
    SQTree *tree = sq_tree_from_bytes(language, slab, slab_length, &error);
    check(tree != NULL, error);
    checksum += sq_tree_group_count(tree);
    sq_tree_delete(tree);
  } else {
    uint64_t sum = 0;
    for (SQNode node = sq_tree_root_node(keeper); node.tree; node = sq_node_next_preorder(node)) {
      for (uint32_t s = 0; s < super_count; s++) sum += sq_node_has_supertype(node, supers[s]);
    }
    checksum += sum;
  }
}
#ifndef BENCH_MEMORY
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}
static void time_operation(unsigned op, const char *name) {
  unsigned loops = 1;
  for (;;) {
    double start = now();
    for (unsigned i = 0; i < loops; i++) operation(op);
    if (now() - start >= 0.015 || loops >= 65536) break;
    loops *= 2;
  }
  printf(",\"%s\":{\"loops\":%u,\"samples_us\":[", name, loops);
  for (unsigned r = 0; r < 5; r++) {
    double start = now();
    for (unsigned i = 0; i < loops; i++) operation(op);
    printf("%s%.6f", r ? "," : "", (now() - start) * 1e6 / loops);
  }
  printf("]}");
}
#else
static void memory_measure(void) {
  // Parsed input, source bytes, and the external grammar mapping are excluded.
  tracking = true;
  SQError error;
  SQTree *trees[16];
  for (unsigned i = 0; i < 16; i++) {
    trees[i] = sq_tree_pack(parsed, options, &error);
    check(trees[i] != NULL, error);
    if (i == 0 || i == 15) {
      printf(",\"trees_%u\":{", i + 1);
      print_usage("retained", live);
      putchar(','); print_usage("peak", peak);
      printf("}");
    }
  }
  for (unsigned i = 0; i < 16; i++) sq_tree_delete(trees[i]);
  assert(live.allocations == 0);
  peak = (Usage){0};
  context = sq_pack_context_new(language, &error);
  check(context != NULL, error);
  printf(",\"context_new\":{"); print_usage("retained", live); printf("}");
  keeper = sq_pack_context_pack(context, parsed, options, &error);
  check(keeper != NULL, error);
  printf(",\"context_and_tree\":{"); print_usage("retained", live); printf("}");
  sq_pack_context_trim(context);
  printf(",\"trimmed_context_and_tree\":{"); print_usage("retained", live); printf("}");
  sq_tree_delete(keeper);
  sq_pack_context_delete(context);
  keeper = NULL; context = NULL;
  assert(live.allocations == 0);
  tracking = false;
}
#endif

int main(int argc, char **argv) {
  assert(argc == 4);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) { fprintf(stderr, "%s\n", dlerror()); return 1; }
  const TSLanguage *(*get_language)(void) = dlsym(library, argv[2]);
  assert(get_language);
  language = get_language();
  for (uint32_t s = 0; s < language->symbol_count + language->alias_count; s++) {
    if (language->symbol_metadata[s].supertype) supers[super_count++] = (TSSymbol)s;
  }
  FILE *file = fopen(argv[3], "rb");
  assert(file && !fseek(file, 0, SEEK_END));
  long length = ftell(file);
  assert(length >= 0 && length <= UINT32_MAX);
  rewind(file);
  char *source = malloc((size_t)length + 1);
  assert(source && fread(source, 1, (size_t)length, file) == (size_t)length);
  fclose(file);
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language));
  parsed = ts_parser_parse_string(parser, NULL, source, (uint32_t)length);
  assert(parsed);
  ts_parser_delete(parser);
  free(source);
  options = sq_pack_options_default();
  options.repack = getenv("SQ_REPACK") != NULL;
  SQError error;
  keeper = sq_tree_pack(parsed, options, &error);
  check(keeper != NULL, error);
  const void *data = sq_tree_data(keeper, &slab_length);
  slab = malloc(slab_length);
  assert(slab);
  memcpy(slab, data, slab_length);
  printf("{\"source_bytes\":%ld,\"nodes\":%u,\"supertypes\":%u,\"states\":%u,"
         "\"dictionary_count\":%u,\"index_bits\":%u,\"slab_bytes\":%u,\"groups\":%u,\"repack\":%s",
         length, sq_node_descendant_count(sq_tree_root_node(keeper)), super_count,
         language->state_count, sq_header(keeper)->supertype_dictionary_count,
         keeper->layout.supertype_bits, slab_length, sq_tree_group_count(keeper),
         options.repack ? "true" : "false");
  // Validate each variant's slab before timing and check equivalent membership.
  SQTree *loaded = sq_tree_from_bytes(language, slab, slab_length, &error);
  check(loaded != NULL, error);
  sq_tree_delete(loaded);
  operation(6);
  printf(",\"membership_checksum\":%llu", (unsigned long long)checksum);
  sq_tree_delete(keeper); keeper = NULL;
#ifdef BENCH_MEMORY
  memory_measure();
#else
  time_operation(0, "context_cold");
  time_operation(1, "pack_cold");
  time_operation(4, "load_cold");
  keeper = sq_tree_pack(parsed, options, &error);
  check(keeper != NULL, error);
  time_operation(2, "pack_warm");
  time_operation(5, "load_warm");
  context = sq_pack_context_new(language, &error);
  check(context != NULL, error);
  time_operation(3, "pack_context");
  time_operation(6, "membership_walk");
  sq_pack_context_delete(context);
  sq_tree_delete(keeper);
#endif
  printf("}\n");
  free(slab);
  ts_tree_delete(parsed);
  dlclose(library);
}
