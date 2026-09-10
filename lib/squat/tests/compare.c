#define _DEFAULT_SOURCE
#define _POSIX_C_SOURCE 200809L
#include <tree_sitter/squat.h>
#include "../internal.h"
#include "field_lookup.h"
#include "../../src/tree_cursor.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static const char *input_name;
static uint32_t current_ordinal;
static uint32_t seek_mismatches;
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
#if SQ_INCLUDE_POINTS
static bool points_equal(TSPoint a, TSPoint b) {
  return a.row == b.row && a.column == b.column;
}
#endif
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
                sq_node_last_flag(nodes->packed[dump]));                                         \
      }                                                                                            \
    }                                                                                              \
    CHECK(ma == pa);                                                                               \
  } while (0)
static void compare_seek(Nodes *nodes, TSNode expected, SQNode actual, const char *operation) {
  uint32_t mainline = ordinal_mainline(nodes, expected);
  uint32_t packed = ordinal_packed(nodes, actual);
  if (mainline != packed) {
    if (!seek_mismatches) {
      fprintf(stderr, "%s: %s differs: mainline ordinal %u, squat ordinal %u\n", input_name,
              operation, mainline, packed);
    }
    seek_mismatches++;
  }
}
#define SAME_SEEK(main, squat) compare_seek(nodes, (main), (squat), #main)

