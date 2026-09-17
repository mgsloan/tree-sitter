#define _DEFAULT_SOURCE
#define _POSIX_C_SOURCE 200809L
#include <tree_sitter/squat.h>
#include "../internal.h"
#include "field_lookup.h"
#include "language_clone.h"
#include "../../src/tree_cursor.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static const char *input_name;
static uint32_t current_ordinal;
static uint32_t expected_field_mismatches;
#define CHECK(condition)                                                                           \
  do {                                                                                             \
    if (!(condition)) {                                                                            \
      fprintf(stderr, "%s node %u: %s (line %d)\n", input_name, current_ordinal, #condition,       \
              __LINE__);                                                                           \
      abort();                                                                                     \
    }                                                                                              \
  } while (0)
static bool strings_equal(const char *a, const char *b) {
  return a && b ? !strcmp(a, b) : a == b;
}

static bool points_equal(TSPoint a, TSPoint b) {
  return a.row == b.row && a.column == b.column;
}

typedef struct {
  TSNode *mainline;
  SQNode *packed;
  uint32_t count;
} Nodes;

static uint32_t ordinal_mainline(Nodes *nodes, TSNode n) {
  if (ts_node_is_null(n)) {
    return UINT32_MAX;
  }

  for (uint32_t i = 0; i < nodes->count; i++) {
    if (ts_node_eq(nodes->mainline[i], n)) {
      return i;
    }
  }

  CHECK(false);
  return 0;
}

static uint32_t ordinal_packed(Nodes *nodes, SQNode n) {
  if (sq_node_is_null(n)) {
    return UINT32_MAX;
  }

  uint32_t low = 0, high = nodes->count;
  while (low < high) {
    uint32_t mid = low + (high - low) / 2;
    if (nodes->packed[mid].slot > n.slot) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }

  CHECK(low < nodes->count && nodes->packed[low].slot == n.slot);
  return low;
}

#define SAME_NODE(main, squat)                                                                     \
  do {                                                                                             \
    uint32_t ma = ordinal_mainline(nodes, (main)), pa = ordinal_packed(nodes, (squat));            \
    if (ma != pa) {                                                                                \
      TSNode actual = nodes->mainline[current_ordinal];                                            \
      fprintf(stderr, "identity mismatch: %s = %u, %s = %u; current %s [%u,%u] slot %u\n", #main,  \
              ma, #squat, pa, ts_node_type(actual), ts_node_start_byte(actual),                    \
              ts_node_end_byte(actual), nodes->packed[current_ordinal].slot);                      \
    }                                                                                              \
    if (ma != pa) {                                                                                \
      for (uint32_t dump = current_ordinal > 2 ? current_ordinal - 2 : 0;                          \
           dump < nodes->count && dump < current_ordinal + 5; dump++) {                            \
        TSNode d = nodes->mainline[dump];                                                          \
        fprintf(stderr, "  %u %s [%u,%u] parent=%u endslot=%u last=%u\n", dump, ts_node_type(d),   \
                ts_node_start_byte(d), ts_node_end_byte(d),                                        \
                ordinal_mainline(nodes, ts_node_parent(d)), sq_node_end_slot(nodes->packed[dump]), \
                sq_node_last_flag(nodes->packed[dump]));                                           \
      }                                                                                            \
    }                                                                                              \
    CHECK(ma == pa);                                                                               \
  } while (0)
static void compare_field(Nodes *nodes, TSNode parent, SQNode packed_parent, TSFieldId field) {
  TSNode expected = ts_node_child_by_field_id(parent, field);
  SQNode actual = sq_node_child_by_field_id(packed_parent, field);
  if (ordinal_mainline(nodes, expected) == ordinal_packed(nodes, actual)) {
    return;
  }

  TSNode ordinary = visible_child_by_field(parent, field);
  SAME_NODE(ordinary, actual);
  if (!expected_field_mismatches) {
    fprintf(stderr,
            "%s: expected field mismatch at %s [%u,%u), field %u: "
            "lookup ordinal %u, visible-child ordinal %u\n",
            input_name, ts_node_type(parent), ts_node_start_byte(parent), ts_node_end_byte(parent),
            field, ordinal_mainline(nodes, expected), ordinal_mainline(nodes, ordinary));
  }

  expected_field_mismatches++;
}

static void compare_node(Nodes *nodes, uint32_t i) {
  current_ordinal = i;
  TSNode a = nodes->mainline[i];
  SQNode b = nodes->packed[i];
  CHECK(ts_node_symbol(a) == sq_node_symbol(b));
  CHECK(sq_node_symbol_id(b) == sq_encode_symbol(b.tree, ts_node_symbol(a)));
  CHECK(ts_node_grammar_symbol(a) == sq_node_grammar_symbol(b));
  CHECK(strings_equal(ts_node_type(a), sq_node_type(b)));
  CHECK(strings_equal(ts_node_grammar_type(a), sq_node_grammar_type(b)));
  CHECK(ts_node_start_byte(a) == sq_node_start_byte(b));
  CHECK(ts_node_end_byte(a) == sq_node_end_byte(b));
  CHECK(points_equal(ts_node_start_point(a), sq_node_start_point(b)));
  CHECK(points_equal(ts_node_end_point(a), sq_node_end_point(b)));
  CHECK(ts_node_is_named(a) == sq_node_is_named(b));
  CHECK(ts_node_is_extra(a) == sq_node_is_extra(b));
  CHECK(ts_node_is_missing(a) == sq_node_is_missing(b));
  CHECK(ts_node_is_error(a) == sq_node_is_error(b));
  bool block_error = false;
  for (uint32_t neighbor = i; neighbor < nodes->count &&
       nodes->packed[neighbor].slot / SQ_GROUP_SIZE == b.slot / SQ_GROUP_SIZE; neighbor++) {
    block_error |= ts_node_has_error(nodes->mainline[neighbor]);
  }
  for (uint32_t neighbor = i; neighbor > 0 &&
       nodes->packed[neighbor - 1].slot / SQ_GROUP_SIZE == b.slot / SQ_GROUP_SIZE; neighbor--) {
    block_error |= ts_node_has_error(nodes->mainline[neighbor - 1]);
  }
  CHECK(block_error == sq_node_has_error(b));
  CHECK(ts_node_has_changes(a) == sq_node_has_changes(b));
  CHECK(ts_node_descendant_count(a) == sq_node_descendant_count(b));
  CHECK(ts_node_child_count(a) == sq_node_child_count(b));
  CHECK(ts_node_named_child_count(a) == sq_node_named_child_count(b));
  SAME_NODE(ts_node_parent(a), sq_node_parent(b));
  SAME_NODE(ts_node_next_sibling(a), sq_node_next_sibling(b));
  SAME_NODE(ts_node_prev_sibling(a), sq_node_prev_sibling(b));
  SAME_NODE(ts_node_next_named_sibling(a), sq_node_next_named_sibling(b));
  SAME_NODE(ts_node_prev_named_sibling(a), sq_node_prev_named_sibling(b));
  uint32_t children = ts_node_child_count(a);
  for (uint32_t j = 0; j <= children; j++) {
    SAME_NODE(ts_node_child(a, j), sq_node_child(b, j));
    CHECK(strings_equal(ts_node_field_name_for_child(a, j), sq_node_field_name_for_child(b, j)));
  }

  for (uint32_t j = 0; j <= ts_node_named_child_count(a); j++) {
    SAME_NODE(ts_node_named_child(a, j), sq_node_named_child(b, j));
    CHECK(strings_equal(ts_node_field_name_for_named_child(a, j),
                        sq_node_field_name_for_named_child(b, j)));
  }

  const TSLanguage *language = sq_tree_language(b.tree);
  for (uint32_t f = 0; f <= ts_language_field_count(language) + 1; f++) {
    compare_field(nodes, a, b, (TSFieldId)f);
  }

  CHECK(sq_tree_group_has_symbol(b.tree, b.slot / SQ_GROUP_SIZE, sq_node_symbol(b)));
}

