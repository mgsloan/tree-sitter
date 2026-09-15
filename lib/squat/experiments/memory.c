// Linux/glibc allocation accounting, not RSS: the tracker and shared grammar
// mappings are deliberately outside the measured tree allocations.
#include "../internal.h"
#include <dlfcn.h>
#include <inttypes.h>
#include <malloc.h>
#include <stdio.h>

void *__real_malloc(size_t);
void *__real_calloc(size_t, size_t);
void *__real_realloc(void *, size_t);
void *__real_aligned_alloc(size_t, size_t);
void __real_free(void *);

typedef struct {
  void *pointer;
  size_t requested;
  size_t usable;
} Allocation;

typedef struct {
  uint64_t requested;
  uint64_t usable;
  uint64_t allocations;
} Usage;

// An out-of-band table preserves the real allocation sizes and alignment.
// A fixed bound makes exhaustion an error rather than a silent undercount.
#define TABLE_SIZE (1u << 22)
#define TOMBSTONE ((void *)(uintptr_t)1)
static Allocation allocations[TABLE_SIZE];
static Usage live, peak;
static bool tracking;

static void require(bool condition, const char *message) {
  if (!condition) {
    fprintf(stderr, "memory-bench: %s\n", message);
    exit(1);
  }
}

static Allocation *find_allocation(void *pointer, bool insert) {
  uintptr_t hash = (uintptr_t)pointer >> 4;
  hash ^= hash >> 23;
  hash *= UINT64_C(0x2127599bf4325c37);
  hash ^= hash >> 47;
  Allocation *vacant = NULL;
  for (size_t probe = 0; probe < TABLE_SIZE; probe++) {
    Allocation *entry = &allocations[(hash + probe) & (TABLE_SIZE - 1)];
    if (entry->pointer == pointer) {
      return entry;
    }

    if (entry->pointer == TOMBSTONE && !vacant) {
      vacant = entry;
    }

    if (!entry->pointer) {
      return insert ? (vacant ? vacant : entry) : NULL;
    }
  }

  require(!insert || vacant, "allocation table exhausted");
  return insert ? vacant : NULL;
}

static void record_allocation(void *pointer, size_t requested) {
  if (!tracking || !pointer) {
    return;
  }

  Allocation *entry = find_allocation(pointer, true);
  require(entry->pointer != pointer, "duplicate allocation");
  *entry = (Allocation){pointer, requested, malloc_usable_size(pointer)};
  live.requested += entry->requested;
  live.usable += entry->usable;
  live.allocations++;
  if (live.requested > peak.requested) peak.requested = live.requested;
  if (live.usable > peak.usable) peak.usable = live.usable;
  if (live.allocations > peak.allocations) peak.allocations = live.allocations;
}

static void forget_allocation(void *pointer) {
  if (!pointer) {
    return;
  }

  Allocation *entry = find_allocation(pointer, false);
  if (entry) {
    live.requested -= entry->requested;
    live.usable -= entry->usable;
    live.allocations--;
    entry->pointer = TOMBSTONE;
  }
}

void *__wrap_malloc(size_t size) {
  void *pointer = __real_malloc(size);
  record_allocation(pointer, size);
  return pointer;
}

void *__wrap_calloc(size_t count, size_t size) {
  void *pointer = __real_calloc(count, size);
  record_allocation(pointer, count * size);
  return pointer;
}

void *__wrap_aligned_alloc(size_t alignment, size_t size) {
  void *pointer = __real_aligned_alloc(alignment, size);
  record_allocation(pointer, size);
  return pointer;
}

void *__wrap_realloc(void *pointer, size_t size) {
  // Keep failed reallocations in the live set. glibc realloc(p, 0) frees p.
  Allocation *entry = pointer ? find_allocation(pointer, false) : NULL;
  void *next = __real_realloc(pointer, size);
  if (next || !size) {
    if (entry) {
      live.requested -= entry->requested;
      live.usable -= entry->usable;
      live.allocations--;
      entry->pointer = TOMBSTONE;
    }

    record_allocation(next, size);
  }

  return next;
}

void __wrap_free(void *pointer) {
  forget_allocation(pointer);
  __real_free(pointer);
}

static Usage difference(Usage total, Usage baseline) {
  require(total.requested >= baseline.requested && total.usable >= baseline.usable &&
              total.allocations >= baseline.allocations,
          "tree accounting fell below its baseline");
  return (Usage){total.requested - baseline.requested, total.usable - baseline.usable,
                 total.allocations - baseline.allocations};
}

static void print_usage(const char *name, Usage usage) {
  printf("\"%s\":{\"requested\":%" PRIu64 ",\"usable\":%" PRIu64 ",\"allocations\":%" PRIu64 "}",
         name, usage.requested, usage.usable, usage.allocations);
}

static void check_tracker(void) {
  tracking = true;
  void *pointer = __wrap_malloc(17);
  require(live.requested == 17 && live.allocations == 1, "malloc accounting");
  pointer = __wrap_realloc(pointer, 4096);
  require(live.requested == 4096 && live.allocations == 1, "realloc accounting");
  __wrap_free(pointer);
  pointer = __wrap_calloc(3, 19);
  require(live.requested == 57 && live.allocations == 1, "calloc accounting");
  __wrap_free(pointer);
  pointer = __wrap_aligned_alloc(64, 128);
  require(live.requested == 128 && live.allocations == 1, "aligned accounting");
  __wrap_free(pointer);
  require(!live.requested && !live.usable && !live.allocations, "tracker cleanup");
  tracking = false;
  peak = (Usage){0};
}

