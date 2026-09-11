#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>

// Preserve the old sibling-descent algorithm as an independent oracle. Mainline
// has known empty-node differences, so comparing only against it can conceal
// a regression in Squatter's existing behavior.
static SQNode reference(SQNode node, uint32_t left, uint32_t right, bool named) {
  if (!node.tree || left > right) return sq_null();
  SQNode result = node;
  for (;;) {
    SQNode found = sq_null();
    for (SQNode child = sq_node_child(node, 0); child.tree;
         child = sq_node_next_sibling_including_empty(child)) {
      uint32_t start = sq_node_start_byte(child), end = sq_node_end_byte(child);
      if (end < right || (start == end ? end < left : end <= left)) continue;
      if (left < start) break;
      found = child;
      break;
    }

    if (!found.tree) return result;
    node = found;
    if (!named || sq_node_is_named(node)) result = node;
  }
}

static uint64_t checks;
static const char *path;
static void check(SQNode node, uint32_t left, uint32_t right) {
  for (unsigned named = 0; named < 2; named++) {
    SQNode expected = reference(node, left, right, named);
    SQNode actual = named ? sq_node_named_descendant_for_byte_range(node, left, right)
                          : sq_node_descendant_for_byte_range(node, left, right);
    if (!sq_node_eq(expected, actual)) {
      fprintf(stderr, "%s: root=%u [%u,%u] range=[%u,%u] named=%u expected=%u actual=%u\n", path,
              node.slot, sq_node_start_byte(node), sq_node_end_byte(node), left, right, named,
              expected.tree ? expected.slot : SQ_NONE, actual.tree ? actual.slot : SQ_NONE);
      abort();
    }

    checks++;
  }
}

static uint32_t random_value(uint64_t *state) {
  *state = *state * UINT64_C(6364136223846793005) + 1;
  return (uint32_t)(*state >> 32);
}

static void exercise(TSParser *parser, const char *source, uint32_t size) {
  TSTree *parsed = ts_parser_parse_string(parser, NULL, source, size);
  assert(parsed);
  SQError error;
  SQTree *tree = sq_tree_pack(parsed, sq_pack_options_default(), &error);
  assert(tree && error == SQ_OK);
  SQNode root = sq_tree_root_node(tree);
  uint64_t state = 42;
  check(sq_null(), 0, 0);
  check(root, 0, UINT32_MAX);
  check(root, UINT32_MAX, UINT32_MAX);
  check(root, 1, 0);
  if (size <= 128) {
    for (uint32_t left = 0; left <= size + 1; left++) {
      for (uint32_t right = left; right <= size + 1; right++) check(root, left, right);
    }
  }

  for (unsigned i = 0; i < 256; i++) {
    uint32_t left = random_value(&state) % (size + 1);
    check(root, left, left);
    check(root, left, left + random_value(&state) % 32);
  }

  for (unsigned i = 0; i < 64; i++) {
    uint32_t slot = sq_previous_slot(tree, random_value(&state) % sq_tree_slot_count(tree));
    SQNode node = {tree, slot};
    uint32_t start = sq_node_start_byte(node), end = sq_node_end_byte(node);
    uint32_t positions[] = {start ? start - 1 : 0, start, start + 1,
                            end ? end - 1 : 0,     end,   end + 1};
    for (unsigned j = 0; j < 6; j++) {
      check(root, positions[j], positions[j]);
      for (unsigned k = j; k < 6; k++) check(node, positions[j], positions[k]);
    }
  }

  sq_tree_delete(tree);
  ts_tree_delete(parsed);
}

int main(int argc, char **argv) {
  assert(argc == 4);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  assert(library);
  const TSLanguage *(*language)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  assert(language);
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language()));
  FILE *inputs = fopen(argv[3], "r");
  assert(inputs);
  char *line = NULL;
  size_t capacity = 0;
  ssize_t length;
  unsigned files = 0;
  while ((length = getline(&line, &capacity, inputs)) >= 0) {
    if (length && line[length - 1] == '\n') line[--length] = 0;
    path = line;
    FILE *file = fopen(path, "rb");
    assert(file && !fseek(file, 0, SEEK_END));
    long size = ftell(file);
    assert(size >= 0 && size < UINT32_MAX);
    rewind(file);
    char *source = malloc((size_t)size + 1);
    assert(source && fread(source, 1, (size_t)size, file) == (size_t)size);
    fclose(file);
    exercise(parser, source, (uint32_t)size);
    if (size) {
      // Missing delimiters, truncation, and invalid bytes exercise error trees.
      source[size / 2] = '}';
      if (size > 4) source[size / 3] = (char)0xff;
      exercise(parser, source, (uint32_t)size - 1);
    }

    free(source);
    files++;
  }

  printf("%u files, %llu exact seek comparisons\n", files, (unsigned long long)checks);
  free(line);
  fclose(inputs);
  ts_parser_delete(parser);
  dlclose(library);
}