static void compare_cursor_state(SQCursor *cursor) {
  SQNode node = sq_cursor_node(cursor);
  SQCursorAttributes actual;
  sq_cursor_attributes(cursor, &actual);
  SQCursorAttributes direct;
  sq_node_attributes(node, &direct);
  CHECK(memcmp(&actual, &direct, sizeof(actual)) == 0);
  CHECK(actual.symbol == sq_node_symbol(node));
  CHECK(actual.grammar_symbol == sq_node_grammar_symbol(node));
  CHECK(strcmp(actual.type, sq_node_type(node)) == 0);
  CHECK(strcmp(actual.grammar_type, sq_node_grammar_type(node)) == 0);
  CHECK(actual.start_byte == sq_node_start_byte(node));
  CHECK(actual.end_byte == sq_node_end_byte(node));
  TSPoint start = sq_node_start_point(node), end = sq_node_end_point(node);
  CHECK(actual.start_point.row == start.row && actual.start_point.column == start.column);
  CHECK(actual.end_point.row == end.row && actual.end_point.column == end.column);
  CHECK(actual.field_id == sq_node_field_id(node));
  CHECK(actual.is_named == sq_node_is_named(node));
  CHECK(actual.is_extra == sq_node_is_extra(node));
  CHECK(actual.is_missing == sq_node_is_missing(node));
  CHECK(actual.is_error == sq_node_is_error(node));
  CHECK(actual.has_error == sq_node_has_error(node));
}

static void compare_supertypes(const TSTreeCursor *cursor, SQNode node) {
  const TreeCursor *raw = (const TreeCursor *)cursor;
  const SQTree *tree = node.tree;
  for (uint32_t s = 0; s < tree->supertype_count; s++) {
    bool expected = false;
    for (uint32_t j = raw->stack.size - 1; j > 0; j--) {
      const TreeCursorEntry *entry = &raw->stack.contents[j - 1];
      TSSymbol alias = 0;
      if (j > 1 && !ts_subtree_extra(*entry->subtree)) {
        alias = ts_language_alias_at(tree->language,
                                     raw->stack.contents[j - 2].subtree->ptr->production_id,
                                     entry->structural_child_index);
      }

      TSSymbol symbol = alias ? alias : ts_subtree_symbol(*entry->subtree);
      expected |= symbol == tree->supertypes[s];
      if (j == 1 || alias || ts_subtree_visible(*entry->subtree)) {
        break;
      }
    }

    CHECK(expected == sq_node_has_supertype(node, tree->supertypes[s]));
  }
}

static void compare_equal_column(const SQTree *tree, uint32_t group,
                                 uint64_t (*equal)(const SQTree *, uint32_t, uint32_t),
                                 uint32_t (*value)(SQNode)) {
  SQNode first = {tree, group * SQ_GROUP_SIZE};
  uint32_t targets[] = {0, value(first), UINT32_MAX};
  for (unsigned target = 0; target < 3; target++) {
    uint64_t expected = 0;
    uint32_t used = SQ_GROUP_SIZE - sq_group_waste(tree, group);
    for (uint32_t lane = 0; lane < used; lane++) {
      SQNode node = {tree, group * SQ_GROUP_SIZE + lane};
      if (value(node) == targets[target]) expected |= UINT64_C(1) << lane;
    }

    CHECK(equal(tree, group, targets[target]) == expected);
  }
}

static void compare_group_equality(const SQTree *tree) {
  for (uint32_t group = 0; group < sq_tree_group_count(tree); group++) {
    compare_equal_column(tree, group, sq_tree_group_span_delta_equal, sq_node_span_delta);
    compare_equal_column(tree, group, sq_tree_group_start_byte_delta_equal,
                         sq_node_start_byte_delta);
    compare_equal_column(tree, group, sq_tree_group_end_byte_delta_equal, sq_node_end_byte_delta);
    compare_equal_column(tree, group, sq_tree_group_start_point_equal, sq_node_start_point_key);
    compare_equal_column(tree, group, sq_tree_group_end_point_equal, sq_node_end_point_key);
    compare_equal_column(tree, group, sq_tree_group_supertype_equal, sq_node_supertype);
    compare_equal_column(tree, group, sq_tree_group_symbol_equal, sq_node_symbol_id);
    compare_equal_column(tree, group, sq_tree_group_grammar_symbol_equal, sq_node_grammar_id);
    compare_equal_column(tree, group, sq_tree_group_field_equal, sq_node_field_value);
  }
}

