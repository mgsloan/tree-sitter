#define _POSIX_C_SOURCE 200809L
// Experiment harness: compile against a frozen source tree with byte-rounding.patch.
#include "../internal.h"
#include "../include/tree_sitter/squat_query.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>

static volatile uint64_t sink;
static bool end_to_end;
static unsigned query_kind;
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}

typedef uint64_t (*Operation)(void *);
static void measure(Operation op, void *arg, unsigned repeats, double seconds) {
  uint64_t expected = op(arg);
  unsigned loops = 1;
  for (;;) {
    double start = now();
    for (unsigned i = 0; i < loops; i++) sink = op(arg);
    if (now() - start >= seconds || loops >= 65536) break;
    loops *= 2;
  }
  printf("{\"loops\":%u,\"checksum\":%llu,\"us\":[", loops, (unsigned long long)expected);
  for (unsigned r = 0; r < repeats; r++) {
    uint64_t sum = 0;
    double start = now();
    for (unsigned i = 0; i < loops; i++) sum += op(arg);
    double elapsed = now() - start;
    assert(sum == expected * loops);
    sink = sum;
    printf("%s%.6f", r ? "," : "", elapsed * 1e6 / loops);
  }
  printf("]}");
}

typedef struct {
  TSTree *parsed;
  SQTree *tree;
  SQNode *nodes;
  uint32_t count, symbol;
  TSFieldId field;
  SQQuery *query[3];
  SQQueryCursor *query_cursor;
} Input;
typedef struct { Input *inputs; unsigned count; } Batch;

static uint64_t attributes_sum(SQCursorAttributes a) {
  uint64_t sum = (uint64_t)a.symbol + a.grammar_symbol + a.field_id + a.start_byte + a.end_byte +
                 a.is_named + a.is_extra + a.is_missing + a.is_error + a.has_error;
#if SQ_INCLUDE_POINTS
  sum += (uint64_t)a.start_point.row + a.start_point.column + a.end_point.row + a.end_point.column;
#endif
  return sum;
}
static uint64_t pack(void *arg) {
  Batch *b = arg; uint64_t sum = 0;
  for (unsigned i = 0; i < b->count; i++) {
    SQError error;
    SQTree *tree = sq_tree_pack(b->inputs[i].parsed, sq_pack_options_default(), &error);
    assert(tree && error == SQ_OK);
    sum += sq_node_descendant_count(sq_tree_root_node(tree));
    sq_tree_delete(tree);
  }
  return sum;
}
static uint64_t attributes(void *arg) {
  Batch *b = arg; uint64_t sum = 0;
  for (unsigned i = 0; i < b->count; i++) {
    for (uint32_t j = 0; j < b->inputs[i].count; j++) {
      SQCursorAttributes a;
      sq_node_attributes(b->inputs[i].nodes[j], &a);
      sum += attributes_sum(a);
    }
  }
  return sum;
}
static bool use_cache = true;
static uint64_t cached(void *arg) {
  Batch *b = arg; uint64_t sum = 0;
  for (unsigned i = 0; i < b->count; i++) {
    SQNodeIterator *it = sq_node_iterator_new(sq_tree_root_node(b->inputs[i].tree), use_cache);
    assert(it);
    while (sq_node_iterator_next(it).tree) {
      SQCursorAttributes a;
      sq_node_iterator_attributes(it, &a);
      sum += attributes_sum(a);
    }
    sq_node_iterator_delete(it);
  }
  return sum;
}
static uint64_t uncached(void *arg) {
  use_cache = false; uint64_t sum = cached(arg); use_cache = true; return sum;
}
static uint64_t cursor_walk(void *arg) {
  Batch *b = arg; uint64_t sum = 0;
  for (unsigned i = 0; i < b->count; i++) {
    SQCursor *cursor = sq_cursor_new(sq_tree_root_node(b->inputs[i].tree));
    assert(cursor);
    for (;;) {
      SQCursorAttributes a; sq_cursor_attributes(cursor, &a); sum += attributes_sum(a);
      if (sq_cursor_goto_first_child(cursor)) continue;
      while (!sq_cursor_goto_next_sibling(cursor)) {
        if (!sq_cursor_goto_parent(cursor)) goto finished;
      }
    }
finished:
    sq_cursor_delete(cursor);
  }
  return sum;
}
static uint64_t random_ids(void *arg) {
  Batch *b = arg; uint64_t sum = 0;
  for (unsigned i = 0; i < b->count; i++) {
    Input *in = &b->inputs[i]; uint32_t state = 123456789;
    for (uint32_t j = 0; j < in->count; j++) {
      state = state * 1664525 + 1013904223;
      SQNode node = in->nodes[((uint64_t)state * in->count) >> 32];
      sum += sq_node_symbol_id(node) + sq_node_field_value(node) + sq_node_grammar_id(node);
    }
  }
  return sum;
}
static uint64_t scans(void *arg) {
  Batch *b = arg; uint64_t sum = 0;
  for (unsigned i = 0; i < b->count; i++) {
    Input *in = &b->inputs[i];
    for (uint32_t g = 0; g < sq_tree_group_count(in->tree); g++) {
      sum += sq_tree_group_symbol_equal(in->tree, g, in->symbol);
      sum += sq_tree_group_field_equal(in->tree, g, in->field);
    }
  }
  return sum;
}
static uint64_t queries(void *arg) {
  Batch *b = arg; uint64_t sum = 0;
  for (unsigned i = 0; i < b->count; i++) {
    Input *in = &b->inputs[i];
    sq_query_cursor_exec(in->query_cursor, in->query[query_kind], sq_tree_root_node(in->tree));
    SQQueryMatch m; uint32_t capture;
    while (sq_query_cursor_next_capture(in->query_cursor, &m, &capture)) {
      sum += 1 + (uint64_t)sq_node_start_byte(m.captures[capture].node) + m.pattern_index;
    }
    assert(sq_query_cursor_error(in->query_cursor) == SQ_QUERY_OK);
  }
  return sum;
}