static void compare_field(Nodes *nodes, TSNode parent, SQNode packed_parent, TSFieldId field) {
  TSNode expected = ts_node_child_by_field_id(parent, field);
  SQNode actual = sq_node_child_by_field_id(packed_parent, field);
  if (ordinal_mainline(nodes, expected) == ordinal_packed(nodes, actual)) {
    return;
  }
  TSNode ordinary = visible_child_by_field(parent, field);
  SAME_NODE(ordinary, actual);
  if (!expected_field_mismatches) {
    fprintf(stderr, "%s: expected field mismatch at %s [%u,%u), field %u: "
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
  CHECK(ts_node_grammar_symbol(a) == sq_node_grammar_symbol(b));
  CHECK(strings_equal(ts_node_type(a), sq_node_type(b)));
  CHECK(strings_equal(ts_node_grammar_type(a), sq_node_grammar_type(b)));
  CHECK(ts_node_start_byte(a) == sq_node_start_byte(b));
  CHECK(ts_node_end_byte(a) == sq_node_end_byte(b));
#if SQ_INCLUDE_POINTS
  CHECK(points_equal(ts_node_start_point(a), sq_node_start_point(b)));
  CHECK(points_equal(ts_node_end_point(a), sq_node_end_point(b)));
#endif
  CHECK(ts_node_is_named(a) == sq_node_is_named(b));
  CHECK(ts_node_is_extra(a) == sq_node_is_extra(b));
  CHECK(ts_node_is_missing(a) == sq_node_is_missing(b));
  CHECK(ts_node_is_error(a) == sq_node_is_error(b));
  CHECK(ts_node_has_error(a) == sq_node_has_error(b));
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
  CHECK(actual.symbol == sq_node_symbol(node));
  CHECK(actual.grammar_symbol == sq_node_grammar_symbol(node));
  CHECK(strcmp(actual.type, sq_node_type(node)) == 0);
  CHECK(strcmp(actual.grammar_type, sq_node_grammar_type(node)) == 0);
  CHECK(actual.start_byte == sq_node_start_byte(node));
  CHECK(actual.end_byte == sq_node_end_byte(node));
#if SQ_INCLUDE_POINTS
  TSPoint start = sq_node_start_point(node), end = sq_node_end_point(node);
  CHECK(actual.start_point.row == start.row && actual.start_point.column == start.column);
  CHECK(actual.end_point.row == end.row && actual.end_point.column == end.column);
#endif
  CHECK(actual.field_id == sq_node_field_id(node));
  CHECK(actual.is_named == sq_node_is_named(node));
  CHECK(actual.is_extra == sq_node_is_extra(node));
  CHECK(actual.is_missing == sq_node_is_missing(node));
  CHECK(actual.is_error == sq_node_is_error(node));
  CHECK(actual.has_error == sq_node_has_error(node));
  CHECK(actual.child_count == sq_node_child_count(node));
  CHECK(actual.named_child_count == sq_node_named_child_count(node));
  CHECK(actual.descendant_count == sq_node_descendant_count(node));
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
    compare_equal_column(tree, group, sq_tree_group_start_byte_delta_equal, sq_node_start_byte_delta);
    compare_equal_column(tree, group, sq_tree_group_end_byte_delta_equal, sq_node_end_byte_delta);
#if SQ_INCLUDE_POINTS
    compare_equal_column(tree, group, sq_tree_group_start_row_delta_equal, sq_node_start_row_delta);
    compare_equal_column(tree, group, sq_tree_group_end_row_delta_equal, sq_node_end_row_delta);
    compare_equal_column(tree, group, sq_tree_group_start_column_delta_equal, sq_node_start_column_delta);
    compare_equal_column(tree, group, sq_tree_group_end_column_delta_equal, sq_node_end_column_delta);
#endif
    compare_equal_column(tree, group, sq_tree_group_supertype_equal, sq_node_supertype);
    compare_equal_column(tree, group, sq_tree_group_symbol_equal, sq_node_symbol_id);
    compare_equal_column(tree, group, sq_tree_group_grammar_symbol_equal, sq_node_grammar_id);
    compare_equal_column(tree, group, sq_tree_group_field_equal, sq_node_field_value);
  }
}

static void compare_iterator(const Nodes *nodes, SQNode root) {
  uint32_t end = sq_node_first_slot(root);
  for (unsigned cached = 0; cached < 2; cached++) {
    SQNodeIterator *iterator = sq_node_iterator_new(root, cached);
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
      // Exercise lazy field-only fills before full snapshots, then repeat reads.
      CHECK(sq_node_iterator_field_id(iterator) == sq_node_field_id(node));
      sq_node_iterator_attributes(iterator, &actual);
      sq_cursor_attributes(cursor, &expected);
      CHECK(!memcmp(&actual, &expected, sizeof(actual)));
      sq_node_iterator_attributes(iterator, &actual);
      CHECK(!memcmp(&actual, &expected, sizeof(actual)));
      if (!sq_cursor_goto_first_child(cursor)) {
        while (!sq_cursor_goto_next_sibling(cursor) && sq_cursor_goto_parent(cursor)) {}
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
  ts_tree_cursor_delete(&cursor);
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
    for (uint32_t width = 0; width <= 1; width++) {
      SAME_SEEK(ts_node_descendant_for_byte_range(root, start, start + width),
                sq_node_descendant_for_byte_range(flat, start, start + width));
      SAME_SEEK(ts_node_named_descendant_for_byte_range(root, start, start + width),
                sq_node_named_descendant_for_byte_range(flat, start, start + width));
    }
    SAME_SEEK(ts_node_first_child_for_byte(root, start), sq_node_first_child_for_byte(flat, start));
    SAME_SEEK(ts_node_first_named_child_for_byte(root, start),
              sq_node_first_named_child_for_byte(flat, start));
  }
  for (i = 0; i < count; i += count / 100 + 1) {
#if SQ_INCLUDE_POINTS
    TSPoint start = ts_node_start_point(nodes->mainline[i]),
            end = ts_node_end_point(nodes->mainline[i]);
    SAME_SEEK(ts_node_descendant_for_point_range(root, start, start),
              sq_node_descendant_for_point_range(flat, start, start));
    SAME_SEEK(ts_node_named_descendant_for_point_range(root, start, end),
              sq_node_named_descendant_for_point_range(flat, start, end));
#endif
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
static void reject_index_mutation(const SQTree *tree, uint8_t *bytes) {
  SQError error;
  CHECK(!sq_tree_from_bytes(tree->language, bytes, tree->size, &error) &&
        error == SQ_ERROR_INVALID_SLAB);
  CHECK(!sq_tree_from_bytes_borrowed(tree->language, bytes, tree->size, &error) &&
        error == SQ_ERROR_INVALID_SLAB);
  memcpy(bytes, tree->data, tree->size);
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
            reject_index_mutation(tree, bytes);
            break;
          }
        }
      }
      checked_bitmap = true;
    } else if (!sq_get_packed(bytes, offset, symbol, 1)) {
      // Occurrences and unused sentinels must each match exactly, including
      // the preorder position within this symbol's occurrence list.
      for (uint32_t index = 0; index < entry_bytes / 4; index++) {
        uint32_t slot;
        memcpy(&slot, entry + (size_t)index * 4, 4);
        bool *checked = slot == SQ_NONE ? &checked_sentinel : &checked_occurrence;
        if (!*checked) {
          entry[(size_t)index * 4] ^= 1;
          reject_index_mutation(tree, bytes);
          *checked = true;
        }
      }
    }
  }
  sq_set_packed(bytes, offset, 0, 1, !sq_get_packed(bytes, offset, 0, 1));
  reject_index_mutation(tree, bytes);
  if (symbols % 64) {
    sq_set_packed(bytes, offset, symbols, 1, 1);
    reject_index_mutation(tree, bytes);
  }
  uint64_t used = mode_bytes + (uint64_t)symbols * entry_bytes;
  if (used < sq_presence_size(tree)) {
    bytes[offset + used] = 1;
    reject_index_mutation(tree, bytes);
  }
  free(bytes);
}