static void compare_iterator(const Nodes *nodes, SQNode root) {
  uint32_t end = sq_node_first_slot(root);
  SQNodeIterator *iterator = sq_node_iterator_new(root);
  CHECK(iterator && sq_node_is_null(sq_node_iterator_node(iterator)));
  CHECK(sq_node_iterator_field_id(iterator) == 0);
  SQCursor *cursor = sq_cursor_new(root);
  CHECK(cursor);
  uint32_t ordinal = 0;
  while (!sq_node_eq(nodes->packed[ordinal], root)) ordinal++;
  for (; ordinal < nodes->count && nodes->packed[ordinal].slot >= end; ordinal++) {
    SQNode node = sq_node_iterator_next(iterator);
    CHECK(sq_node_eq(node, nodes->packed[ordinal]));
    CHECK(sq_node_eq(node, sq_node_iterator_node(iterator)));
    SQCursorAttributes actual, expected;

    uint32_t start, end;
    CHECK(sq_node_iterator_symbol(iterator) == sq_node_symbol(node));
    sq_node_iterator_byte_range(iterator, &start, &end);
    CHECK(start == sq_node_start_byte(node) && end == sq_node_end_byte(node));
    // Exercise selective reads before full snapshots, then repeat reads.
    CHECK(sq_node_iterator_field_id(iterator) == sq_node_field_id(node));
    sq_node_iterator_attributes(iterator, &actual);
    sq_cursor_attributes(cursor, &expected);
    CHECK(!memcmp(&actual, &expected, sizeof(actual)));
    sq_node_iterator_attributes(iterator, &actual);
    CHECK(!memcmp(&actual, &expected, sizeof(actual)));
    if (!sq_cursor_goto_first_child(cursor)) {
      while (!sq_cursor_goto_next_sibling(cursor) && sq_cursor_goto_parent(cursor)) {
      }
    }
  }

  CHECK(sq_node_is_null(sq_node_iterator_next(iterator)));
  CHECK(sq_node_is_null(sq_node_iterator_next(iterator)));
  CHECK(sq_node_is_null(sq_node_iterator_node(iterator)));
  CHECK(sq_node_iterator_field_id(iterator) == 0);
  SQCursorAttributes empty, actual;
  memset(&empty, 0, sizeof(empty));
  sq_node_iterator_attributes(iterator, &actual);
  CHECK(!memcmp(&actual, &empty, sizeof(actual)));
  sq_cursor_delete(cursor);
  sq_node_iterator_delete(iterator);
}

static void compare_cursor_seeks(Nodes *nodes) {
  TSTreeCursor native = ts_tree_cursor_new(nodes->mainline[0]);
  SQCursor *packed = sq_cursor_new(nodes->packed[0]);
  CHECK(packed);
  for (uint32_t index = 0; index < nodes->count; index += nodes->count / 32 + 1) {
    TSNode parent = nodes->mainline[index];
    SQNode root = nodes->packed[index];
    uint32_t bytes[] = {0, ts_node_start_byte(parent), ts_node_end_byte(parent), UINT32_MAX};
    TSPoint points[] = {{0}, ts_node_start_point(parent), ts_node_end_point(parent), {UINT32_MAX, 0}};
    for (unsigned target = 0; target < 4; target++) {
      for (unsigned by_point = 0; by_point < 2; by_point++) {
        ts_tree_cursor_reset(&native, parent);
        sq_cursor_reset(packed, root);
        CHECK(sq_cursor_depth(packed) == 0);
        CHECK(!sq_cursor_goto_parent(packed));
        CHECK(!sq_cursor_goto_previous_sibling(packed));
        int64_t expected = by_point ? ts_tree_cursor_goto_first_child_for_point(&native, points[target])
                                   : ts_tree_cursor_goto_first_child_for_byte(&native, bytes[target]);
        int64_t actual = by_point ? sq_cursor_goto_first_child_for_point(packed, points[target])
                                 : sq_cursor_goto_first_child_for_byte(packed, bytes[target]);
        CHECK(expected == actual);
        SAME_NODE(ts_tree_cursor_current_node(&native), sq_cursor_node(packed));
        CHECK(ts_tree_cursor_current_depth(&native) == sq_cursor_depth(packed));
      }
    }
    ts_tree_cursor_reset(&native, parent);
    sq_cursor_reset(packed, root);
    CHECK(ts_tree_cursor_goto_last_child(&native) == sq_cursor_goto_last_child(packed));
    do {
      SAME_NODE(ts_tree_cursor_current_node(&native), sq_cursor_node(packed));
      bool moved = ts_tree_cursor_goto_previous_sibling(&native);
      CHECK(moved == sq_cursor_goto_previous_sibling(packed));
      if (!moved) break;
    } while (true);
  }
  ts_tree_cursor_delete(&native);
  sq_cursor_delete(packed);
}

