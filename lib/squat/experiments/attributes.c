#define _POSIX_C_SOURCE 200809L
#include <tree_sitter/squat.h>
#ifdef NDEBUG
#undef NDEBUG
#endif
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

// Every mode consumes the same O(1) attributes. Field IDs, counts and depth
// are excluded. Native bulk APIs also decode a field ID, which is not consumed.
// Parsing, packing and full differential checks are outside calibrated timing.
typedef struct { TSTree *mainline; SQTree *packed; } Input;
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}
static void mainline_attributes(TSNode n, SQCursorAttributes *a) {
  memset(a, 0, sizeof(*a));
  a->type = ts_node_type(n); a->grammar_type = ts_node_grammar_type(n);
  a->symbol = ts_node_symbol(n); a->grammar_symbol = ts_node_grammar_symbol(n);
  a->start_byte = ts_node_start_byte(n); a->end_byte = ts_node_end_byte(n);
#if SQ_INCLUDE_POINTS
  a->start_point = ts_node_start_point(n); a->end_point = ts_node_end_point(n);
#endif
  a->is_named = ts_node_is_named(n); a->is_extra = ts_node_is_extra(n);
  a->is_missing = ts_node_is_missing(n); a->is_error = ts_node_is_error(n);
  a->has_error = ts_node_has_error(n);
}
static void individual_attributes(SQNode n, SQCursorAttributes *a) {
  memset(a, 0, sizeof(*a));
  a->type = sq_node_type(n); a->grammar_type = sq_node_grammar_type(n);
  a->symbol = sq_node_symbol(n); a->grammar_symbol = sq_node_grammar_symbol(n);
  a->start_byte = sq_node_start_byte(n); a->end_byte = sq_node_end_byte(n);
#if SQ_INCLUDE_POINTS
  a->start_point = sq_node_start_point(n); a->end_point = sq_node_end_point(n);
#endif
  a->is_named = sq_node_is_named(n); a->is_extra = sq_node_is_extra(n);
  a->is_missing = sq_node_is_missing(n); a->is_error = sq_node_is_error(n);
  a->has_error = sq_node_has_error(n);
}
static uint64_t checksum(const SQCursorAttributes *a) {
  // Consume strings without hashing their complete contents in the hot loop.
  // Validation compares the complete strings, not just this checksum.
  uint64_t sum = (uint64_t)a->start_byte + a->end_byte + a->symbol + a->grammar_symbol +
      (unsigned char)a->type[0] + (unsigned char)a->grammar_type[0] +
      a->is_named + a->is_extra + a->is_missing + a->is_error + a->has_error;
#if SQ_INCLUDE_POINTS
  sum += (uint64_t)a->start_point.row + a->start_point.column +
      a->end_point.row + a->end_point.column;
#endif
  return sum;
}
static void equal(SQCursorAttributes expected, SQCursorAttributes actual) {
  assert(strcmp(expected.type, actual.type) == 0);
  assert(strcmp(expected.grammar_type, actual.grammar_type) == 0);
  actual.type = expected.type; actual.grammar_type = expected.grammar_type;
  actual.field_id = expected.field_id = 0;
  assert(memcmp(&expected, &actual, sizeof(expected)) == 0);
}
static void validate(Input in) {
  TSTreeCursor mainline = ts_tree_cursor_new(ts_tree_root_node(in.mainline));
  SQCursor *cursor = sq_cursor_new(sq_tree_root_node(in.packed));
  SQNodeIterator *plain = sq_node_iterator_new(sq_tree_root_node(in.packed), false);
  SQNodeIterator *cached = sq_node_iterator_new(sq_tree_root_node(in.packed), true);
  assert(cursor && plain && cached);
  for (;;) {
    SQCursorAttributes expected, actual;
    mainline_attributes(ts_tree_cursor_current_node(&mainline), &expected);
    SQNode node = sq_cursor_node(cursor);
    individual_attributes(node, &actual); equal(expected, actual);
    sq_node_attributes(node, &actual); equal(expected, actual);
    sq_cursor_attributes(cursor, &actual); equal(expected, actual);
    assert(sq_node_eq(node, sq_node_iterator_next(plain)));
    sq_node_iterator_attributes(plain, &actual); equal(expected, actual);
    assert(sq_node_eq(node, sq_node_iterator_next(cached)));
    sq_node_iterator_attributes(cached, &actual); equal(expected, actual);
    bool child = ts_tree_cursor_goto_first_child(&mainline);
    assert(child == sq_cursor_goto_first_child(cursor));
    if (child) continue;
    bool moved;
    for (;;) {
      moved = ts_tree_cursor_goto_next_sibling(&mainline);
      assert(moved == sq_cursor_goto_next_sibling(cursor));
      if (moved) break;
      bool parent = ts_tree_cursor_goto_parent(&mainline);
      assert(parent == sq_cursor_goto_parent(cursor));
      if (!parent) break;
    }
    if (!moved) break;
  }
  assert(!sq_node_iterator_next(plain).tree && !sq_node_iterator_next(cached).tree);
  sq_node_iterator_delete(plain); sq_node_iterator_delete(cached);
  sq_cursor_delete(cursor); ts_tree_cursor_delete(&mainline);
}
static uint64_t walk_mainline(Input in) {
  uint64_t sum = 0;
  TSTreeCursor cursor = ts_tree_cursor_new(ts_tree_root_node(in.mainline));
  for (;;) {
    SQCursorAttributes a;
    mainline_attributes(ts_tree_cursor_current_node(&cursor), &a);
    sum += checksum(&a);
    if (ts_tree_cursor_goto_first_child(&cursor)) continue;
    for (;;) {
      if (ts_tree_cursor_goto_next_sibling(&cursor)) break;
      if (!ts_tree_cursor_goto_parent(&cursor)) {
        ts_tree_cursor_delete(&cursor); return sum;
      }
    }
  }
}
#define CURSOR_WALK(name, read) \
static uint64_t name(Input in) { \
  uint64_t sum = 0; \
  SQCursor *cursor = sq_cursor_new(sq_tree_root_node(in.packed)); \
  assert(cursor); \
  for (;;) { \
    SQCursorAttributes a; \
    read; \
    sum += checksum(&a); \
    if (sq_cursor_goto_first_child(cursor)) continue; \
    for (;;) { \
      if (sq_cursor_goto_next_sibling(cursor)) break; \
      if (!sq_cursor_goto_parent(cursor)) { sq_cursor_delete(cursor); return sum; } \
    } \
  } \
}
CURSOR_WALK(walk_individual, individual_attributes(sq_cursor_node(cursor), &a))
CURSOR_WALK(walk_cursor_bulk, sq_cursor_attributes(cursor, &a))
CURSOR_WALK(walk_node_bulk, sq_node_attributes(sq_cursor_node(cursor), &a))
#define ITERATOR_WALK(name, cache, read) \
static uint64_t name(Input in) { \
  uint64_t sum = 0; \
  SQNodeIterator *iterator = sq_node_iterator_new(sq_tree_root_node(in.packed), cache); \
  assert(iterator); \
  SQNode node; \
  while ((node = sq_node_iterator_next(iterator)).tree) { \
    SQCursorAttributes a; read; sum += checksum(&a); \
  } \
  sq_node_iterator_delete(iterator); return sum; \
}
ITERATOR_WALK(walk_iterator_individual, false, individual_attributes(node, &a))
ITERATOR_WALK(walk_iterator_bulk, false, sq_node_iterator_attributes(iterator, &a))
ITERATOR_WALK(walk_iterator_cached, true, sq_node_iterator_attributes(iterator, &a))
static uint64_t (*const walks[])(Input) = {walk_mainline, walk_individual, walk_cursor_bulk,
    walk_node_bulk, walk_iterator_individual, walk_iterator_bulk, walk_iterator_cached};
