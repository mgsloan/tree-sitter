#define _POSIX_C_SOURCE 200809L
#include <tree_sitter/squat.h>
#include "../internal.h"
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

// Same-tree mainline-vs-Squatter comparison: forward checked walk (every node,
// O(1) selected attributes) and N deterministic byte-range descendant seeks.
// Compiled twice (points enabled / SQ_INCLUDE_POINTS=0) to see the effect of
// carrying row/column data through both representations.

static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}

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


static uint32_t xorshift(uint32_t *state) {
  uint32_t x = *state;
  x ^= x << 13; x ^= x >> 17; x ^= x << 5;
  return *state = x;
}

int main(int argc, char **argv) {
  if (argc < 6) {
    fprintf(stderr, "usage: walk-bench LIBRARY SYMBOL SOURCE REPEATS SEEK_ROUNDS\n");
    return 2;
  }

  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) { fprintf(stderr, "%s\n", dlerror()); return 2; }
  const TSLanguage *(*language_fn)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  if (!language_fn) return 2;
  const TSLanguage *language = language_fn();

  FILE *file = fopen(argv[3], "rb");
  if (!file || fseek(file, 0, SEEK_END)) return 2;
  long length = ftell(file);
  rewind(file);
  char *source = malloc((size_t)length + 1);
  if (fread(source, 1, (size_t)length, file) != (size_t)length) return 2;
  fclose(file);

  int repeats = atoi(argv[4]);
  int seek_rounds = atoi(argv[5]);

  TSParser *parser = ts_parser_new();
  ts_parser_set_language(parser, language);
  TSTree *tree = ts_parser_parse_string(parser, NULL, source, (uint32_t)length);
  TSNode root = ts_tree_root_node(tree);
  uint32_t nodes = ts_node_descendant_count(root);

  SQError error;
  SQPackOptions options = sq_pack_options_default();
  SQTree *squat = sq_tree_pack(tree, options, &error);
  if (!squat) { fprintf(stderr, "pack failed: %s\n", sq_error_string(error)); return 1; }
  SQNode squat_root = sq_tree_root_node(squat);

  double mainline_walk = 1e18, squat_walk = 1e18;
  uint64_t sum_m = 0, sum_s = 0;
  bool squat_first = getenv("SQ_SQUAT_FIRST") && atoi(getenv("SQ_SQUAT_FIRST"));
  for (int pass = 0; pass < 2; pass++) {
    bool use_squat = (pass == 0) == squat_first;
    for (int r = 0; r < repeats; r++) {
      double start = now();
      uint64_t sum = use_squat ? walk_squat(squat_root) : walk_mainline(root);
      double elapsed = now() - start;
      if (use_squat) {
        sum_s = sum;
        if (elapsed < squat_walk) squat_walk = elapsed;
      } else {
        sum_m = sum;
        if (elapsed < mainline_walk) mainline_walk = elapsed;
      }
    }
  }
  if (sum_m != sum_s) { fprintf(stderr, "WALK CHECKSUM MISMATCH\n"); return 1; }

  // Deterministic pseudo-random zero-length byte ranges.
  uint32_t *offsets = malloc((size_t)seek_rounds * sizeof(uint32_t));
  uint32_t state = 12345;
  for (int i = 0; i < seek_rounds; i++) offsets[i] = xorshift(&state) % ((uint32_t)length + 1);

  double mainline_seek = 1e18, squat_seek = 1e18;
  uint64_t seek_sum_m = 0, seek_sum_s = 0;
  for (int r = 0; r < repeats; r++) {
    double start = now();
    uint64_t sum = 0;
    for (int i = 0; i < seek_rounds; i++) {
      TSNode n = ts_node_descendant_for_byte_range(root, offsets[i], offsets[i]);
      sum += ts_node_start_byte(n);
    }
    double elapsed = now() - start;
    if (elapsed < mainline_seek) mainline_seek = elapsed;
    seek_sum_m = sum;
  }
  for (int r = 0; r < repeats; r++) {
    double start = now();
    uint64_t sum = 0;
    for (int i = 0; i < seek_rounds; i++) {
      SQNode n = sq_node_descendant_for_byte_range(squat_root, offsets[i], offsets[i]);
      sum += sq_node_start_byte(n);
    }
    double elapsed = now() - start;
    if (elapsed < squat_seek) squat_seek = elapsed;
    seek_sum_s = sum;
  }
  // Known seek differences (see the harness policy) can make sums disagree on
  // some grammars/inputs; report but do not fail on that.
  int seek_mismatch = seek_sum_m != seek_sum_s;

  printf("{\"nodes\":%u,\"mainline_walk_ms\":%.6f,\"squat_walk_ms\":%.6f,"
         "\"mainline_seek_ms\":%.6f,\"squat_seek_ms\":%.6f,\"seek_checksum_mismatch\":%d}\n",
         nodes, mainline_walk * 1000, squat_walk * 1000, mainline_seek * 1000,
         squat_seek * 1000, seek_mismatch);

  sq_tree_delete(squat);
  ts_tree_delete(tree);
  ts_parser_delete(parser);
  free(source); free(offsets);
  dlclose(library);
  return 0;
}