static void compare_tree(const TSTree *tree, const SQTree *packed, bool exhaustive) {
  compare_group_equality(packed);
  uint32_t count = ts_node_descendant_count(ts_tree_root_node(tree));
  Nodes storage = {malloc((size_t)count * sizeof(TSNode)), malloc((size_t)count * sizeof(SQNode)),
                   count};
  Nodes *nodes = &storage;
  CHECK(nodes->mainline && nodes->packed);
  TSTreeCursor cursor = ts_tree_cursor_new(ts_tree_root_node(tree));
  SQNode n = sq_tree_root_node(packed);
  uint32_t i = 0;
  for (;;) {
    CHECK(i < count && !sq_node_is_null(n));
    nodes->mainline[i] = ts_tree_cursor_current_node(&cursor);
    nodes->packed[i++] = n;
    CHECK(ts_tree_cursor_current_field_id(&cursor) == sq_node_field_id(n));
    compare_supertypes(&cursor, n);
    n = sq_node_next_preorder(n);
    if (ts_tree_cursor_goto_first_child(&cursor)) {
      continue;
    }

    bool moved = false;
    do {
      if (ts_tree_cursor_goto_next_sibling(&cursor)) {
        moved = true;
        break;
      }
    } while (ts_tree_cursor_goto_parent(&cursor));
    if (!moved) {
      break;
    }
  }

  CHECK(i == count && sq_node_is_null(n));
  uint32_t flags = 0;
  for (uint32_t index = 0; index < count; index++) {
    TSNode node = nodes->mainline[index];
    if (ts_node_is_extra(node)) flags |= SQ_EXTRAS;
    if (ts_node_is_missing(node)) flags |= SQ_MISSING;
    if (ts_node_has_error(node)) flags |= SQ_ERRORS;
    if (packed->grammar->symbols.separate && ts_node_symbol(node) != ts_node_grammar_symbol(node))
      flags |= SQ_SEPARATE_GRAMMAR;
  }
  CHECK((sq_header_get(packed, format_flags) & SQ_OPTIONAL_FLAGS) == flags);
  ts_tree_cursor_delete(&cursor);
  compare_cursor_seeks(nodes);
  if (exhaustive) {
    for (i = 0; i < count; i++) {
      compare_node(nodes, i);
    }
  } else {
    for (i = 0; i < count; i++) {
      CHECK(ts_node_symbol(nodes->mainline[i]) == sq_node_symbol(nodes->packed[i]));
      CHECK(ts_node_start_byte(nodes->mainline[i]) == sq_node_start_byte(nodes->packed[i]));
      CHECK(ts_node_descendant_count(nodes->mainline[i]) ==
            sq_node_descendant_count(nodes->packed[i]));
    }
  }

  n = nodes->packed[count - 1];
  for (i = count; i > 0; i--) {
    CHECK(sq_node_eq(n, nodes->packed[i - 1]));
    n = sq_node_prev_preorder(n);
  }

  CHECK(sq_node_is_null(n));
  TSNode root = nodes->mainline[0];
  SQNode flat = nodes->packed[0];
  uint32_t bytes = ts_node_end_byte(root);
  uint32_t samples = bytes < 300 ? bytes + 2 : 100;
  for (i = 0; i < samples; i++) {
    uint32_t start = bytes < 300 ? i : (uint32_t)((uint64_t)i * (bytes + 1) / samples);
    SAME_NODE(ts_node_first_child_for_byte(root, start), sq_node_first_child_for_byte(flat, start));
    SAME_NODE(ts_node_first_named_child_for_byte(root, start),
              sq_node_first_named_child_for_byte(flat, start));
  }

  for (i = 0; i < count; i += count / 100 + 1) {
    if (i) {
      SAME_NODE(ts_node_child_with_descendant(root, nodes->mainline[i]),
                sq_node_child_with_descendant(flat, nodes->packed[i]));
    }
  }

  compare_iterator(nodes, flat);

  // Sample interior roots, including leaves and starts inside a physical group.
  for (uint32_t index = 1; index < count; index += count / 8 + 1) {
    compare_iterator(nodes, nodes->packed[index]);
  }

  SQCursor *packed_cursor = sq_cursor_new(flat);
  CHECK(packed_cursor);
  i = 0;
  for (;;) {
    compare_cursor_state(packed_cursor);
    CHECK(sq_node_eq(sq_cursor_node(packed_cursor), nodes->packed[i++]));
    if (sq_cursor_goto_first_child(packed_cursor)) {
      continue;
    }

    bool moved = false;
    do {
      if (sq_cursor_goto_next_sibling(packed_cursor)) {
        moved = true;
        break;
      }
    } while (sq_cursor_goto_parent(packed_cursor));
    if (!moved) {
      break;
    }
  }

  CHECK(i == count && sq_cursor_depth(packed_cursor) == 0);
  CHECK(sq_node_is_null(sq_cursor_parent_node(packed_cursor)));
  sq_cursor_delete(packed_cursor);

  // Rooting at an interior node must not escape to its tree-level siblings.
  SQNode child = sq_node_child(flat, 0);
  if (!sq_node_is_null(child)) {
    packed_cursor = sq_cursor_new(child);
    CHECK(packed_cursor);
    CHECK(!sq_cursor_goto_next_sibling(packed_cursor));
    CHECK(!sq_cursor_goto_parent(packed_cursor));
    uint32_t child_count = sq_node_child_count(child);
    SQNode last = child_count ? sq_node_child(child, child_count - 1) : sq_null();
    CHECK(sq_cursor_goto_last_child(packed_cursor) == !sq_node_is_null(last));
    if (!sq_node_is_null(last)) {
      CHECK(sq_node_eq(sq_cursor_node(packed_cursor), last));
      CHECK(sq_node_eq(sq_cursor_parent_node(packed_cursor), child));
      CHECK(!sq_cursor_goto_next_sibling(packed_cursor));
      compare_cursor_state(packed_cursor);
      CHECK(sq_cursor_goto_parent(packed_cursor));
    }

    CHECK(sq_node_eq(sq_cursor_node(packed_cursor), child));
    sq_cursor_delete(packed_cursor);
  }

  free(nodes->mainline);
  free(nodes->packed);
}

static void reject_index_mutation(const SQTree *tree, uint8_t *bytes, bool presence_only) {
  SQError error;
  CHECK(!sq_tree_from_bytes(tree->grammar, bytes, tree->size, &error) &&
        error == SQ_ERROR_INVALID_SLAB);
  CHECK(!sq_tree_from_bytes_borrowed(tree->grammar, bytes, tree->size, &error) &&
        error == SQ_ERROR_INVALID_SLAB);
  SQTree *safety = sq_tree_from_bytes_safety_checked(tree->grammar, bytes, tree->size, &error);
  if (!presence_only) {
    CHECK(!safety && error == SQ_ERROR_INVALID_SLAB);
    CHECK(!sq_tree_from_bytes_borrowed_safety_checked(tree->grammar, bytes, tree->size, &error) &&
          error == SQ_ERROR_INVALID_SLAB);
    memcpy(bytes, tree->data, tree->size);
    return;
  }
  CHECK(safety && error == SQ_OK);
  SQTree *borrowed =
      sq_tree_from_bytes_borrowed_safety_checked(tree->grammar, bytes, tree->size, &error);
  CHECK(borrowed && error == SQ_OK && borrowed->data == bytes);
  sq_tree_delete(borrowed);
  // Mutated auxiliary values are data, not addresses. Exercise both modes with
  // every valid group/symbol before releasing the owned copy.
  for (uint32_t group = 0; group < sq_tree_group_count(safety); group++) {
    for (uint32_t symbol = 0; symbol < sq_symbols(safety); symbol++) {
      (void)sq_tree_group_has_symbol(safety, group, sq_decode_symbol(safety, symbol));
    }
  }
  sq_tree_delete(safety);
  memcpy(bytes, tree->data, tree->size);
}

