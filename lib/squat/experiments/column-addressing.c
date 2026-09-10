#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>

// Read-only views isolate address calculation without changing the tree format
// or introducing cache-refresh rules into the production packer. Embed a copy
// of the real descriptor so offset reads have no extra tree-pointer indirection.
typedef struct {
  SQTree tree;
  const uint8_t *groups[G_COLUMNS], *nodes[N_COLUMNS];
  uint32_t group_skip, node_skip;
} PointerView;

static inline uint32_t pointer_node(const PointerView *view, unsigned column,
                                    uint32_t slot, bool cache_skip) {
  uint32_t skip = cache_skip ? view->node_skip
                            : 0;
  return sq_get(view->nodes[column], 0, skip + slot,
                sq_node_width(&view->tree.layout, column));
}

static inline uint32_t pointer_group(const PointerView *view, unsigned column,
                                     uint32_t group, bool cache_skip) {
  uint32_t skip = cache_skip ? view->group_skip
                            : 0;
  return sq_get(view->groups[column], 0, skip + group, sq_group_width(column));
}

// Keep individual getters out of line, like the exported ordinary-node API.
// Separate loop kernels below allow inlining/hoisting, like a bulk consumer.
#define DEFINE_READERS(NAME, NODE, GROUP)                                                        \
  static inline uint32_t NAME##_byte(const PointerView *view, uint32_t slot) {                   \
    const SQTree *tree = &view->tree;                                                            \
    (void)tree;                                                                                \
    unsigned column = N_BYTE, group_column = G_BYTE;                                            \
    uint32_t group = slot / SQ_GROUP_SIZE;                                                      \
    return (GROUP) + (NODE);                                                                   \
  }                                                                                           \
  __attribute__((noinline)) static uint32_t NAME##_getter(const PointerView *view,               \
                                                          uint32_t slot) {                     \
    return NAME##_byte(view, slot);                                                            \
  }                                                                                           \
  __attribute__((noinline)) static uint64_t NAME##_bulk(const PointerView *view,                  \
                                                       const uint32_t *slots, uint32_t count) { \
    uint64_t sum = 0;                                                                          \
    for (uint32_t i = 0; i < count; i++) {                                                      \
      sum += NAME##_byte(view, slots[i]);                                                      \
    }                                                                                         \
    return sum;                                                                               \
  }                                                                                           \
  __attribute__((noinline)) static uint64_t NAME##_columns(const PointerView *view,               \
                                                          const uint32_t *slots,               \
                                                          uint32_t count) {                    \
    const SQTree *tree = &view->tree;                                                            \
    (void)tree;                                                                                \
    uint64_t sum = 0;                                                                          \
    for (uint32_t i = 0; i < count; i++) {                                                      \
      uint32_t slot = slots[i], group = slot / SQ_GROUP_SIZE;                                   \
      for (unsigned column = 0; column < N_COLUMNS; column++) sum += (NODE);                    \
      for (unsigned group_column = 0; group_column < G_COLUMNS; group_column++) sum += (GROUP); \
    }                                                                                         \
    return sum;                                                                               \
  }

DEFINE_READERS(offset, sq_node_get(((SQNode){tree, slot}), column),
               sq_group_get(tree, group_column, group))
DEFINE_READERS(offset_skip,
               sq_get(tree->data, tree->layout.nodes[column], view->node_skip + slot,
                      sq_node_width(&tree->layout, column)),
               sq_get(tree->data, tree->layout.groups[group_column], view->group_skip + group,
                      sq_group_width(group_column)))
DEFINE_READERS(pointer, pointer_node(view, column, slot, false),
               pointer_group(view, group_column, group, false))
DEFINE_READERS(skip, pointer_node(view, column, slot, true),
               pointer_group(view, group_column, group, true))

static double now(void) {
  struct timespec value;
  clock_gettime(CLOCK_MONOTONIC, &value);
  return value.tv_sec + value.tv_nsec * 1e-9;
}

typedef uint32_t (*Getter)(const PointerView *, uint32_t);
typedef uint64_t (*Walk)(const PointerView *, const uint32_t *, uint32_t);
static volatile uint64_t sink;

static uint64_t run(unsigned workload, unsigned variant, const PointerView *view,
                    const uint32_t *slots, uint32_t count) {
  if (workload == 0) {
    Getter volatile getter = (Getter[]){offset_getter, pointer_getter, offset_skip_getter, skip_getter}[variant];
    uint64_t sum = 0;
    for (uint32_t i = 0; i < count; i++) sum += getter(view, slots[i]);
    return sum;
  }
  Walk volatile walk = workload == 1
      ? (Walk[]){offset_bulk, pointer_bulk, offset_skip_bulk, skip_bulk}[variant]
      : (Walk[]){offset_columns, pointer_columns, offset_skip_columns, skip_columns}[variant];
  return walk(view, slots, count);
}

