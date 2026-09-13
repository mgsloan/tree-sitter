#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>

// Pack a batch of already-parsed trees repeatedly, so per-tree fixed setup
// shows up against the per-node work. Parsing is outside the timed region.
static double now(void) {
  struct timespec time;
  clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &time);
  return time.tv_sec + time.tv_nsec * 1e-9;
}

static int compare_double(const void *left, const void *right) {
  double a = *(const double *)left, b = *(const double *)right;
  return (a > b) - (a < b);
}

int main(int argc, char **argv) {
  if (argc < 5) {
    fprintf(stderr, "usage: small LIBRARY SYMBOL REPEATS SOURCE...\n");
    return 2;
  }

  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) {
    fprintf(stderr, "%s\n", dlerror());
    return 2;
  }

  const TSLanguage *(*language)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  if (!language) return 2;
  int repeats = atoi(argv[3]);
  if (repeats < 1 || repeats > 99) return 2;

  TSParser *parser = ts_parser_new();
  if (!ts_parser_set_language(parser, language())) return 2;

  int count = argc - 4;
  TSTree **trees = calloc((size_t)count, sizeof(TSTree *));
  if (!trees) return 2;
  uint64_t nodes = 0, bytes = 0;
  int parsed = 0;
  for (int i = 0; i < count; i++) {
    FILE *file = fopen(argv[4 + i], "rb");
    if (!file || fseek(file, 0, SEEK_END)) continue;
    long length = ftell(file);
    if (length < 0 || length > 4 * 1024 * 1024) {
      fclose(file);
      continue;
    }

    rewind(file);
    char *source = malloc((size_t)length + 1);
    if (!source || fread(source, 1, (size_t)length, file) != (size_t)length) return 2;
    fclose(file);
    trees[parsed] = ts_parser_parse_string(parser, NULL, source, (uint32_t)length);
    free(source);
    if (!trees[parsed]) continue;
    nodes += ts_node_descendant_count(ts_tree_root_node(trees[parsed]));
    bytes += (uint64_t)length;
    parsed++;
  }

  ts_parser_delete(parser);
  if (!parsed) return 2;

  SQError context_error;
  SQPackContext *context = getenv("SQ_REUSE_CONTEXT")
      ? sq_pack_context_new(language(), &context_error) : NULL;
  if (getenv("SQ_REUSE_CONTEXT") && !context) return 1;
  int loops = getenv("SQ_BATCH_LOOPS") ? atoi(getenv("SQ_BATCH_LOOPS")) : 1;
  if (loops < 1 || loops > 1000) return 2;

  double timings[99];
  for (int repeat = 0; repeat < repeats; repeat++) {
    double start = now();
    for (int loop = 0; loop < loops; loop++) {
      for (int i = 0; i < parsed; i++) {
        SQError error;
        SQTree *packed = context
            ? sq_pack_context_pack(context, trees[i], sq_pack_options_default(), &error)
            : sq_tree_pack(trees[i], sq_pack_options_default(), &error);
        if (!packed) {
          fprintf(stderr, "%s\n", sq_error_string(error));
          return 1;
        }

        sq_tree_delete(packed);
      }
    }

    timings[repeat] = (now() - start) * 1000 / loops;
  }

  qsort(timings, (size_t)repeats, sizeof(double), compare_double);
  printf("{\"files\":%d,\"nodes\":%llu,\"bytes\":%llu,\"median_ms\":%.6f,"
         "\"ns_per_node\":%.3f,\"us_per_file\":%.3f}\n",
         parsed, (unsigned long long)nodes, (unsigned long long)bytes, timings[repeats / 2],
         timings[repeats / 2] * 1e6 / (double)nodes, timings[repeats / 2] * 1e3 / parsed);
  for (int i = 0; i < parsed; i++) ts_tree_delete(trees[i]);
  free(trees);
  sq_pack_context_delete(context);
  dlclose(library);
  return 0;
}
