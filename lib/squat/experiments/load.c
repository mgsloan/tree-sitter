#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>

// Time owned loading over already-serialized slabs. SQ_SAFETY_ONLY=1 selects
// the cache-oriented policy used by persistence; the default checks integrity.
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}

int main(int argc, char **argv) {
  void *lib = dlopen(argv[1], RTLD_NOW);
  const TSLanguage *(*fn)(void) = (const TSLanguage *(*)(void))dlsym(lib, argv[2]);
  const TSLanguage *language = fn();
  bool safety_only = getenv("SQ_SAFETY_ONLY") != NULL;
  int repeats = atoi(argv[3]), count = argc - 4, n = 0;
  uint8_t **slabs = malloc((size_t)count * sizeof(uint8_t *));
  uint32_t *sizes = malloc((size_t)count * sizeof(uint32_t));
  TSParser *parser = ts_parser_new();
  ts_parser_set_language(parser, language);
  uint64_t bytes = 0;
  for (int i = 0; i < count; i++) {
    FILE *f = fopen(argv[4 + i], "rb");
    if (!f || fseek(f, 0, SEEK_END)) continue;
    long len = ftell(f);
    rewind(f);
    char *s = malloc((size_t)len + 1);
    if (fread(s, 1, (size_t)len, f) != (size_t)len) return 2;
    fclose(f);
    TSTree *parsed = ts_parser_parse_string(parser, NULL, s, (uint32_t)len);
    free(s);
    if (!parsed) continue;
    SQError e;
    SQPackOptions options = sq_pack_options_default();
    options.repack = true;
    SQTree *packed = sq_tree_pack(parsed, options, &e);
    ts_tree_delete(parsed);
    if (!packed) return 2;
    uint32_t size = 0;
    const void *data = sq_tree_data(packed, &size);
    slabs[n] = malloc(size);
    memcpy(slabs[n], data, size);
    sizes[n] = size;
    bytes += size;
    sq_tree_delete(packed);
    n++;
  }

  ts_parser_delete(parser);
  double best = 1e18;
  for (int r = 0; r < repeats; r++) {
    double start = now();
    for (int i = 0; i < n; i++) {
      SQError e;
      SQTree *loaded = safety_only
                           ? sq_tree_from_bytes_safety_checked(language, slabs[i], sizes[i], &e)
                           : sq_tree_from_bytes(language, slabs[i], sizes[i], &e);
      if (!loaded) {
        fprintf(stderr, "%s\n", sq_error_string(e));
        return 1;
      }

      sq_tree_delete(loaded);
    }

    double elapsed = (now() - start) * 1000;
    if (elapsed < best) best = elapsed;
  }

  printf("{\"validation\":\"%s\",\"files\":%d,\"slab_bytes\":%llu,\"load_ms\":%.4f,"
         "\"us_per_file\":%.3f}\n",
         safety_only ? "safety" : "integrity", n, (unsigned long long)bytes, best,
         best * 1e3 / n);
  return 0;
}
