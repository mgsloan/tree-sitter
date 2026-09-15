#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>

static double now(void) {
  struct timespec time;
  clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &time);
  return time.tv_sec + time.tv_nsec * 1e-9;
}

static int compare_double(const void *left, const void *right) {
  double a = *(const double *)left, b = *(const double *)right;
  return (a > b) - (a < b);
}

static double measure(const TSLanguage *language, const void *cache, size_t cache_size,
                      unsigned repeats) {
  double *samples = malloc((size_t)repeats * sizeof(double));
  if (!samples) return -1;
  for (unsigned repeat = 0; repeat < repeats; repeat++) {
    SQError error;
    double start = now();
    SQPackContext *context = cache
                                 ? sq_pack_context_new_with_grammar_cache(
                                       language, cache, cache_size, &error)
                                 : sq_pack_context_new(language, &error);
    samples[repeat] = (now() - start) * 1e6;
    if (!context) {
      fprintf(stderr, "%s\n", sq_error_string(error));
      free(samples);
      return -1;
    }
    sq_pack_context_delete(context);
  }
  qsort(samples, repeats, sizeof(double), compare_double);
  double result = samples[repeats / 2];
  free(samples);
  return result;
}

int main(int argc, char **argv) {
  if (argc != 4) {
    fprintf(stderr, "usage: context-bench LIBRARY SYMBOL REPEATS\n");
    return 2;
  }
  unsigned repeats = (unsigned)atoi(argv[3]);
  if (!repeats) return 2;
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) return 2;
  const TSLanguage *(*language_function)(void) =
      (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  if (!language_function) return 2;
  const TSLanguage *language = language_function();
  uint32_t symbols = language->symbol_count + language->alias_count;
  TSSymbolMetadata *metadata = malloc((size_t)symbols * sizeof(*metadata));
  if (!metadata) return 2;
  memcpy(metadata, language->symbol_metadata, (size_t)symbols * sizeof(*metadata));
  uint32_t supertypes = 0;
  for (uint32_t symbol = 0; symbol < symbols; symbol++) {
    supertypes += metadata[symbol].supertype;
    metadata[symbol].supertype = false;
  }
  TSLanguage fields = *language;
  fields.symbol_metadata = metadata;
  TSLanguage symbols_only = fields;
  symbols_only.field_count = 0;
  symbols_only.production_id_count = 0;

  SQError error;
  SQPackContext *derived = sq_pack_context_new(language, &error);
  if (!derived) return 1;
  uint32_t cache_size = sq_pack_context_grammar_cache_size(derived);
  void *cache = cache_size ? malloc(cache_size) : NULL;
  if (cache_size &&
      (!cache || !sq_pack_context_copy_grammar_cache(derived, cache, cache_size, &error))) return 1;
  sq_pack_context_delete(derived);

  double symbols_us = measure(&symbols_only, NULL, 0, repeats);
  double fields_us = measure(&fields, NULL, 0, repeats);
  double derive_us = measure(language, NULL, 0, repeats);
  // A nonnull sentinel distinguishes the narrow-grammar empty cache from the
  // derive path without requiring an allocation.
  uint8_t empty = 0;
  double cached_us = measure(language, cache_size ? cache : &empty, cache_size, repeats);
  if (symbols_us < 0 || fields_us < 0 || derive_us < 0 || cached_us < 0) return 1;
  printf("{\"symbols\":%u,\"fields\":%u,\"productions\":%u,\"supertypes\":%u,"
         "\"cache_bytes\":%u,\"symbols_us\":%.3f,\"symbols_fields_us\":%.3f,"
         "\"derive_us\":%.3f,\"cached_us\":%.3f}\n",
         symbols, language->field_count, language->production_id_count, supertypes, cache_size,
         symbols_us, fields_us, derive_us, cached_us);
  free(cache);
  free(metadata);
  dlclose(library);
  return 0;
}
