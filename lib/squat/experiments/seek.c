#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>

// Link a frozen node.o with its defined symbols renamed to before_* so both
// implementations seek on the same packed tree, without timing parsing/packing.
SQNode before_sq_node_descendant_for_byte_range(SQNode, uint32_t, uint32_t);
SQNode before_sq_node_named_descendant_for_byte_range(SQNode, uint32_t, uint32_t);
#if SQ_INCLUDE_POINTS
SQNode before_sq_node_descendant_for_point_range(SQNode, TSPoint, TSPoint);
SQNode before_sq_node_named_descendant_for_point_range(SQNode, TSPoint, TSPoint);
#endif

enum { QUERY_COUNT = 128, REPEATS = 5 };

typedef struct {
  SQNode root;
  uint32_t start, end;
  TSPoint start_point, end_point;
  bool named;
} Query;

static volatile uint64_t observed;

static uint32_t random_value(uint64_t *state) {
  *state = *state * UINT64_C(6364136223846793005) + 1;
  return (uint32_t)(*state >> 32);
}

static uint64_t nanoseconds(void) {
  struct timespec time;
  assert(!clock_gettime(CLOCK_THREAD_CPUTIME_ID, &time));
  return (uint64_t)time.tv_sec * 1000000000 + time.tv_nsec;
}

static SQNode lookup(const Query *query, bool before, bool points) {
#if SQ_INCLUDE_POINTS
  if (points) {
    if (before) {
      return query->named
          ? before_sq_node_named_descendant_for_point_range(query->root, query->start_point, query->end_point)
          : before_sq_node_descendant_for_point_range(query->root, query->start_point, query->end_point);
    }

    return query->named
        ? sq_node_named_descendant_for_point_range(query->root, query->start_point, query->end_point)
        : sq_node_descendant_for_point_range(query->root, query->start_point, query->end_point);
  }
#else
  (void)points;
#endif

  if (before) {
    return query->named
        ? before_sq_node_named_descendant_for_byte_range(query->root, query->start, query->end)
        : before_sq_node_descendant_for_byte_range(query->root, query->start, query->end);
  }

  return query->named
      ? sq_node_named_descendant_for_byte_range(query->root, query->start, query->end)
      : sq_node_descendant_for_byte_range(query->root, query->start, query->end);
}

static uint64_t run(const Query *queries, bool before, bool points, unsigned rounds) {
  uint64_t sum = 0;
  uint64_t start = nanoseconds();
  for (unsigned round = 0; round < rounds; round++) {
    for (unsigned i = 0; i < QUERY_COUNT; i++) {
      SQNode node = lookup(&queries[i], before, points);
      sum += node.tree ? node.slot : UINT32_MAX;
    }
  }

  uint64_t elapsed = nanoseconds() - start;
  observed = sum;
  return elapsed;
}

static int compare_u64(const void *a, const void *b) {
  uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;
  return (x > y) - (x < y);
}