int main(int argc, char **argv) {
  require(argc == 4, "usage: memory-bench LIBRARY SYMBOL SOURCE");
  check_tracker();
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) {
    fprintf(stderr, "%s\n", dlerror());
    return 1;
  }

  const TSLanguage *(*language_function)(void) =
      (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  require(language_function != NULL, "grammar symbol missing");
  const TSLanguage *language = language_function();
  FILE *file = fopen(argv[3], "rb");
  require(file && !fseek(file, 0, SEEK_END), "cannot open source");
  long length = ftell(file);
  require(length >= 0 && (uint64_t)length <= UINT32_MAX, "invalid source size");
  rewind(file);
  char *source = __real_malloc((size_t)length + 1);
  require(source && fread(source, 1, (size_t)length, file) == (size_t)length, "cannot read source");
  fclose(file);

  tracking = true;
  TSParser *parser = ts_parser_new();
  require(ts_parser_set_language(parser, language), "incompatible grammar");
  TSTree *parsed = ts_parser_parse_string(parser, NULL, source, (uint32_t)length);
  require(parsed != NULL, "parse failed");
  ts_parser_delete(parser);
  Usage mainline = live;
  Usage parse_peak = peak;
  uint32_t nodes = ts_node_descendant_count(ts_tree_root_node(parsed));
  unsigned occupancy = getenv("SQ_CAPACITY_PERCENT") ? (unsigned)atoi(getenv("SQ_CAPACITY_PERCENT")) : 75;
  require(occupancy > 0 && occupancy <= 100, "invalid capacity occupancy percentage");
  uint32_t initial_capacity = getenv("SQ_CAPACITY_PERCENT")
      ? (uint32_t)((uint64_t)nodes * 100 / (SQ_GROUP_SIZE * occupancy) + 1) : 0;
  const char *point_option = getenv("SQ_POINTS");
  bool points = !point_option || strcmp(point_option, "0");
  printf("{\"points\":%d,\"group_size\":%u,\"source_bytes\":%ld,\"nodes\":%u,", points,
         SQ_GROUP_SIZE, length, nodes);
  print_usage("mainline", mainline);
  putchar(',');
  print_usage("parse_peak", parse_peak);

  Usage packed_usage[2];
  for (unsigned compact = 0; compact < 2; compact++) {
    peak = live;
    SQPackOptions options = sq_pack_options_default();
    options.points = points;
    options.repack = compact;
    options.initial_group_capacity = initial_capacity;
    SQError error;
    SQTree *tree = sq_tree_pack(parsed, options, &error);
    require(tree != NULL, sq_error_string(error));
    Usage retained = difference(live, mainline);
    packed_usage[compact] = retained;
    Usage pack_peak = peak;
    require(sq_node_descendant_count(sq_tree_root_node(tree)) == nodes,
            "packed node count differs");

    // Cross-check the retained allocation tracker against the actual owners.
    size_t expected = sq_runtime_size(language) + tree->size;
    expected = (expected + SQ_COLUMN_ALIGNMENT - 1) & ~(size_t)(SQ_COLUMN_ALIGNMENT - 1);
    require(tree->storage == SQ_STORAGE_COLOCATED && retained.requested == expected &&
                retained.allocations == 1,
            "unexpected Squatter retained allocation");
    printf(",\"%s\":{", compact ? "compact" : "default");
    print_usage("retained", retained);
    putchar(',');
    print_usage("pack_peak_with_mainline", pack_peak);
    printf(",\"slab_bytes\":%u,\"groups\":%u,\"group_capacity\":%u}", tree->size,
           sq_tree_group_count(tree), sq_tree_group_capacity(tree));
    sq_tree_delete(tree);
    Usage remaining = difference(live, mainline);
    require(!remaining.requested && !remaining.usable && !remaining.allocations,
            "packing leaked memory or changed the mainline tree");
  }

  ts_tree_delete(parsed);
  require(!live.requested && !live.usable && !live.allocations, "tree cleanup leaked");

  // Measure the public parse-and-pack lifecycle too: it holds the parser and
  // mainline tree during conversion, unlike the parser-free staged pack above.
  for (unsigned compact = 0; compact < 2; compact++) {
    peak = (Usage){0};
    parser = ts_parser_new();
    require(ts_parser_set_language(parser, language), "incompatible grammar");
    SQPackOptions options = sq_pack_options_default();
    options.repack = compact;
    options.initial_group_capacity = initial_capacity;
    SQError error;
    SQTree *tree = sq_tree_parse(parser, source, (uint32_t)length, options, &error);
    require(tree != NULL, sq_error_string(error));
    ts_parser_delete(parser);
    require(live.requested == packed_usage[compact].requested &&
                live.allocations == packed_usage[compact].allocations,
            "parse-and-pack retained memory differs from staged packing");
    putchar(',');
    print_usage(compact ? "compact_parse_pack_peak" : "default_parse_pack_peak", peak);
    sq_tree_delete(tree);
    require(!live.requested && !live.usable && !live.allocations, "parse-and-pack cleanup leaked");
  }

  tracking = false;
  puts(",\"cleanup_zero\":true}");
  __real_free(source);
  dlclose(library);
  return 0;
}