static uint64_t structural_queries(void *arg) {
  query_kind = 1; uint64_t sum = queries(arg); query_kind = 0; return sum;
}
static uint64_t field_queries(void *arg) {
  query_kind = 2; uint64_t sum = queries(arg); query_kind = 0; return sum;
}
// Count up to 512 distinct named parent/child relationships and select the top twelve.
// Field queries use real field names; fieldless grammars reuse structural patterns.
static void structural_source(Input *in, char *source, size_t capacity, bool fields) {
  struct Pattern { char text[512]; uint32_t count; } patterns[512] = {0};
  unsigned count = 0;
  TSTreeCursor cursor = ts_tree_cursor_new(ts_tree_root_node(in->parsed));
  for (;;) {
    TSNode child = ts_tree_cursor_current_node(&cursor), parent = ts_node_parent(child);
    const char *field = ts_tree_cursor_current_field_name(&cursor);
    if (!ts_node_is_null(parent) && ts_node_is_named(parent) && ts_node_is_named(child) &&
        !ts_node_is_error(parent) && !ts_node_is_error(child) && (!fields || field)) {
      char pattern[512];
      int n = snprintf(pattern, sizeof(pattern), "(%s %s%s(%s) @child) @parent\n",
          ts_node_type(parent), fields ? field : "", fields ? ": " : "", ts_node_type(child));
      assert(n > 0 && (size_t)n < sizeof(pattern));
      unsigned j = 0;
      while (j < count && strcmp(patterns[j].text, pattern)) j++;
      if (j < count) patterns[j].count++;
      else if (count < 512) { strcpy(patterns[count].text, pattern); patterns[count++].count = 1; }
    }
    if (ts_tree_cursor_goto_first_child(&cursor)) continue;
    while (!ts_tree_cursor_goto_next_sibling(&cursor)) {
      if (!ts_tree_cursor_goto_parent(&cursor)) goto finished_patterns;
    }
  }
finished_patterns:
  ts_tree_cursor_delete(&cursor); source[0] = 0;
  for (unsigned k = 0; k < 12 && k < count; k++) {
    unsigned best = 0;
    for (unsigned j = 1; j < count; j++) if (patterns[j].count > patterns[best].count) best = j;
    assert(strlen(source) + strlen(patterns[best].text) < capacity);
    strcat(source, patterns[best].text); patterns[best].count = 0;
  }
  if (!count && fields) structural_source(in, source, capacity, false);
  else if (!count) strcpy(source, "(_) @hit");
}