static void measure(const PointerView *view, uint32_t *slots, uint32_t count,
                     const char *order) {
  const char *variants[] = {"offset", "pointer", "offset_cached_skip", "pointer_cached_skip"};
  const char *workloads[] = {"byte_getter", "byte_bulk", "all_columns"};
  for (unsigned workload = 0; workload < 3; workload++) {
    uint64_t expected = run(workload, 0, view, slots, count);
    for (unsigned variant = 1; variant < 4; variant++) {
      assert(run(workload, variant, view, slots, count) == expected);
    }
    // Calibrate equal work, then rotate all four positions on every repeat.
    unsigned batches = 1;
    for (;;) {
      double start = now();
      for (unsigned batch = 0; batch < batches; batch++)
        sink += run(workload, 0, view, slots, count);
      if (now() - start >= 0.003 || batches >= 65536) break;
      batches *= 2;
    }
    for (unsigned repeat = 0; repeat < 12; repeat++) {
      for (unsigned step = 0; step < 4; step++) {
        unsigned variant = (repeat + step) % 4;
        double start = now();
        for (unsigned batch = 0; batch < batches; batch++)
          sink += run(workload, variant, view, slots, count);
        double elapsed = now() - start;
        printf("%s,%s,%s,%u,%u,%u,%.9f\n", order, workloads[workload], variants[variant],
               repeat, count, batches, elapsed * 1e9 / ((double)batches * count));
      }
    }
  }
}

int main(int argc, char **argv) {
  assert(argc == 4);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) { fprintf(stderr, "%s\n", dlerror()); return 1; }
  const TSLanguage *(*language_function)(void) =
      (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  assert(language_function);
  FILE *file = fopen(argv[3], "rb");
  assert(file && !fseek(file, 0, SEEK_END));
  long length = ftell(file);
  assert(length >= 0 && (uint64_t)length <= UINT32_MAX);
  rewind(file);
  char *source = malloc((size_t)length + 1);
  assert(source && fread(source, 1, (size_t)length, file) == (size_t)length);
  fclose(file);
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language_function()));
  SQError error;
  SQTree *tree = sq_tree_parse(parser, source, (uint32_t)length,
                              sq_pack_options_default(), &error);
  assert(tree);
  ts_parser_delete(parser);
  PointerView view = {.tree = *tree};
  for (unsigned column = 0; column < G_COLUMNS; column++)
    view.groups[column] = tree->data + tree->layout.groups[column];
  for (unsigned column = 0; column < N_COLUMNS; column++)
    view.nodes[column] = tree->data + tree->layout.nodes[column];
  // Version 4 has no index bias. Historical bias comparisons require the
  // version-3 experiment revision recorded in the report.
  view.group_skip = 0;
  view.node_skip = view.group_skip * SQ_GROUP_SIZE;
  uint32_t count = sq_node_descendant_count(sq_tree_root_node(tree));
  uint32_t *slots = malloc((size_t)count * sizeof(uint32_t));
  assert(slots);
  uint32_t index = 0;
  for (SQNode node = sq_tree_root_node(tree); node.tree; node = sq_node_next_preorder(node)) {
    assert(index < count);
    slots[index++] = node.slot;
    // Check every decoded value, not only a checksum of each walk.
    for (unsigned column = 0; column < N_COLUMNS; column++) {
      uint32_t expected = sq_node_get(node, column);
      assert(pointer_node(&view, column, node.slot, false) == expected);
      assert(pointer_node(&view, column, node.slot, true) == expected);
    }
    for (unsigned column = 0; column < G_COLUMNS; column++) {
      uint32_t group = node.slot / SQ_GROUP_SIZE;
      uint32_t expected = sq_group_get(tree, column, group);
      assert(pointer_group(&view, column, group, false) == expected);
      assert(pointer_group(&view, column, group, true) == expected);
    }
  }
  assert(index == count);
  puts("order,workload,variant,repeat,nodes,batches,ns_per_node");
  measure(&view, slots, count, "preorder");
  uint32_t random = 42;
  for (uint32_t remaining = count; remaining > 1; remaining--) {
    random ^= random << 13;
    random ^= random >> 17;
    random ^= random << 5;
    uint32_t other = random % remaining, value = slots[remaining - 1];
    slots[remaining - 1] = slots[other];
    slots[other] = value;
  }
  measure(&view, slots, count, "shuffled");
  fprintf(stderr, "points=%d tree_bytes=%zu offset_bytes=%zu pointer_bytes=%zu checksum=%llu\n",
          SQ_INCLUDE_POINTS, sizeof(SQTree), sizeof(uint32_t) * (G_COLUMNS + N_COLUMNS),
          sizeof(void *) * (G_COLUMNS + N_COLUMNS), (unsigned long long)sink);
  free(slots);
  sq_tree_delete(tree);
  free(source);
  dlclose(library);
}
