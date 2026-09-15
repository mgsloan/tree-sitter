// Write reference slabs, or require byte-for-byte equality with another build.
// Only public APIs are used so the same probe can link against either layout.
#include <tree_sitter/squat.h>
#include <assert.h>
#ifndef SQ_TEST_LANGUAGE
#include <dlfcn.h>
#else
extern const TSLanguage *SQ_TEST_LANGUAGE(void);
#endif
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint8_t *read_file(const char *path, uint32_t *size) {
  FILE *file = fopen(path, "rb");
  assert(file && !fseek(file, 0, SEEK_END));
  long length = ftell(file);
  assert(length >= 0 && (unsigned long)length <= UINT32_MAX);
  rewind(file);
  *size = (uint32_t)length;
  uint8_t *bytes = aligned_alloc(64, ((size_t)length + 64) & ~(size_t)63);
  assert(bytes && fread(bytes, 1, *size, file) == *size);
  fclose(file);
  return bytes;
}

static void compare_trees(const SQTree *expected, const SQTree *actual) {
  assert(sq_tree_slot_count(expected) == sq_tree_slot_count(actual));
  assert(sq_tree_has_points(expected) == sq_tree_has_points(actual));
  for (uint32_t slot = 0; slot < sq_tree_slot_count(expected); slot++) {
    SQNode left = sq_tree_node_at_slot(expected, slot);
    SQNode right = sq_tree_node_at_slot(actual, slot);
    assert(sq_node_is_null(left) == sq_node_is_null(right));
    if (sq_node_is_null(left)) continue;
    SQCursorAttributes first, second;
    sq_node_attributes(left, &first);
    sq_node_attributes(right, &second);
#define CHECK(member) assert(first.member == second.member)
    CHECK(start_byte); CHECK(end_byte);
    CHECK(start_point.row); CHECK(start_point.column);
    CHECK(end_point.row); CHECK(end_point.column);
    CHECK(symbol); CHECK(grammar_symbol); CHECK(field_id);
    CHECK(is_named); CHECK(is_extra); CHECK(is_missing); CHECK(is_error); CHECK(has_error);
#undef CHECK
    assert(!strcmp(first.type, second.type));
    assert(!strcmp(first.grammar_type, second.grammar_type));
    assert(sq_node_child_count(left) == sq_node_child_count(right));
    assert(sq_node_named_child_count(left) == sq_node_named_child_count(right));
    assert(sq_node_descendant_count(left) == sq_node_descendant_count(right));
    assert(sq_node_end_slot(left) == sq_node_end_slot(right));
    SQNode left_parent = sq_node_parent(left), right_parent = sq_node_parent(right);
    assert(sq_node_is_null(left_parent) == sq_node_is_null(right_parent));
    if (!sq_node_is_null(left_parent)) assert(left_parent.slot == right_parent.slot);
  }
}

int main(int argc, char **argv) {
  assert(argc == 5 || argc == 6);
#ifdef SQ_EXPECT_BIG_ENDIAN
  const uint16_t endian = 1;
  assert((*(const uint8_t *)&endian == 0) == SQ_EXPECT_BIG_ENDIAN);
#endif
#ifdef SQ_TEST_LANGUAGE
  const TSLanguage *language = SQ_TEST_LANGUAGE();
#else
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  assert(library);
  const TSLanguage *(*language_fn)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  assert(language_fn);
  const TSLanguage *language = language_fn();
#endif
  SQError grammar_error;
  SQGrammar *grammar = sq_grammar_new(language, &grammar_error);
  if (!grammar) return 1;
  uint32_t source_size;
  uint8_t *source = read_file(argv[3], &source_size);
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language));
  TSTree *parsed = ts_parser_parse_string(parser, NULL, (const char *)source, source_size);
  assert(parsed);
  for (unsigned variant = 0; variant < 16; variant++) {
    SQPackOptions options = sq_pack_options_default();
    options.initial_group_capacity = variant & 1;
    options.repack = (variant & 2) != 0;
    options.points = (variant & 4) == 0;
    options.symbol_presence = (variant & 8) == 0;
    SQError error;
    SQTree *tree = sq_tree_pack(grammar, parsed, options, &error);
    assert(tree && error == SQ_OK);
    uint32_t size;
    const void *bytes = sq_tree_data(tree, &size);
    char path[4096];
    int length = snprintf(path, sizeof(path), "%s-%u.slab", argv[4], variant);
    assert(length > 0 && (size_t)length < sizeof(path));
    FILE *file = fopen(path, "wb");
    assert(file && fwrite(bytes, 1, size, file) == size);
    fclose(file);
    if (argc == 6) {
      length = snprintf(path, sizeof(path), "%s-%u.slab", argv[5], variant);
      assert(length > 0 && (size_t)length < sizeof(path));
      uint32_t reference_size;
      uint8_t *reference = read_file(path, &reference_size);
      assert(size == reference_size && !memcmp(bytes, reference, size));
      SQTree *copy = sq_tree_from_bytes(grammar, reference, size, &error);
      assert(copy && error == SQ_OK);
      SQTree *borrowed = sq_tree_from_bytes_borrowed(grammar, reference, size, &error);
      assert(borrowed && error == SQ_OK && sq_tree_data(borrowed, NULL) == reference);
      compare_trees(tree, copy);
      compare_trees(tree, borrowed);
      sq_tree_delete(copy);
      sq_tree_delete(borrowed);
      free(reference);
    }

    sq_tree_delete(tree);
  }

  ts_tree_delete(parsed);
  ts_parser_delete(parser);
  free(source);
  sq_grammar_delete(grammar);
#ifndef SQ_TEST_LANGUAGE
  dlclose(library);
#endif
  return 0;
}