static void prepare_query(Input *in, const TSLanguage *language) {
  uint32_t symbols = sq_symbols(in->tree), *counts = calloc(symbols, sizeof(uint32_t));
  uint32_t *fields = calloc(language->field_count + 1, sizeof(uint32_t));
  assert(counts && fields);
  for (uint32_t j = 0; j < in->count; j++) {
    SQNode node = in->nodes[j];
    if (sq_node_is_named(node) && !sq_node_is_error(node)) counts[sq_node_symbol_id(node)]++;
    fields[sq_node_field_value(node)]++;
  }
  in->symbol = 0; in->field = 0;
  for (uint32_t j = 1; j < symbols; j++) if (counts[j] > counts[in->symbol]) in->symbol = j;
  for (uint32_t j = 1; j <= language->field_count; j++)
    if (!in->field || fields[j] > fields[in->field]) in->field = (TSFieldId)j;
  char source[4096] = {0}; size_t used = 0;
  for (unsigned k = 0; k < 3; k++) {
    uint32_t best = 0;
    for (uint32_t j = 1; j < symbols; j++) if (counts[j] > counts[best]) best = j;
    if (!counts[best]) break;
    const char *name = ts_language_symbol_name(language, sq_decode_symbol(in->tree, best));
    int written = snprintf(source + used, sizeof(source) - used, "(%s) @hit\n", name);
    assert(written > 0 && (size_t)written < sizeof(source) - used);
    used += (size_t)written; counts[best] = 0;
  }
  if (!used) strcpy(source, "(_) @hit");
  for (query_kind = 0; query_kind < (end_to_end ? 3u : 1u); query_kind++) {
    if (query_kind) structural_source(in, source, sizeof(source), query_kind == 2);
    uint32_t offset; TSQueryError error;
    in->query[query_kind] = sq_query_new(language, source, (uint32_t)strlen(source), &offset, &error);
    assert(in->query[query_kind]);
    TSQuery *mainline = ts_query_new(language, source, (uint32_t)strlen(source), &offset, &error);
    assert(mainline);
    TSQueryCursor *cursor = ts_query_cursor_new();
    if (!in->query_cursor) in->query_cursor = sq_query_cursor_new();
    assert(cursor && in->query_cursor);
    ts_query_cursor_exec(cursor, mainline, ts_tree_root_node(in->parsed));
    sq_query_cursor_exec(in->query_cursor, in->query[query_kind], sq_tree_root_node(in->tree));
    TSQueryMatch a; SQQueryMatch b; uint32_t ai, bi;
    while (ts_query_cursor_next_capture(cursor, &a, &ai)) {
      assert(sq_query_cursor_next_capture(in->query_cursor, &b, &bi));
      assert(a.pattern_index == b.pattern_index && a.captures[ai].index == b.captures[bi].index);
      assert(ts_node_start_byte(a.captures[ai].node) == sq_node_start_byte(b.captures[bi].node));
      assert(ts_node_end_byte(a.captures[ai].node) == sq_node_end_byte(b.captures[bi].node));
    }
    assert(!sq_query_cursor_next_capture(in->query_cursor, &b, &bi));
    ts_query_cursor_delete(cursor); ts_query_delete(mainline);
  }
  query_kind = 0; free(counts); free(fields);
}

// Standalone column probes cover widths unavailable in the real grammar sample.
typedef struct { SQTree tree; uint16_t *out; uint32_t count; } Micro;
static uint64_t micro_seq(void *arg) {
  Micro *m = arg; uint64_t sum = 0;
  for (uint32_t j = 0; j < m->count; j++) sum += sq_node_symbol_id((SQNode){&m->tree, j});
  return sum;
}
static uint64_t micro_random(void *arg) {
  Micro *m = arg; uint64_t sum = 0; uint32_t state = 123456789;
  for (uint32_t j = 0; j < m->count; j++) {
    state = state * 1664525 + 1013904223;
    sum += sq_node_symbol_id((SQNode){&m->tree, state & (m->count - 1)});
  }
  return sum;
}
static uint64_t micro_unpack(void *arg) {
  Micro *m = arg;
  sq_unpack_select(0)(m->tree.data + m->tree.layout.symbol, 0, m->count,
                       m->tree.layout.symbol_bits, m->out);
  uint64_t sum = 0;
  for (uint32_t j = 0; j < m->count; j++) sum += m->out[j];
  return sum;
}
static uint64_t micro_scan(void *arg) {
  Micro *m = arg; uint64_t sum = 0;
  for (uint32_t g = 0; g < m->count / SQ_GROUP_SIZE; g++)
    sum += sq_tree_group_symbol_equal(&m->tree, g, 1);
  return sum;
}
static void micro(unsigned repeats) {
  const char *names[] = {"sequential", "random", "unpack", "scan"};
  Operation ops[] = {micro_seq, micro_random, micro_unpack, micro_scan};
  printf("[");
  for (uint8_t required = 2; required <= 15; required++) {
    for (unsigned rounded = 0; rounded < 2; rounded++) {
      uint8_t bits = rounded ? (required <= 8 ? 8 : 16) : required;
      Micro m = {.count = 262144};
      m.tree.layout.waste = sizeof(SQHeader);
      m.tree.layout.symbol = sizeof(SQHeader) + (uint32_t)sq_column_size(m.count / SQ_GROUP_SIZE, SQ_WASTE_BITS);
      m.tree.layout.symbol_bits = bits; m.tree.layout.symbol_lanes = 64 / bits;
      m.tree.layout.symbol_mask = (1u << bits) - 1;
      m.tree.data = sq_allocate_data(m.tree.layout.symbol + (size_t)sq_column_size(m.count, bits));
      m.out = malloc(m.count * sizeof(uint16_t)); assert(m.tree.data && m.out);
      sq_header(&m.tree)->group_count = m.count / SQ_GROUP_SIZE;
      for (uint32_t j = 0; j < m.count; j++)
        sq_set_packed(m.tree.data, m.tree.layout.symbol, j, bits, (j * 7919u) & ((1u << required) - 1));
      assert(micro_seq(&m) == micro_unpack(&m));
      printf("%s{\"required\":%u,\"stored\":%u,\"bytes\":%llu,\"count\":%u,\"modes\":{",
          required == 2 && !rounded ? "" : ",", required, bits,
          (unsigned long long)sq_column_size(m.count, bits), m.count);
      for (unsigned op = 0; op < 4; op++) {
        printf("%s\"%s\":", op ? "," : "", names[op]); measure(ops[op], &m, repeats, .005);
      }
      printf("}}"); free(m.tree.data); free(m.out);
    }
  }
  puts("]");
}