static void check_grammar_validation(const SQTree *tree) {
  uint8_t *bytes = sq_allocate_data(tree->size);
  CHECK(bytes);
  memcpy(bytes, tree->data, tree->size);
  SQHeader header = sq_read_header(bytes);
  header.format_flags ^= SQ_SEPARATE_GRAMMAR;
  sq_write_header(bytes, header);
  reject_index_mutation(tree, bytes, false);
  uint32_t root = sq_tree_root_node(tree).slot;
  if (sq_header_get(tree, format_flags) & SQ_SEPARATE_GRAMMAR) {
    if (sq_symbols(tree) <= UINT16_MAX) {
      sq_set_u16(bytes, tree->layout.grammar, root, sq_symbols(tree));
      reject_index_mutation(tree, bytes, false);
    }
  } else {
    uint32_t symbols = sq_symbols(tree), shift = tree->layout.symbol_shift;
    uint32_t invalid = symbols;
    if (((uint64_t)invalid << shift) > UINT16_MAX) {
      for (invalid = 0; invalid < symbols - 2; invalid++) {
        if (tree->language->public_symbol_map[invalid] != invalid) break;
      }
      if (invalid == symbols - 2) {
        free(bytes);
        return;
      }
    }
    sq_set_u16(bytes, tree->layout.symbol, root, invalid << shift);
    reject_index_mutation(tree, bytes, false);
  }
  free(bytes);
}

static void check_presence_validation(const SQTree *tree) {
  uint32_t offset = sq_presence_offset(tree);
  if (!offset) return;
  uint8_t *bytes = sq_allocate_data(tree->size);
  CHECK(bytes);
  memcpy(bytes, tree->data, tree->size);
  uint32_t symbols = sq_symbols(tree);
  uint32_t mode_bytes = (uint32_t)sq_column_size(symbols, 1);
  uint32_t entry_bytes = (sq_tree_group_count(tree) + 31) / 32 * 4;
  bool checked_bitmap = false, checked_occurrence = false, checked_sentinel = false;
  for (uint32_t symbol = 0; symbol < symbols; symbol++) {
    uint8_t *entry = bytes + offset + mode_bytes + (size_t)symbol * entry_bytes;
    if (sq_get_packed(bytes, offset, symbol, 1) && !checked_bitmap) {
      // Toggle both an existing bit and an absent bit. The latter tests the
      // cardinality check, not merely membership of every observed group.
      for (unsigned value = 0; value <= 1; value++) {
        for (uint32_t bit = 0; bit < entry_bytes * 8; bit++) {
          if (((entry[bit / 8] >> (bit % 8)) & 1) == value) {
            entry[bit / 8] ^= (uint8_t)(1u << (bit % 8));
            reject_index_mutation(tree, bytes, true);
            break;
          }
        }
      }

      checked_bitmap = true;
    } else if (!sq_get_packed(bytes, offset, symbol, 1)) {
      // Occurrences and unused sentinels must each match exactly, including
      // the preorder position within this symbol's occurrence list.
      for (uint32_t index = 0; index < entry_bytes / 4; index++) {
        uint32_t slot = sq_get_u32(entry, 0, index);
        bool *checked = slot == SQ_NONE ? &checked_sentinel : &checked_occurrence;
        if (!*checked) {
          entry[(size_t)index * 4] ^= 1;
          reject_index_mutation(tree, bytes, true);
          *checked = true;
        }
      }
    }
  }

  sq_set_packed(bytes, offset, 0, 1, !sq_get_packed(bytes, offset, 0, 1));
  reject_index_mutation(tree, bytes, true);
  if (symbols % 64) {
    sq_set_packed(bytes, offset, symbols, 1, 1);
    reject_index_mutation(tree, bytes, true);
  }

  uint64_t used = mode_bytes + (uint64_t)symbols * entry_bytes;
  if (used < sq_presence_size(tree)) {
    bytes[offset + used] = 1;
    reject_index_mutation(tree, bytes, true);
  }

  free(bytes);
}

static void check_pack_bases(const SQTree *tree) {
  for (uint32_t group = 0; group < sq_tree_group_count(tree); group++) {
    uint32_t count = SQ_GROUP_SIZE - sq_group_waste(tree, group);
    uint32_t span_min = UINT32_MAX, span_max = 0;
    uint32_t column_min = UINT32_MAX, column_max = 0, end_column_max = 0;
    for (uint32_t lane = 0; lane < count; lane++) {
      SQNode node = {tree, group * SQ_GROUP_SIZE + lane};
      uint32_t span = node.slot - sq_node_first_slot(node);
      if (span < span_min) span_min = span;
      if (span > span_max) span_max = span;
      uint32_t column = sq_node_start_point(node).column;
      if (column < column_min) column_min = column;
      if (column > column_max) column_max = column;
      uint32_t end_column = sq_node_end_point(node).column;
      if (end_column > end_column_max) end_column_max = end_column;
    }

    CHECK(sq_group_span_base(tree, group) == (span_max <= UINT8_MAX ? 0 : span_min));
    TSPoint start_base = sq_point_from_key(sq_group_start_point_base(tree, group));
    TSPoint end_base = sq_point_from_key(sq_group_end_point_base(tree, group));
    CHECK(start_base.column == column_min);
    CHECK(end_base.column == end_column_max);
  }
}

