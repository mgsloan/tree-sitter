// Exercise the actual mask interner and emitter with synthetic inherited masks.
// Including pack.c keeps the builder private to production code.
#include "../pack.c"
#include <assert.h>
#include <stdio.h>
#include <pthread.h>
#include <stdatomic.h>
#include "supertype_fixture.h"

static void check_tree(SQTree *tree, const uint32_t *slots, uint32_t count) {
  assert(tree->layout.supertype_bits == 16);
  assert(sq_header_get(tree, supertype_dictionary_count) == 512);
  for (uint32_t i = 0; i < count; i++) {
    SQNode node = {tree, slots[i]};
    assert(sq_node_supertype(node) == i);
    for (uint32_t bit = 0; bit < 9; bit++) {
      assert(sq_node_has_supertype(node, bit + 2) == ((i & (1u << bit)) != 0));
    }
    uint64_t matches = sq_tree_group_supertype_equal(tree, slots[i] / SQ_GROUP_SIZE, i);
    assert(matches & (UINT64_C(1) << (slots[i] % SQ_GROUP_SIZE)));
    assert(!sq_tree_group_supertype_equal(tree, slots[i] / SQ_GROUP_SIZE, 65536));
  }
}

static void exercise(uint32_t count, bool repack) {
  SupertypeFixture fixture;
  supertype_fixture(&fixture, 9, true);
  TSLanguage language = fixture.language;
  SQError error;
  SQGrammar *grammar = sq_grammar_new(&language, &error);
  assert(grammar);
  Builder builder = {.public_index = grammar->public_index, .tree = sq_allocate(grammar, 1, true, &error), .words = 1,
                     .language = &language, .symbol_count = 11, .symbol_space = 13,
                     .error = &error};
  assert(builder.tree);
  Subtree leaf = {.data = {.is_inline = true, .symbol = 1}};
  uint32_t slots[512];
  for (uint32_t i = 0; i < count; i++) {
    EmitNode node = {.subtree = &leaf, .mask = i, .boundary = distance(&builder),
                     .later = i != 0};
    assert(emit(&builder, &node));
    slots[i] = distance(&builder) - 1;
    if (i == 0) {
      // Duplicate masks retain their grammar-wide ID.
      node.boundary = distance(&builder);
      node.later = true;
      assert(emit(&builder, &node));
    }
    assert(builder.tree->layout.supertype_bits == 16);
  }
  EmitNode root = {.subtree = &leaf, .boundary = 0};
  assert(emit(&builder, &root));
  assert(close_group(&builder));
  uint32_t capacity = sq_header_get(builder.tree, group_count) + (repack ? 0 : 7);
  assert(sq_prepare_final(&builder.tree, capacity, 0, builder.optional_flags, &error));
  check_tree(builder.tree, slots, count);
  assert(sq_resize(&builder.tree, capacity + 17, &error));
  check_tree(builder.tree, slots, count);
  assert(sq_resize(&builder.tree, sq_header_get(builder.tree, group_count), &error));
  check_tree(builder.tree, slots, count);
  uint32_t length;
  const void *bytes = sq_tree_data(builder.tree, &length);
  SQTree *copy = sq_tree_from_bytes(grammar, bytes, length, &error);
  assert(copy && error == SQ_OK);
  check_tree(copy, slots, count);
  SQTree *borrowed = sq_tree_from_bytes_borrowed(grammar, bytes, length, &error);
  assert(borrowed && error == SQ_OK);
  check_tree(borrowed, slots, count);
  sq_tree_delete(borrowed);
  assert(copy->supertype_grammar == builder.tree->supertype_grammar);
  // Fixed and variable layouts must reject one another, even at identical widths.
  sq_header_set(builder.tree, format_flags, sq_header_get(builder.tree, format_flags) ^ 1u);
  assert(!sq_tree_from_bytes(grammar, bytes, length, &error));
  assert(error == SQ_ERROR_INVALID_SLAB);
  sq_header_set(builder.tree, format_flags, sq_header_get(builder.tree, format_flags) ^ 1u);
  // Reject widths inconsistent with the dictionary count, and oversized counts.
  sq_header_set(builder.tree, format_flags, sq_header_get(builder.tree, format_flags) ^ SQ_WIDE_SUPERTYPES);
  assert(!sq_tree_from_bytes(grammar, bytes, length, &error));
  assert(error == SQ_ERROR_INVALID_SLAB);
  sq_header_set(builder.tree, format_flags, sq_header_get(builder.tree, format_flags) ^ SQ_WIDE_SUPERTYPES);
  sq_header_set(builder.tree, supertype_dictionary_count, 65537);
  assert(!sq_tree_from_bytes(grammar, bytes, length, &error));
  assert(error == SQ_ERROR_INVALID_SLAB);
  sq_tree_delete(builder.tree);
  check_tree(copy, slots, count);
  uint8_t *saved = malloc(copy->size);
  assert(saved);
  memcpy(saved, copy->data, copy->size);
  length = copy->size;
  uint32_t grammar_cache_size = sq_tree_grammar_cache_size(copy);
  uint8_t *grammar_cache = malloc(grammar_cache_size);
  assert(grammar_cache &&
         sq_tree_copy_grammar_cache(copy, grammar_cache, grammar_cache_size, &error));
  sq_tree_delete(copy);
  sq_grammar_delete(grammar);
  grammar = sq_grammar_new_with_cache(&language, grammar_cache, grammar_cache_size, &error);
  assert(grammar);
  copy = sq_tree_from_bytes_safety_checked(grammar, saved, length, &error);
  assert(copy);
  check_tree(copy, slots, count);
  sq_tree_delete(copy);
  copy = sq_tree_from_bytes(grammar, saved, length, &error);
  assert(copy);
  check_tree(copy, slots, count);
  sq_tree_delete(copy);
  sq_grammar_delete(grammar);
  free(grammar_cache);
  free(saved);
}