int main(int argc, char **argv) {
  if (argc == 3 && !strcmp(argv[1], "--micro")) { micro((unsigned)atoi(argv[2])); return 0; }
  end_to_end = getenv("SQ_END_TO_END") != NULL;
  assert(argc >= 5);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL); assert(library);
  const TSLanguage *(*fn)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]); assert(fn);
  const TSLanguage *language = fn(); unsigned repeats = (unsigned)atoi(argv[3]);
  Batch batch = {.count = (unsigned)argc - 4}; batch.inputs = calloc(batch.count, sizeof(Input));
  assert(batch.inputs && repeats);
  TSParser *parser = ts_parser_new(); assert(ts_parser_set_language(parser, language));
  uint64_t nodes = 0, slab = 0, compact = 0, retained = 0;
  for (unsigned i = 0; i < batch.count; i++) {
    Input *in = &batch.inputs[i]; FILE *file = fopen(argv[i + 4], "rb"); assert(file);
    assert(!fseek(file, 0, SEEK_END)); long length = ftell(file); assert(length >= 0);
    rewind(file); char *source = malloc((size_t)length + 1); assert(source);
    assert(fread(source, 1, (size_t)length, file) == (size_t)length); fclose(file);
    in->parsed = ts_parser_parse_string(parser, NULL, source, (uint32_t)length); assert(in->parsed); free(source);
    SQError error; in->tree = sq_tree_pack(in->parsed, sq_pack_options_default(), &error); assert(in->tree);
    in->count = ts_node_descendant_count(ts_tree_root_node(in->parsed));
    in->nodes = malloc(in->count * sizeof(SQNode)); assert(in->nodes);
    uint32_t j = 0;
    for (SQNode n = sq_tree_root_node(in->tree); n.tree; n = sq_node_next_preorder(n)) {
      assert(j < in->count); in->nodes[j++] = n;
    }
    assert(j == in->count); nodes += j; slab += in->tree->size;
    retained += sq_runtime_size(language) + in->tree->size;
    SQTree *copy = sq_tree_repack(in->tree, &error); assert(copy);
    compact += copy->size; sq_tree_delete(copy);
    prepare_query(in, language);
  }
  assert(attributes(&batch) == cached(&batch));
  assert(attributes(&batch) == uncached(&batch));
  assert(attributes(&batch) == cursor_walk(&batch));
  const char *names[] = {"pack", "attributes", "cached", "random_ids", "scans", "queries", "cursor_walk", "uncached", "structural_queries", "field_queries"};
  Operation ops[] = {pack, attributes, cached, random_ids, scans, queries, cursor_walk, uncached, structural_queries, field_queries};
  printf("{\"files\":%u,\"nodes\":%llu,\"slab_bytes\":%llu,\"retained_bytes\":%llu,\"compact_bytes\":%llu,"
         "\"symbol_bits\":%u,\"field_bits\":%u,\"modes\":{", batch.count,
         (unsigned long long)nodes, (unsigned long long)slab, (unsigned long long)retained,
         (unsigned long long)compact, batch.inputs[0].tree->layout.symbol_bits, batch.inputs[0].tree->layout.field_bits);
  for (unsigned op = 0; op < (end_to_end ? 10u : 6u); op++) {
    printf("%s\"%s\":", op ? "," : "", names[op]); measure(ops[op], &batch, repeats, .008);
  }
  puts("}}");
  for (unsigned i = 0; i < batch.count; i++) {
    Input *in = &batch.inputs[i];
    sq_query_cursor_delete(in->query_cursor);
    for (unsigned q = 0; q < (end_to_end ? 3u : 1u); q++) sq_query_delete(in->query[q]);
    free(in->nodes); sq_tree_delete(in->tree); ts_tree_delete(in->parsed);
  }
  free(batch.inputs); ts_parser_delete(parser); dlclose(library);
}