static void exercise(TSParser *parser, const char *path, const char *source, uint32_t size,
                     bool mutated, unsigned rounds, int profile) {
  TSTree *parsed = ts_parser_parse_string(parser, NULL, source, size);
  assert(parsed);
  SQError error;
  SQTree *tree = sq_tree_pack(parsed, sq_pack_options_default(), &error);
  assert(tree && error == SQ_OK);
  SQNode root = sq_tree_root_node(tree);
  TSPoint *positions = malloc(((size_t)size + 1) * sizeof(TSPoint));
  assert(positions);
  positions[0] = (TSPoint){0, 0};
  for (uint32_t i = 0; i < size; i++) {
    TSPoint point = positions[i];
    positions[i + 1] = source[i] == '\n' ? (TSPoint){point.row + 1, 0}
                                        : (TSPoint){point.row, point.column + 1};
  }

  for (unsigned mixed = 0; mixed < 2; mixed++) {
    Query queries[QUERY_COUNT];
    uint64_t state = 42;
    for (unsigned i = 0; i < QUERY_COUNT; i++) {
      SQNode node = root;
      if (mixed && i % 2) {
        uint32_t slot = sq_previous_slot(tree, random_value(&state) % sq_tree_slot_count(tree));
        node = (SQNode){tree, slot};
      }

      uint32_t first = sq_node_start_byte(node), last = sq_node_end_byte(node);
      uint32_t start = first + random_value(&state) % (last - first + 1);
      uint32_t end = start;
      if (mixed && i % 3) end += random_value(&state) % 32;
      if (end > size) end = size;
      queries[i] = (Query){node, start, end, positions[start], positions[end], mixed && i % 4 < 2};
    }

    for (unsigned points = 0; points <= SQ_INCLUDE_POINTS; points++) {
      if (profile && points != (unsigned)(profile - 1) % 2) continue;
      for (unsigned i = 0; i < QUERY_COUNT; i++) {
        SQNode before = lookup(&queries[i], true, points);
        SQNode after = lookup(&queries[i], false, points);
        if (!sq_node_eq(before, after)) {
          fprintf(stderr, "%s: mutated=%d mixed=%u points=%u query=%u before=%u after=%u\n",
                  path, mutated, mixed, points, i, before.slot, after.slot);
          abort();
        }
      }

      if (profile) {
        run(queries, profile > 2, points, rounds);
        continue;
      }

      uint64_t timings[2][REPEATS];
      run(queries, true, points, 1);
      run(queries, false, points, 1);
      for (unsigned repeat = 0; repeat < REPEATS; repeat++) {
        for (unsigned order = 0; order < 2; order++) {
          unsigned variant = (repeat + order + mixed + points + mutated) % 2;
          timings[variant][repeat] = run(queries, variant == 0, points, rounds);
        }
      }

      uint64_t samples[2][REPEATS];
      memcpy(samples, timings, sizeof(samples));
      for (unsigned variant = 0; variant < 2; variant++) {
        qsort(timings[variant], REPEATS, sizeof(uint64_t), compare_u64);
      }

      printf("{\"path\":\"%s\",\"mutated\":%s,\"mixed\":%u,\"points\":%u,"
             "\"size\":%u,\"queries\":%u,\"before_ns\":%llu,\"after_ns\":%llu",
             path, mutated ? "true" : "false", mixed, points, size, rounds * QUERY_COUNT,
             (unsigned long long)timings[0][REPEATS / 2],
             (unsigned long long)timings[1][REPEATS / 2]);
      for (unsigned variant = 0; variant < 2; variant++) {
        printf(",\"%s_samples_ns\":[", variant ? "after" : "before");
        for (unsigned repeat = 0; repeat < REPEATS; repeat++) {
          printf("%s%llu", repeat ? "," : "", (unsigned long long)samples[variant][repeat]);
        }

        printf("]");
      }

      printf("}\n");
    }
  }

  free(positions);
  sq_tree_delete(tree);
  ts_tree_delete(parsed);
}

int main(int argc, char **argv) {
  assert(argc == 6);
  // profile: 0 pairs both versions; 1/2 profile current byte/point seeks;
  // 3/4 profile frozen byte/point seeks. Both workloads and input modes run.
  unsigned rounds = (unsigned)strtoul(argv[4], NULL, 10);
  int profile = atoi(argv[5]);
  assert(rounds && profile >= 0 && profile <= 4);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  assert(library);
  const TSLanguage *(*language)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  assert(language);
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language()));
  FILE *inputs = fopen(argv[3], "r");
  assert(inputs);
  char *line = NULL;
  size_t capacity = 0;
  ssize_t length;
  while ((length = getline(&line, &capacity, inputs)) >= 0) {
    if (length && line[length - 1] == '\n') line[--length] = 0;
    FILE *file = fopen(line, "rb");
    assert(file && !fseek(file, 0, SEEK_END));
    long size = ftell(file);
    assert(size >= 0 && size < UINT32_MAX);
    rewind(file);
    char *source = malloc((size_t)size + 1);
    assert(source && fread(source, 1, (size_t)size, file) == (size_t)size);
    fclose(file);
    exercise(parser, line, source, (uint32_t)size, false, rounds, profile);
    if (size) {
      source[size / 2] = '}';
      if (size > 4) source[size / 3] = (char)0xff;
    }

    exercise(parser, line, source, size ? (uint32_t)size - 1 : 0, true, rounds, profile);
    free(source);
  }

  free(line);
  fclose(inputs);
  ts_parser_delete(parser);
  dlclose(library);
}