static void direct_mask_tests(void) {
  for (unsigned bits = 0; bits <= 8; bits++) {
    SupertypeFixture fixture;
    supertype_fixture(&fixture, bits, true);
    const TSLanguage *language = &fixture.language;
    SQError error;
    SQGrammar *grammar = sq_grammar_new(language, &error);
    assert(grammar);
    Builder builder = {.public_index = grammar->public_index, .tree = sq_allocate(grammar, 1, true, &error), .words = 1,
                       .language = language, .small_supertypes = true,
                       .symbol_count = language->symbol_count,
                       .symbol_space = language->symbol_count + 2, .error = &error};
    assert(builder.tree && builder.tree->layout.supertype_bits == 16);
    Subtree leaf = {.data = {.is_inline = true, .symbol = 1}};
    uint32_t slots[256];
    unsigned count = 1u << bits;
    for (unsigned mask = 0; mask < count; mask++) {
      EmitNode node = {.subtree = &leaf, .mask = mask, .boundary = distance(&builder),
                       .later = mask != 0};
      assert(emit(&builder, &node));
      slots[mask] = distance(&builder) - 1;
    }
    EmitNode root = {.subtree = &leaf, .boundary = 0};
    assert(emit(&builder, &root) && close_group(&builder));
    unsigned groups = sq_tree_group_count(builder.tree);
    assert(sq_prepare_final(&builder.tree, groups, 0, builder.optional_flags, &error));
    // Exercise the emitter, resize copy, and both persistence ownership modes.
    for (unsigned pass = 0; pass < 3; pass++) {
      assert(sq_resize(&builder.tree, groups + (pass == 1 ? 17 : 0), &error));
      uint32_t length;
      const void *bytes = sq_tree_data(builder.tree, &length);
      SQTree *copy = sq_tree_from_bytes(grammar, bytes, length, &error);
      SQTree *borrowed = sq_tree_from_bytes_borrowed(grammar, bytes, length, &error);
      assert(copy && borrowed);
      SQTree *trees[] = {builder.tree, copy, borrowed};
      for (unsigned t = 0; t < 3; t++) {
        for (unsigned mask = 0; mask < count; mask++) {
          SQNode node = {trees[t], slots[mask]};
          assert(sq_node_supertype(node) == mask);
          for (unsigned bit = 0; bit < bits; bit++) {
            assert(sq_node_has_supertype(node, bit + 2) == ((mask & (1u << bit)) != 0));
          }
          assert(sq_tree_group_supertype_equal(trees[t], slots[mask] / SQ_GROUP_SIZE, mask)
                 & (UINT64_C(1) << (slots[mask] % SQ_GROUP_SIZE)));
        }
      }
      sq_tree_delete(copy);
      sq_tree_delete(borrowed);
    }
    sq_tree_delete(builder.tree);
    sq_grammar_delete(grammar);
  }
}

