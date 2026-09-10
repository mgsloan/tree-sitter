/* Write reference slabs, or require byte-for-byte equality with another build.
 * Only public APIs are used so the same probe can link against either layout. */
#include <tree_sitter/squat.h>
#include <assert.h>
#include <dlfcn.h>
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

int main(int argc, char **argv) {
  assert(argc == 5 || argc == 6);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  assert(library);
  const TSLanguage *(*language_fn)(void) =
      (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  assert(language_fn);
  const TSLanguage *language = language_fn();
  uint32_t source_size;
  uint8_t *source = read_file(argv[3], &source_size);
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language));
  TSTree *parsed = ts_parser_parse_string(parser, NULL, (const char *)source, source_size);
  assert(parsed);
  for (unsigned variant = 0; variant < 4; variant++) {
    SQPackOptions options = sq_pack_options_default();
    options.initial_group_capacity = variant & 1;
    options.repack = (variant & 2) != 0;
    SQError error;
    SQTree *tree = sq_tree_pack(parsed, options, &error);
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
      SQTree *copy = sq_tree_from_bytes(language, reference, size, &error);
      assert(copy && error == SQ_OK);
      SQTree *borrowed = sq_tree_from_bytes_borrowed(language, reference, size, &error);
      assert(borrowed && error == SQ_OK && sq_tree_data(borrowed, NULL) == reference);
      uint32_t count = sq_node_descendant_count(sq_tree_root_node(tree));
      assert(count == sq_node_descendant_count(sq_tree_root_node(copy)));
      assert(count == sq_node_descendant_count(sq_tree_root_node(borrowed)));
      sq_tree_delete(copy);
      sq_tree_delete(borrowed);
      free(reference);
    }
    sq_tree_delete(tree);
  }
  ts_tree_delete(parsed);
  ts_parser_delete(parser);
  free(source);
  dlclose(library);
  return 0;
}