static const char *const names[] = {"mainline", "individual", "cursor_bulk", "node_bulk",
    "iterator_individual", "iterator_bulk", "iterator_cached"};
enum { MODES = sizeof(walks) / sizeof(walks[0]) };
static uint64_t batch(Input *inputs, unsigned count, unsigned mode) {
  uint64_t sum = 0;
  for (unsigned i = 0; i < count; i++) sum += walks[mode](inputs[i]);
  return sum;
}
int main(int argc, char **argv) {
  if (argc < 5) {
    fprintf(stderr, "usage: attributes-bench LIBRARY SYMBOL REPEATS SOURCE...\n"); return 2;
  }
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) { fprintf(stderr, "%s\n", dlerror()); return 2; }
  const TSLanguage *(*language_fn)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  assert(language_fn);
  const TSLanguage *language = language_fn();
  unsigned count = (unsigned)argc - 4, repeats = (unsigned)atoi(argv[3]);
  assert(repeats && repeats <= 99);
  Input *inputs = calloc(count, sizeof(Input)); assert(inputs);
  TSParser *parser = ts_parser_new(); assert(ts_parser_set_language(parser, language));
  SQError error;
  SQPackContext *context = sq_pack_context_new(language, &error); assert(context);
  uint64_t nodes = 0, bytes = 0, slab_bytes = 0;
  for (unsigned i = 0; i < count; i++) {
    FILE *file = fopen(argv[4 + i], "rb"); assert(file && !fseek(file, 0, SEEK_END));
    long length = ftell(file); assert(length >= 0 && length <= 16 * 1024 * 1024);
    rewind(file); char *source = malloc((size_t)length + 1); assert(source);
    assert(fread(source, 1, (size_t)length, file) == (size_t)length); fclose(file);
    inputs[i].mainline = ts_parser_parse_string(parser, NULL, source, (uint32_t)length);
    assert(inputs[i].mainline); free(source);
    inputs[i].packed = sq_pack_context_pack(context, inputs[i].mainline,
                                            sq_pack_options_default(), &error);
    assert(inputs[i].packed); validate(inputs[i]);
    nodes += ts_node_descendant_count(ts_tree_root_node(inputs[i].mainline));
    bytes += (uint64_t)length;
    uint32_t size; sq_tree_data(inputs[i].packed, &size); slab_bytes += size;
  }
  sq_pack_context_delete(context); ts_parser_delete(parser);
  uint64_t expected = batch(inputs, count, 0);
  unsigned loops[MODES] = {0}; double samples[MODES][99] = {{0}};
  unsigned rotation = getenv("SQ_ROTATION") ? (unsigned)atoi(getenv("SQ_ROTATION")) % MODES : 0;
  for (unsigned m = 0; m < MODES; m++) {
    unsigned mode = (m + rotation) % MODES;
    assert(batch(inputs, count, mode) == expected);
    loops[mode] = 1;
    for (;;) {
      double start = now(); uint64_t sum = 0;
      for (unsigned k = 0; k < loops[mode]; k++) sum += batch(inputs, count, mode);
      double elapsed = now() - start;
      assert(sum == expected * loops[mode]);
      if (elapsed >= .010 || loops[mode] >= 65536) break;
      loops[mode] *= 2;
    }
  }
  if (getenv("SQ_PROFILE_MODE")) {
    unsigned mode = (unsigned)atoi(getenv("SQ_PROFILE_MODE")); assert(mode < MODES);
    unsigned rounds = getenv("SQ_PROFILE_ROUNDS") ? (unsigned)atoi(getenv("SQ_PROFILE_ROUNDS")) : 500;
    for (unsigned i = 0; i < rounds; i++) assert(batch(inputs, count, mode) == expected);
  } else {
    for (unsigned repeat = 0; repeat < repeats; repeat++) {
      for (unsigned m = 0; m < MODES; m++) {
        unsigned mode = (m + rotation + repeat) % MODES;
        uint64_t sum = 0; double start = now();
        for (unsigned k = 0; k < loops[mode]; k++) sum += batch(inputs, count, mode);
        samples[mode][repeat] = (now() - start) * 1e6 / loops[mode];
        assert(sum == expected * loops[mode]);
      }
    }
  }
  printf("{\"files\":%u,\"nodes\":%llu,\"bytes\":%llu,\"slab_bytes\":%llu,\"checksum\":%llu,\"modes\":{",
      count, (unsigned long long)nodes, (unsigned long long)bytes,
      (unsigned long long)slab_bytes, (unsigned long long)expected);
  for (unsigned mode = 0; mode < MODES; mode++) {
    printf("%s\"%s\":{\"loops\":%u,\"batch_us\":[", mode ? "," : "", names[mode], loops[mode]);
    for (unsigned j = 0; j < repeats; j++) printf("%s%.6f", j ? "," : "", samples[mode][j]);
    printf("]}");
  }
  puts("}}");
  for (unsigned i = 0; i < count; i++) { sq_tree_delete(inputs[i].packed); ts_tree_delete(inputs[i].mainline); }
  free(inputs); dlclose(library); return 0;
}