static void dictionary_tests(void) {
  SQError error = SQ_OK;
  SupertypeFixture fixture;
  supertype_fixture(&fixture, 9, true);
  SQGrammar *grammar = sq_grammar_new(&fixture.language, &error);
  assert(grammar);
  SQPackContext *context = sq_pack_context_new(&error);
  assert(context);
  SQSupertypeGrammar *first = grammar->supertype_grammar;
  assert(first && first->count == 512);
  sq_pack_context_trim(context);
  for (uint64_t mask = 0; mask < 512; mask++) assert(sq_supertype_mask_id(first, &mask) == mask);
  uint64_t expected[512];
  memcpy(expected, first->masks, sizeof(expected));
  uint32_t cache_size = sq_grammar_cache_size(grammar);
  uint8_t *cache_bytes = malloc(cache_size);
  assert(cache_size == 16 + sizeof(expected) && cache_bytes);
  assert(sq_grammar_copy_cache(grammar, cache_bytes, cache_size, &error));
  sq_pack_context_delete(context);
  sq_grammar_delete(grammar);
  grammar = sq_grammar_new_with_cache(&fixture.language, cache_bytes, cache_size, &error);
  assert(grammar && !memcmp(expected, grammar->supertype_grammar->masks, sizeof(expected)));
  sq_grammar_delete(grammar);
  cache_bytes[0] ^= 1;
  assert(!sq_grammar_new_with_cache(&fixture.language, cache_bytes, cache_size, &error));
  assert(error == SQ_ERROR_INVALID_SLAB);
  free(cache_bytes);
  SQSupertypeGrammar *second = sq_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && !memcmp(expected, second->masks, sizeof(expected)));
  sq_supertype_grammar_delete(second);

  supertype_fixture(&fixture, 65, false);
  second = sq_supertype_grammar_new(&fixture.language, 65, &error);
  assert(second && second->count == 66 && second->words == 2);
  uint64_t mask[2] = {0, 1};
  assert(sq_supertype_mask_id(second, mask) == 65);
  sq_supertype_grammar_delete(second);

  supertype_fixture(&fixture, 16, true);
  second = sq_supertype_grammar_new(&fixture.language, 16, &error);
  assert(second && second->count == 65536);
  mask[0] = 65535;
  assert(sq_supertype_mask_id(second, mask) == 65535);
  sq_supertype_grammar_delete(second);

  // Aliases end inherited paths even when the raw child is hidden.
  supertype_fixture(&fixture, 9, true);
  TSSymbol alias_sequences[] = {0, 1};
  fixture.language.alias_sequences = alias_sequences;
  fixture.language.max_alias_sequence_length = 1;
  fixture.language.production_id_count = 2;
  for (unsigned i = 0; i < 9; i++) fixture.actions[i + 2].action.reduce.production_id = 1;
  second = sq_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 10);
  mask[0] = 3;
  assert(sq_supertype_mask_id(second, mask) == SQ_NONE);
  sq_supertype_grammar_delete(second);

  // A visible supertype alias contributes its own bit to the raw node's children.
  supertype_fixture(&fixture, 9, false);
  fixture.table[fixture.language.symbol_count + 4] = 2;
  fixture.table[2 * fixture.language.symbol_count] = 1;
  fixture.actions[1].entry.count = 1;
  fixture.actions[2].action.reduce.type = TSParseActionTypeReduce;
  fixture.actions[2].action.reduce.symbol = 2;
  fixture.actions[2].action.reduce.child_count = 1;
  fixture.public_symbols[2] = 3;
  second = sq_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 12);
  mask[0] = 6;
  assert(sq_supertype_mask_id(second, mask) != SQ_NONE);
  sq_supertype_grammar_delete(second);
  mask[0] = 3;

  // Ordinary recursive gotos must not make their symbols universal extras.
  supertype_fixture(&fixture, 9, false);
  fixture.table[fixture.language.symbol_count + 2] = 1;
  second = sq_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 10);
  assert(sq_supertype_mask_id(second, mask) == SQ_NONE);
  sq_supertype_grammar_delete(second);
  // Nonterminal extras end with a null lookahead and an EOF reduction.
  fixture.lex_modes[2].lex_state = UINT16_MAX;
  fixture.table[2 * fixture.language.symbol_count] = 1;
  fixture.actions[1].entry.count = 1;
  fixture.actions[2].action.reduce.type = TSParseActionTypeReduce;
  fixture.actions[2].action.reduce.symbol = 2;
  fixture.actions[2].action.reduce.child_count = 1;
  second = sq_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 18);
  assert(sq_supertype_mask_id(second, mask) != SQ_NONE);
  sq_supertype_grammar_delete(second);

  // A hidden first child followed by a visible token uses the full backward
  // walk. Aliasing that first child must still terminate mask inheritance.
  supertype_fixture(&fixture, 9, false);
  fixture.table[3] = 1; // state 0 -- hidden supertype 3 --> state 1
  fixture.table[fixture.language.symbol_count + 1] = 3;
  fixture.actions[3].entry.count = 1;
  fixture.actions[4].action.shift.type = TSParseActionTypeShift;
  fixture.actions[4].action.shift.state = 2;
  fixture.table[2 * fixture.language.symbol_count] = 1;
  fixture.actions[1].entry.count = 1;
  fixture.actions[2].action.reduce.type = TSParseActionTypeReduce;
  fixture.actions[2].action.reduce.symbol = 2;
  fixture.actions[2].action.reduce.child_count = 2;
  second = sq_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 11);
  mask[0] = 3;
  assert(sq_supertype_mask_id(second, mask) != SQ_NONE);
  sq_supertype_grammar_delete(second);
  TSSymbol two_child_aliases[] = {0, 0, 1, 0};
  fixture.language.alias_sequences = two_child_aliases;
  fixture.language.max_alias_sequence_length = 2;
  fixture.language.production_id_count = 2;
  fixture.actions[2].action.reduce.production_id = 1;
  second = sq_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 10);
  assert(sq_supertype_mask_id(second, mask) == SQ_NONE);
  sq_supertype_grammar_delete(second);

  supertype_fixture(&fixture, 17, true);
  assert(!sq_supertype_grammar_new(&fixture.language, 17, &error));
  assert(error == SQ_ERROR_DICTIONARY_FULL);
}