static void exercise(const TSLanguage *language, const char *source, uint32_t length,
                     bool exhaustive) {
  TSParser *parser = ts_parser_new();
  CHECK(ts_parser_set_language(parser, language));
  TSTree *tree = ts_parser_parse_string(parser, NULL, source, length);
  CHECK(tree);
  SQError error;
  SQPackOptions options = sq_pack_options_default();
  options.initial_group_capacity = 1;
  SQTree *packed = sq_tree_pack(tree, options, &error);
  if (!packed) {
    fprintf(stderr, "pack: %s\n", sq_error_string(error));
  }
  CHECK(packed && error == SQ_OK);
  CHECK(packed->storage == SQ_STORAGE_COLOCATED);
  CHECK(packed->data == (uint8_t *)packed + sq_runtime_size(language));
  CHECK(packed->supertypes == (TSSymbol *)(packed + 1));
  compare_tree(tree, packed, exhaustive);
  SQTree *compact = sq_tree_repack(packed, &error);
  CHECK(compact && error == SQ_OK);
  CHECK(compact->storage == SQ_STORAGE_COLOCATED);
  CHECK(compact->data == (uint8_t *)compact + sq_runtime_size(language));
  if (length <= 4096) check_presence_validation(compact);
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
  memcpy(unaligned + 1, bytes, size);
  SQTree *loaded = sq_tree_from_bytes(language, unaligned + 1, size, &error);
  CHECK(loaded && loaded->storage == SQ_STORAGE_COPIED);
  CHECK(loaded->data != unaligned + 1);
  compare_tree(tree, loaded, false);

  // Borrow a read-only mapping. Validation and deletion must neither write to
  // nor free the externally owned payload; repacking returns an owned copy.
  size_t page = (size_t)sysconf(_SC_PAGESIZE);
  size_t mapped_size = ((size_t)size + page - 1) / page * page;
  void *mapping = mmap(NULL, mapped_size, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  CHECK(mapping != MAP_FAILED);
  memcpy(mapping, bytes, size);
  CHECK(!mprotect(mapping, mapped_size, PROT_READ));
  SQTree *borrowed = sq_tree_from_bytes_borrowed(language, mapping, size, &error);
  CHECK(borrowed && borrowed->storage == SQ_STORAGE_BORROWED && borrowed->data == mapping);
  compare_tree(tree, borrowed, false);
  SQTree *owned = sq_tree_repack(borrowed, &error);
  CHECK(owned && owned->storage == SQ_STORAGE_COLOCATED);
  sq_tree_delete(borrowed);
  CHECK(!memcmp(mapping, bytes, size));
  CHECK(!munmap(mapping, mapped_size));
  compare_tree(tree, owned, false);
  sq_tree_delete(owned);
  CHECK(!sq_tree_from_bytes_borrowed(language, unaligned + 1, size, &error) &&
        error == SQ_ERROR_ARGUMENT);
  CHECK(!sq_tree_from_bytes(language, bytes, size - 1, &error) && error == SQ_ERROR_INVALID_SLAB);
  // Reject all three earlier format versions even
  // when the rest of this buffer describes a valid current tree.
  for (unsigned version = 1; version <= 3; version++) {
    unaligned[1] = (uint8_t)((((const uint8_t *)bytes)[0] & 0x0f) | (version << 4));
    CHECK(!sq_tree_from_bytes(language, unaligned + 1, size, &error) &&
          error == SQ_ERROR_INVALID_SLAB);
  }
  // The row/column feature changes column offsets. Reject the other layout
  // before interpreting any of its data, in both directions.
  memcpy(unaligned + 1, bytes, size);
  unaligned[2] ^= 1;
  CHECK(!sq_tree_from_bytes(language, unaligned + 1, size, &error) &&
        error == SQ_ERROR_INVALID_SLAB);
  memcpy(unaligned + 1, bytes, size);
  unaligned[1] = ((const uint8_t *)bytes)[0] ^ 0x80;
  CHECK(!sq_tree_from_bytes(language, unaligned + 1, size, &error) &&
        error == SQ_ERROR_INVALID_SLAB);
  // Derived section locations still require exact counts and feature flags.
  SQHeader valid_header;
  memcpy(&valid_header, bytes, sizeof(valid_header));
  for (unsigned invalid_case = 0; invalid_case < 4; invalid_case++) {
    SQHeader changed = valid_header;
    switch (invalid_case) {
    case 0: changed.group_count = 0; break;
    case 1: changed.group_capacity = UINT32_MAX; break;
    case 2: changed.supertype_dictionary_count = 257; break;
    case 3: changed.format_flags ^= SQ_PRESENCE; break;
    }
    memcpy(unaligned + 1, bytes, size);
    memcpy(unaligned + 1, &changed, sizeof(changed));
    CHECK(!sq_tree_from_bytes(language, unaligned + 1, size, &error) &&
          error == SQ_ERROR_INVALID_SLAB);
  }
  if (sq_tree_group_count(packed) > 32 && length <= 4096) {
    // Large enough for an index, but explicitly omit it. The dictionary (when
    // required by the grammar) must immediately follow the ordinary columns.
    options.symbol_presence = false;
    SQTree *without_index = sq_tree_pack(tree, options, &error);
    CHECK(without_index && !(sq_header(without_index)->format_flags & SQ_PRESENCE));
    SQTree *decoded = sq_tree_from_bytes(language, without_index->data, without_index->size, &error);
    CHECK(decoded);
    compare_tree(tree, decoded, false);
    sq_tree_delete(decoded);
    decoded = sq_tree_from_bytes_borrowed(language, without_index->data, without_index->size, &error);
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
      SQTree *changed = sq_tree_from_bytes(language, unaligned + 1, size, &error);
      CHECK(changed ? error == SQ_OK : error == SQ_ERROR_INVALID_SLAB);
      sq_tree_delete(changed);
    }
    options.symbol_presence = false;
    options.repack = true;
    SQTree *without_index = sq_tree_pack(tree, options, &error);
    CHECK(without_index && error == SQ_OK);
    compare_tree(tree, without_index, true);
    SQTree *decoded = sq_tree_from_bytes(language, without_index->data, without_index->size, &error);
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
}
static void packing_tests(void) {
  CHECK(!sq_node_iterator_new(sq_null(), false));
  CHECK(!sq_node_iterator_new(sq_null(), true));
  CHECK(sq_node_is_null(sq_node_iterator_next(NULL)));
  CHECK(sq_node_is_null(sq_node_iterator_node(NULL)));
  CHECK(sq_node_iterator_field_id(NULL) == 0);
  sq_node_iterator_delete(NULL);
  uint8_t data[256];
  for (uint8_t bits = 1; bits <= 32; bits++) {
    memset(data, 0, sizeof(data));
    uint32_t count = (uint32_t)(sizeof(data) / 8) * (64 / bits);
    for (uint32_t i = 0; i < count; i++) {
      sq_set_packed(data, 0, i, bits, (uint32_t)((i * UINT64_C(7919)) & ((UINT64_C(1) << bits) - 1)));
    }
    for (uint32_t i = 0; i < count; i++) {
      CHECK(sq_get_packed(data, 0, i, bits) == ((i * UINT64_C(7919)) & ((UINT64_C(1) << bits) - 1)));
    }
  }
  CHECK(sq_width(0) == 2 && sq_width(3) == 2 && sq_width(4) == 3 && sq_width(255) == 8 &&
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
    // Several unpack windows, changing group bases, and a partial final window.
    // Other grammars also exercise the iterator over their error recovery trees.
    char source[4096];
    size_t length = 0;
    source[length++] = '[';
    for (unsigned index = 0; index < 129; index++) {
      int written = snprintf(source + length, sizeof(source) - length,
                             "%s{\"key\":[%u,%u],\"value\":true}",
                             index ? "," : "", index, index + 1);
      CHECK(written > 0 && (size_t)written < sizeof(source) - length);
      length += (size_t)written;
    }
    source[length++] = ']';
    input_name = "iterator unpack windows";
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
    /* Reproducible destructive edits, parsed fresh each time. */
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
  dlclose(library);
  printf("seek mismatches: %u\n", seek_mismatches);
  printf("expected field mismatches: %u\n", expected_field_mismatches);
  return seek_mismatches && getenv("SQ_STRICT_SEEKS") ? 1 : 0;
}
