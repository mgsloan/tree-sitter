#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>

// Repeated batches keep sub-microsecond tiny-tree operations above timer noise.
static uint64_t walk_mainline(TSNode root) {
  uint64_t sum = 0;
  TSTreeCursor cursor = ts_tree_cursor_new(root);
  bool visited_children = false;
  for (;;) {
    TSNode node = ts_tree_cursor_current_node(&cursor);
    if (!visited_children) {
      sum += ts_node_start_byte(node) + ts_node_end_byte(node) + ts_node_symbol(node) +
             ts_node_is_named(node) + ts_node_has_error(node);
#if SQ_INCLUDE_POINTS
      TSPoint s = ts_node_start_point(node), e = ts_node_end_point(node);
      sum += s.row + s.column + e.row + e.column;
#endif
      if (ts_tree_cursor_goto_first_child(&cursor)) continue;
      visited_children = true;
    }
    if (ts_tree_cursor_goto_next_sibling(&cursor)) {
      visited_children = false;
      continue;
    }
    if (!ts_tree_cursor_goto_parent(&cursor)) break;
  }
  ts_tree_cursor_delete(&cursor);
  return sum;
}

static uint64_t walk_squat(SQNode root) {
  uint64_t sum = 0;
  SQCursor *cursor = sq_cursor_new(root);
  bool visited_children = false;
  for (;;) {
    if (!visited_children) {
      SQNode node = sq_cursor_node(cursor);
      sum += sq_node_start_byte(node) + sq_node_end_byte(node) + sq_node_symbol(node) +
             sq_node_is_named(node) + sq_node_has_error(node);
#if SQ_INCLUDE_POINTS
      TSPoint start = sq_node_start_point(node), end = sq_node_end_point(node);
      sum += start.row + start.column + end.row + end.column;
#endif
      if (sq_cursor_goto_first_child(cursor)) continue;
      visited_children = true;
    }
    if (sq_cursor_goto_next_sibling(cursor)) {
      visited_children = false;
      continue;
    }
    if (!sq_cursor_goto_parent(cursor)) break;
  }
  sq_cursor_delete(cursor);
  return sum;
}


typedef struct {
  TSTree *parsed;
  SQTree *packed;
  uint32_t length;
  uint64_t walk_sum;
} Input;
static volatile uint64_t sink;
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}
static uint64_t batch(Input *inputs, int count, int operation, SQPackContext *context) {
  uint64_t sum = 0;
  for (int i = 0; i < count; i++) {
    Input *in = inputs + i;
    if (operation < 2) {
      SQError error;
      SQTree *tree = operation == 0
          ? sq_tree_pack(in->parsed, sq_pack_options_default(), &error)
          : sq_pack_context_pack(context, in->parsed, sq_pack_options_default(), &error);
      assert(tree && error == SQ_OK);
      sum += sq_tree_group_count(tree);
      sq_tree_delete(tree);
    } else if (operation == 2) {
      sum += walk_squat(sq_tree_root_node(in->packed));
    } else {
      SQNode root = sq_tree_root_node(in->packed);
      for (unsigned j = 0; j < 8; j++) {
        uint32_t offset = (uint32_t)(((uint64_t)in->length * j) / 7);
        SQNode node = sq_node_descendant_for_byte_range(root, offset, offset);
        sum += sq_node_start_byte(node) + sq_node_end_byte(node) + sq_node_symbol(node);
      }
    }
  }
  return sum;
}
int main(int argc, char **argv) {
  assert(argc >= 5);
  void *lib = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  assert(lib);
  const TSLanguage *(*language_fn)(void) = (const TSLanguage *(*)(void))dlsym(lib, argv[2]);
  assert(language_fn);
  const TSLanguage *language = language_fn();
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language));
  int count = argc - 4, repeats = atoi(argv[3]);
  assert(repeats > 0 && repeats <= 99);
  Input *inputs = calloc((size_t)count, sizeof(Input));
  assert(inputs);
  uint64_t nodes = 0, groups = 0, bytes = 0, slabs = 0, walk_sum = 0;
  uint32_t single_group = 0;
  SQError error;
  SQPackContext *context = sq_pack_context_new(language, &error);
  assert(context);
  for (int i = 0; i < count; i++) {
    FILE *f = fopen(argv[i + 4], "rb");
    assert(f && !fseek(f, 0, SEEK_END));
    long length = ftell(f);
    assert(length > 0 && length <= 1024);
    rewind(f);
    char source[1024];
    assert(fread(source, 1, (size_t)length, f) == (size_t)length);
    fclose(f);
    Input *in = inputs + i;
    in->length = (uint32_t)length;
    in->parsed = ts_parser_parse_string(parser, NULL, source, in->length);
    assert(in->parsed);
    in->packed = sq_tree_pack(in->parsed, sq_pack_options_default(), &error);
    assert(in->packed);
    in->walk_sum = walk_mainline(ts_tree_root_node(in->parsed));
    assert(in->walk_sum == walk_squat(sq_tree_root_node(in->packed)));
    walk_sum += in->walk_sum;
    nodes += ts_node_descendant_count(ts_tree_root_node(in->parsed));
    groups += sq_tree_group_count(in->packed);
    single_group += sq_tree_group_count(in->packed) == 1;
    bytes += in->length;
    slabs += in->packed->size;
  }
  ts_parser_delete(parser);
  printf("{\"files\":%d,\"nodes\":%llu,\"groups\":%llu,\"single_group\":%u,"
         "\"source_bytes\":%llu,\"slab_bytes\":%llu,\"walk_checksum\":%llu,\"operations\":{",
         count, (unsigned long long)nodes, (unsigned long long)groups, single_group,
         (unsigned long long)bytes, (unsigned long long)slabs, (unsigned long long)walk_sum);
  const char *names[] = {"pack", "context_pack", "walk", "seek8"};
  for (int op = 0; op < 4; op++) {
    uint64_t expected = batch(inputs, count, op, context);
    unsigned loops = 1;
    // Each timed sample lasts at least about 10 ms at calibration speed.
    for (;;) {
      double start = now();
      for (unsigned k = 0; k < loops; k++) sink = batch(inputs, count, op, context);
      if (now() - start >= .010 || loops >= 65536) break;
      loops *= 2;
    }
    printf("%s\"%s\":{\"loops\":%u,\"checksum\":%llu,\"batch_us\":[",
           op ? "," : "", names[op], loops, (unsigned long long)expected);
    for (int r = 0; r < repeats; r++) {
      double start = now();
      uint64_t sum = 0;
      for (unsigned k = 0; k < loops; k++) sum += batch(inputs, count, op, context);
      double elapsed = now() - start;
      assert(sum == expected * loops);
      sink = sum;
      printf("%s%.6f", r ? "," : "", elapsed * 1e6 / loops);
    }
    printf("]}");
  }
  puts("}}");
  for (int i = 0; i < count; i++) {
    sq_tree_delete(inputs[i].packed);
    ts_tree_delete(inputs[i].parsed);
  }
  sq_pack_context_delete(context);
  free(inputs);
  dlclose(lib);
  return 0;
}