typedef struct {
  SQGrammar *prepared;
  SQSupertypeGrammar *grammar;
} ThreadArgument;
static atomic_uint ready;
static atomic_bool release_threads;
static void *cache_thread(void *argument) {
  ThreadArgument *arg = argument;
  SQGrammar *prepared = sq_grammar_copy(arg->prepared);
  arg->grammar = prepared->supertype_grammar;
  atomic_fetch_add(&ready, 1);
  while (!atomic_load(&release_threads)) {}
  sq_grammar_delete(prepared);
  for (unsigned i = 0; i < 8; i++) {
    prepared = sq_grammar_copy(arg->prepared);
    assert(prepared && prepared->supertype_grammar->count == 512);
    sq_grammar_delete(prepared);
  }
  return NULL;
}
static void concurrent_cache(void) {
  SupertypeFixture fixture;
  supertype_fixture(&fixture, 9, true);
  SQError error;
  SQGrammar *prepared = sq_grammar_new(&fixture.language, &error);
  assert(prepared);
  pthread_t threads[4];
  ThreadArgument arguments[4];
  for (unsigned i = 0; i < 4; i++) {
    arguments[i] = (ThreadArgument){.prepared = prepared};
    assert(!pthread_create(&threads[i], NULL, cache_thread, &arguments[i]));
  }
  while (atomic_load(&ready) != 4) {}
  for (unsigned i = 1; i < 4; i++) assert(arguments[i].grammar == arguments[0].grammar);
  atomic_store(&release_threads, true);
  for (unsigned i = 0; i < 4; i++) assert(!pthread_join(threads[i], NULL));
  sq_grammar_delete(prepared);
}

static void separate_grammar_emission(void) {
  SupertypeFixture fixture;
  supertype_fixture(&fixture, 0, false);
  TSLanguage language = fixture.language;
  language.symbol_count = 32767;
  TSSymbol *public_symbols = malloc(language.symbol_count * sizeof(TSSymbol));
  TSSymbolMetadata *metadata = calloc(language.symbol_count, sizeof(TSSymbolMetadata));
  assert(public_symbols && metadata);
  for (uint32_t symbol = 0; symbol < language.symbol_count; symbol++) public_symbols[symbol] = symbol;
  public_symbols[1] = 0;
  language.public_symbol_map = public_symbols;
  language.symbol_metadata = metadata;
  SQError error;
  SQGrammar *grammar = sq_grammar_new(&language, &error);
  assert(grammar && grammar->symbols.separate);
  Builder builder = {.public_index = grammar->public_index,
                     .tree = sq_allocate(grammar, 1, true, &error),
                     .language = &language, .symbol_count = language.symbol_count,
                     .symbol_space = language.symbol_count + 2, .small_supertypes = true, .error = &error};
  assert(builder.tree);
  Subtree leaf = {.data = {.is_inline = true, .symbol = 1}};
  EmitNode node = {.subtree = &leaf};
  assert(emit(&builder, &node) && close_group(&builder));
  SQNode root = sq_tree_root_node(builder.tree);
  assert(sq_node_symbol_id(root) == 0 && sq_node_grammar_id(root) == 1);
  SQTree *loaded = sq_tree_from_bytes(grammar, builder.tree->data, builder.tree->size, &error);
  assert(loaded);
  sq_tree_delete(loaded);
  sq_tree_delete(builder.tree);
  sq_grammar_delete(grammar);
  free(public_symbols);
  free(metadata);
}

int main(void) {
  separate_grammar_emission();
  direct_mask_tests();
  dictionary_tests();
  concurrent_cache();
  for (unsigned repack = 0; repack < 2; repack++) {
    exercise(256, repack);
    exercise(257, repack);
    exercise(512, repack);
  }
  puts("ok: grammar dictionaries, stable IDs, aliases, extras, limits, membership, persistence");
}