static void exercise(const TSLanguage *language, const char *source, uint32_t length,
                     bool exhaustive) {
  TSParser *parser = ts_parser_new();
  CHECK(ts_parser_set_language(parser, language));
  TSTree *tree = ts_parser_parse_string(parser, NULL, source, length);
  CHECK(tree);
  SQError error;
  SQGrammar *grammar = sq_grammar_new(language, &error);
  CHECK(grammar);
  SQPackOptions options = sq_pack_options_default();
  options.initial_group_capacity = 1;
  SQTree *packed = sq_tree_pack(grammar, tree, options, &error);
  if (!packed) {
    fprintf(stderr, "pack: %s\n", sq_error_string(error));
  }

  CHECK(packed && error == SQ_OK);
  CHECK(packed->storage == SQ_STORAGE_COLOCATED);
  CHECK(packed->data == (uint8_t *)packed + sq_runtime_size());
  CHECK(packed->supertypes == grammar->supertypes);
  check_pack_bases(packed);
  compare_tree(tree, packed, exhaustive);
  SQTree *compact = sq_tree_repack(packed, &error);
  CHECK(compact && error == SQ_OK);
  CHECK(compact->storage == SQ_STORAGE_COLOCATED);
  CHECK(compact->data == (uint8_t *)compact + sq_runtime_size());
  if (length <= 4096) {
    check_presence_validation(compact);
    check_grammar_validation(compact);
  }

  // Repacking changes capacity and addresses, but never physical slot IDs.
  SQNode before = sq_tree_root_node(packed), after = sq_tree_root_node(compact);
  while (before.tree) {
    CHECK(after.tree && before.slot == after.slot);
    before = sq_node_next_preorder(before);
    after = sq_node_next_preorder(after);
  }

  CHECK(!after.tree);
  CHECK(sq_tree_group_count(compact) == sq_tree_group_capacity(compact));
  compare_tree(tree, compact, false);
  uint32_t size;
  const void *bytes = sq_tree_data(compact, &size);
  uint8_t *unaligned = malloc((size_t)size + 1);
  CHECK(unaligned);
  CHECK(sq_tree_compact_size(packed) == size);
  CHECK(sq_tree_copy_compact(packed, unaligned + 1, size, &error));
  CHECK(!memcmp(unaligned + 1, bytes, size));
  SQTree *loaded = sq_tree_from_bytes(grammar, unaligned + 1, size, &error);
  CHECK(loaded && loaded->storage == SQ_STORAGE_COLOCATED);
  CHECK(loaded->data == (uint8_t *)loaded + sq_runtime_size());
  CHECK(loaded->data != unaligned + 1);
  compare_tree(tree, loaded, false);

  // Borrow a read-only mapping. Validation and deletion must neither write to
  // nor free the externally owned payload; repacking returns an owned copy.
  size_t page = (size_t)sysconf(_SC_PAGESIZE);
  size_t mapped_size = ((size_t)size + page - 1) / page * page;
  void *mapping =
      mmap(NULL, mapped_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  CHECK(mapping != MAP_FAILED);
  memcpy(mapping, bytes, size);
  CHECK(!mprotect(mapping, mapped_size, PROT_READ));
  SQTree *borrowed = sq_tree_from_bytes_borrowed(grammar, mapping, size, &error);
  CHECK(borrowed && borrowed->storage == SQ_STORAGE_BORROWED && borrowed->data == mapping);
  CHECK(sq_tree_copy_compact(borrowed, unaligned + 1, size, &error));
  CHECK(!memcmp(unaligned + 1, bytes, size));
  compare_tree(tree, borrowed, false);
  SQTree *owned = sq_tree_repack(borrowed, &error);
  CHECK(owned && owned->storage == SQ_STORAGE_COLOCATED);
  sq_tree_delete(borrowed);
  CHECK(!memcmp(mapping, bytes, size));
  CHECK(!munmap(mapping, mapped_size));
  compare_tree(tree, owned, false);
  sq_tree_delete(owned);
  CHECK(!sq_tree_from_bytes_borrowed(grammar, unaligned + 1, size, &error) &&
        error == SQ_ERROR_ARGUMENT);
  CHECK(!sq_tree_from_bytes(grammar, bytes, size - 1, &error) && error == SQ_ERROR_INVALID_SLAB);

  // Reject all earlier format versions even
  // when the rest of this buffer describes a valid current tree.
  for (unsigned version = 1; version <= 5; version++) {
    unaligned[1] = (uint8_t)((((const uint8_t *)bytes)[0] & 0x0f) | (version << 4));
    CHECK(!sq_tree_from_bytes(grammar, unaligned + 1, size, &error) &&
          error == SQ_ERROR_INVALID_SLAB);
    CHECK(!sq_tree_from_bytes_safety_checked(grammar, unaligned + 1, size, &error) &&
          error == SQ_ERROR_INVALID_SLAB);
  }

  // The row/column feature changes column offsets. Reject the other layout
  // before interpreting any of its data, in both directions.
  memcpy(unaligned + 1, bytes, size);
  unaligned[2] ^= 1;
  CHECK(!sq_tree_from_bytes(grammar, unaligned + 1, size, &error) &&
        error == SQ_ERROR_INVALID_SLAB);
  memcpy(unaligned + 1, bytes, size);
  unaligned[1] = ((const uint8_t *)bytes)[0] ^ 0x80;
  CHECK(!sq_tree_from_bytes(grammar, unaligned + 1, size, &error) &&
        error == SQ_ERROR_INVALID_SLAB);

  // Derived section locations still require exact counts and feature flags.
  SQHeader valid_header = sq_read_header(bytes);
  for (unsigned invalid_case = 0; invalid_case < 4; invalid_case++) {
    SQHeader changed = valid_header;
    switch (invalid_case) {
    case 0:
      changed.group_count = 0;
      break;
    case 1:
      changed.group_capacity = UINT32_MAX;
      break;
    case 2:
      changed.supertype_dictionary_count ^= 1;
      break;
    case 3:
      changed.format_flags ^= SQ_PRESENCE;
      break;
    }

    memcpy(unaligned + 1, bytes, size);
    sq_write_header(unaligned + 1, changed);
    CHECK(!sq_tree_from_bytes(grammar, unaligned + 1, size, &error) &&
          error == SQ_ERROR_INVALID_SLAB);
  }

  if (sq_tree_group_count(packed) > 32 && length <= 4096) {
    // Large enough for an index, but explicitly omit it.
    options.symbol_presence = false;
    SQTree *without_index = sq_tree_pack(grammar, tree, options, &error);
    CHECK(without_index && !(sq_header_get(without_index, format_flags) & SQ_PRESENCE));
    SQTree *decoded =
        sq_tree_from_bytes(grammar, without_index->data, without_index->size, &error);
    CHECK(decoded);
    compare_tree(tree, decoded, false);
    sq_tree_delete(decoded);
    decoded =
        sq_tree_from_bytes_borrowed(grammar, without_index->data, without_index->size, &error);
    CHECK(decoded && decoded->data == without_index->data);
    compare_tree(tree, decoded, false);
    sq_tree_delete(decoded);
    sq_tree_delete(without_index);
  }

  if (length < 40) {
    // A changed bit can describe another valid tree. The requirement is safe
    // validation and ownership, not rejection of every possible mutation.
    uint32_t state = 42;
    for (unsigned trial = 0; trial < 64; trial++) {
      memcpy(unaligned + 1, bytes, size);
      state = state * 1664525 + 1013904223;
      unaligned[1 + state % size] ^= (uint8_t)(1u << (trial % 8));
      SQTree *changed = sq_tree_from_bytes(grammar, unaligned + 1, size, &error);
      CHECK(changed ? error == SQ_OK : error == SQ_ERROR_INVALID_SLAB);
      sq_tree_delete(changed);
      changed = sq_tree_from_bytes_safety_checked(grammar, unaligned + 1, size, &error);
      CHECK(changed ? error == SQ_OK : error == SQ_ERROR_INVALID_SLAB);
      if (changed) {
        uint32_t visited = 0;
        for (SQNode node = sq_tree_root_node(changed); node.tree;
             node = sq_node_next_preorder(node)) {
          CHECK(visited++ < sq_tree_slot_count(changed));
          (void)sq_node_parent(node);
          (void)sq_node_symbol(node);
          (void)sq_node_field_name(node);
          (void)sq_node_descendant_count(node);
        }
      }
      sq_tree_delete(changed);
    }

    options.symbol_presence = false;
    options.repack = true;
    SQTree *without_index = sq_tree_pack(grammar, tree, options, &error);
    CHECK(without_index && error == SQ_OK);
    compare_tree(tree, without_index, true);
    SQTree *decoded =
        sq_tree_from_bytes(grammar, without_index->data, without_index->size, &error);
    CHECK(decoded);
    sq_tree_delete(decoded);
    sq_tree_delete(without_index);
  }

  free(unaligned);
  sq_tree_delete(loaded);
  sq_tree_delete(compact);
  sq_tree_delete(packed);
  ts_tree_delete(tree);
  ts_parser_delete(parser);
  sq_grammar_delete(grammar);
}

// Edits can change columns by a different amount than bytes. A single-line
// reverse packer must subtract the two extents independently, even though
// ordinary UTF-8 parsing usually gives them equal widths.
static void edited_positions(const TSLanguage *language) {
  TSParser *parser = ts_parser_new();
  CHECK(ts_parser_set_language(parser, language));
  const char *source = "[1, 2, 3]";
  TSTree *tree = ts_parser_parse_string(parser, NULL, source, (uint32_t)strlen(source));
  CHECK(tree);
  TSInputEdit edit = {.start_byte = 4, .old_end_byte = 4, .new_end_byte = 7,
      .start_point = {0, 4}, .old_end_point = {0, 4}, .new_end_point = {0, 11}};
  ts_tree_edit(tree, &edit);
  SQError error;
  SQGrammar *grammar = sq_grammar_new(language, &error);
  CHECK(grammar);
  SQTree *packed = sq_tree_pack(grammar, tree, sq_pack_options_default(), &error);
  CHECK(packed && error == SQ_OK);
  SQPackContext *context = sq_pack_context_new(&error);
  CHECK(context);
  SQTree *cached = sq_pack_context_pack(context, grammar, tree, sq_pack_options_default(), &error);
  CHECK(cached && cached->size == packed->size);
  CHECK(memcmp(cached->data, packed->data, packed->size) == 0);
  TSTreeCursor cursor = ts_tree_cursor_new(ts_tree_root_node(tree));
  SQNode node = sq_tree_root_node(packed);
  for (;;) {
    TSNode expected = ts_tree_cursor_current_node(&cursor);
    CHECK(!sq_node_is_null(node));
    CHECK(ts_node_start_byte(expected) == sq_node_start_byte(node));
    CHECK(ts_node_end_byte(expected) == sq_node_end_byte(node));
    CHECK(points_equal(ts_node_start_point(expected), sq_node_start_point(node)));
    CHECK(points_equal(ts_node_end_point(expected), sq_node_end_point(node)));
    node = sq_node_next_preorder(node);
    if (ts_tree_cursor_goto_first_child(&cursor)) continue;
    bool moved = false;
    do {
      if (ts_tree_cursor_goto_next_sibling(&cursor)) { moved = true; break; }
    } while (ts_tree_cursor_goto_parent(&cursor));
    if (!moved) break;
  }
  CHECK(sq_node_is_null(node));
  ts_tree_cursor_delete(&cursor);
  sq_tree_delete(cached);
  sq_tree_delete(packed);
  sq_pack_context_delete(context);
  ts_tree_delete(tree);
  ts_parser_delete(parser);
  sq_grammar_delete(grammar);
}

static void omitted_points(const TSLanguage *language) {
  TSParser *parser = ts_parser_new();
  CHECK(ts_parser_set_language(parser, language));
  const char *source = "[\n  1,\n  2\n]";
  TSTree *tree = ts_parser_parse_string(parser, NULL, source, (uint32_t)strlen(source));
  CHECK(tree);
  SQPackOptions options = sq_pack_options_default();
  options.points = false;
  SQError error;
  SQGrammar *grammar = sq_grammar_new(language, &error);
  CHECK(grammar);
  SQTree *packed = sq_tree_pack(grammar, tree, options, &error);
  CHECK(packed && error == SQ_OK && !sq_tree_has_points(packed));
  CHECK(packed->layout.start_point_base == packed->layout.extra);
  CHECK(packed->layout.start_point == packed->layout.extra);
  CHECK(packed->layout.end_point_base == packed->layout.extra);
  CHECK(packed->layout.end_point == packed->layout.extra);

  for (SQNode node = sq_tree_root_node(packed); node.tree; node = sq_node_next_preorder(node)) {
    TSPoint start = sq_node_start_point(node), end = sq_node_end_point(node);
    CHECK(start.row == 0 && start.column == sq_node_start_byte(node));
    CHECK(end.row == 0 && end.column == sq_node_end_byte(node));
    SQCursorAttributes attributes;
    sq_node_attributes(node, &attributes);
    CHECK(points_equal(attributes.start_point, start));
    CHECK(points_equal(attributes.end_point, end));
  }

  SQNode root = sq_tree_root_node(packed);
  SQNode by_byte = sq_node_descendant_for_byte_range(root, 3, 4);
  SQNode by_point = sq_node_descendant_for_point_range(root, (TSPoint){0, 3}, (TSPoint){0, 4});
  CHECK(sq_node_eq(by_byte, by_point));
  CHECK(!sq_tree_group_start_point_equal(packed, 0, 0));
  CHECK(!sq_tree_group_end_point_equal(packed, 0, 0));

  SQNodeIterator *iterator = sq_node_iterator_new(root);
  CHECK(iterator);
  for (SQNode node = sq_node_iterator_next(iterator); node.tree;
       node = sq_node_iterator_next(iterator)) {
    SQCursorAttributes attributes;
    sq_node_iterator_attributes(iterator, &attributes);
    CHECK(attributes.start_point.row == 0 && attributes.start_point.column == attributes.start_byte);
    CHECK(attributes.end_point.row == 0 && attributes.end_point.column == attributes.end_byte);
  }
  sq_node_iterator_delete(iterator);

  SQTree *compact = sq_tree_repack(packed, &error);
  CHECK(compact && !sq_tree_has_points(compact));
  uint32_t size;
  const void *bytes = sq_tree_data(compact, &size);
  SQTree *loaded = sq_tree_from_bytes(grammar, bytes, size, &error);
  CHECK(loaded && !sq_tree_has_points(loaded));
  CHECK(sq_node_start_point(sq_tree_root_node(loaded)).row == 0);
  CHECK(sq_node_end_point(sq_tree_root_node(loaded)).column == strlen(source));

  sq_tree_delete(loaded);
  sq_tree_delete(compact);
  sq_tree_delete(packed);
  ts_tree_delete(tree);
  ts_parser_delete(parser);
  sq_grammar_delete(grammar);
}

static void packing_tests(void) {
  CHECK(!sq_node_iterator_new(sq_null()));
  CHECK(sq_node_is_null(sq_node_iterator_next(NULL)));
  CHECK(sq_node_is_null(sq_node_iterator_node(NULL)));
  CHECK(sq_node_iterator_field_id(NULL) == 0);
  sq_node_iterator_delete(NULL);
  SQCursorAttributes empty = {0}, actual;
  memset(&actual, 0xff, sizeof(actual));
  sq_node_attributes(sq_null(), &actual);
  CHECK(memcmp(&empty, &actual, sizeof(actual)) == 0);
  sq_node_attributes(sq_null(), NULL);
  uint8_t data[256];
  for (uint8_t bits = 1; bits <= 32; bits++) {
    memset(data, 0, sizeof(data));
    uint32_t count = (uint32_t)(sizeof(data) / 8) * (64 / bits);
    for (uint32_t i = 0; i < count; i++) {
      sq_set_packed(data, 0, i, bits,
                    (uint32_t)((i * UINT64_C(7919)) & ((UINT64_C(1) << bits) - 1)));
    }

    for (uint32_t i = 0; i < count; i++) {
      CHECK(sq_get_packed(data, 0, i, bits) ==
            ((i * UINT64_C(7919)) & ((UINT64_C(1) << bits) - 1)));
    }
  }

  CHECK(sq_width(0) == 0 && sq_width(3) == 2 && sq_width(4) == 3 && sq_width(255) == 8 &&
        sq_width(256) == 9);
}

int main(int argc, char **argv) {
  input_name = "packing";
  packing_tests();
  if (argc < 3) {
    fprintf(stderr, "usage: compare LIBRARY SYMBOL [SOURCE...]\n");
    return 2;
  }

  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) {
    fprintf(stderr, "%s\n", dlerror());
    return 2;
  }

  const TSLanguage *(*language_fn)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  CHECK(language_fn);
  const TSLanguage *language = language_fn();
  TSLanguage synthetic = test_clone_language(language);
  TSSymbolMetadata *metadata = NULL;
  if (getenv("SQ_TEST_SUPERTYPES")) {
    uint32_t symbols = language->symbol_count + language->alias_count;
    metadata = malloc(symbols * sizeof(*metadata));
    CHECK(metadata);
    memcpy(metadata, language->symbol_metadata, symbols * sizeof(*metadata));
    unsigned remaining = (unsigned)atoi(getenv("SQ_TEST_SUPERTYPES"));
    for (uint32_t i = 0; i < symbols; i++) {
      metadata[i].supertype = i >= language->token_count && !metadata[i].visible && remaining;
      if (metadata[i].supertype) remaining--;
    }
    CHECK(!remaining);
    synthetic.symbol_metadata = metadata;
    language = &synthetic;
  }
  input_name = "edited positions";
  edited_positions(language);
  input_name = "omitted points";
  omitted_points(language);
  const char *samples[] = {"",
                           "x",
                           "{\"a\": [1, true, null], \"b\": {\"c\": 2}}",
                           "{\"a\": [1,",
                           "// comment\nfunction f(a) { return a + 1; }",
                           "\n\n\n\t\"héllo🌲\"\r\n",
                           "[[[[[[[[[[]]]]]]]]]]",
                           "\""};
  for (size_t i = 0; !getenv("SQ_SKIP_EDGE_CASES") && i < sizeof(samples) / sizeof(samples[0]);
       i++) {
    input_name = samples[i];
    exercise(language, samples[i], (uint32_t)strlen(samples[i]), true);
  }

  if (!getenv("SQ_SKIP_EDGE_CASES")) {
    // Several physical groups, changing group bases, and a partial final group.
    // Other grammars also exercise the iterator over their error recovery trees.
    char source[4096];
    size_t length = 0;
    source[length++] = '[';
    for (unsigned index = 0; index < 129; index++) {
      int written =
          snprintf(source + length, sizeof(source) - length, "%s{\"key\":[%u,%u],\"value\":true}",
                   index ? "," : "", index, index + 1);
      CHECK(written > 0 && (size_t)written < sizeof(source) - length);
      length += (size_t)written;
    }

    source[length++] = ']';
    input_name = "iterator group boundaries";
    exercise(language, source, (uint32_t)length, false);
  }

  for (int i = 3; i < argc; i++) {
    input_name = argv[i];
    FILE *file = fopen(argv[i], "rb");
    CHECK(file);
    CHECK(!fseek(file, 0, SEEK_END));
    long length = ftell(file);
    CHECK(length >= 0 && length < 16 * 1024 * 1024);
    rewind(file);
    char *source = malloc((size_t)length + 1);
    CHECK(source);
    CHECK(fread(source, 1, (size_t)length, file) == (size_t)length);
    fclose(file);
    exercise(language, source, (uint32_t)length, length < 20000);

    // Reproducible destructive edits, parsed fresh each time.
    uint32_t state = 42;
    for (unsigned trial = 0; trial < 8; trial++) {
      state = state * 1664525 + 1013904223;
      if (length) {
        source[state % (uint32_t)length] = trial & 1 ? '\n' : '}';
      }

      exercise(language, source, (uint32_t)length, length < 3000);
    }

    free(source);
  }

  printf("ok: %s (%d files plus edge cases)\n", argv[2], argc - 3);
  free(metadata);
  dlclose(library);
  printf("expected field mismatches: %u\n", expected_field_mismatches);
  return 0;
}
