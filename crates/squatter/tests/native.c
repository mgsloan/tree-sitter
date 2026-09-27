#include "internal.h"
#include "tree_feller.h"
#include "tf_lexer.h"
#undef NDEBUG
#include <assert.h>

_Static_assert(SQ_SLAB_FORMAT(0xFC, 0xAB) == UINT32_C(0xFCAB0000), "slab format fields");

// Tiny compiled tables whose reductions permit every supertype to wrap every
// other one. The analysis must compute the full power set, including cycles.
typedef struct {
  TSLanguage language;
  TSSymbolMetadata metadata[70];
  TSSymbol public_symbols[70], aliases[1];
  uint16_t table[3 * 70];
  TSParseActionEntry actions[70];
  TSLexerMode lex_modes[3];
} SupertypeFixture;
static void supertype_fixture(SupertypeFixture *fixture, unsigned count, bool connected) {
  memset(fixture, 0, sizeof(*fixture));
  uint32_t symbols = count + 2;
  assert(symbols <= 70);
  fixture->language = (TSLanguage){.abi_version = TREE_SITTER_LANGUAGE_VERSION,
      .symbol_count = symbols, .token_count = 2, .state_count = 3, .large_state_count = 3,
      .symbol_metadata = fixture->metadata, .public_symbol_map = fixture->public_symbols,
      .alias_map = fixture->aliases, .parse_table = fixture->table, .parse_actions = fixture->actions, .lex_modes = fixture->lex_modes};
  fixture->metadata[1].visible = fixture->metadata[1].named = true;
  for (uint32_t i = 0; i < symbols; i++) {
    fixture->public_symbols[i] = (TSSymbol)i;
    fixture->metadata[i].supertype = i >= 2;
    if (i >= 2 && connected) fixture->table[symbols + i] = 2;
  }
  if (connected) {
    fixture->table[2 * symbols] = 1;
    fixture->actions[1].entry.count = (uint8_t)count;
    for (unsigned i = 0; i < count; i++) {
      fixture->actions[i + 2].action = (TSParseAction){.reduce = {
          .type = TSParseActionTypeReduce, .symbol = (TSSymbol)(i + 2), .child_count = 1}};
    }
  }
}

void sq_test_dictionaries(void) {
  SQError error = SQ_OK;
  SupertypeFixture fixture;
  supertype_fixture(&fixture, 9, true);
  SQGrammar *grammar = sq_native_grammar_new(&fixture.language, &error);
  assert(grammar);
  SQSupertypeGrammar *first = grammar->supertype_grammar;
  assert(first && first->count == 512);
  for (uint64_t mask = 0; mask < 512; mask++) assert(sq_native_supertype_mask_id(first, &mask) == mask);
  uint64_t expected[512];
  memcpy(expected, first->masks, sizeof(expected));
  uint32_t cache_size = sq_native_grammar_cache_size(grammar);
  uint8_t *cache_bytes = malloc(cache_size);
  assert(cache_size == 16 + sizeof(expected) && cache_bytes);
  assert(sq_native_grammar_copy_cache(grammar, cache_bytes, cache_size, &error));
  assert(cache_bytes[0] == 0 && cache_bytes[1] == 0 && cache_bytes[2] == 0 && cache_bytes[3] == 0xFC);
  sq_native_grammar_delete(grammar);
  grammar = sq_native_grammar_new_with_cache(&fixture.language, cache_bytes, cache_size, &error);
  assert(grammar && !memcmp(expected, grammar->supertype_grammar->masks, sizeof(expected)));
  sq_native_grammar_delete(grammar);
  for (unsigned bit = 0; bit < 32; bit++) {
    cache_bytes[bit / 8] ^= 1u << (bit % 8);
    assert(!sq_native_grammar_new_with_cache(&fixture.language, cache_bytes, cache_size, &error));
    assert(error == SQ_ERROR_INVALID_SLAB);
    cache_bytes[bit / 8] ^= 1u << (bit % 8);
  }
  free(cache_bytes);
  SQSupertypeGrammar *second = sq_native_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && !memcmp(expected, second->masks, sizeof(expected)));
  sq_native_supertype_grammar_delete(second);

  supertype_fixture(&fixture, 65, false);
  second = sq_native_supertype_grammar_new(&fixture.language, 65, &error);
  assert(second && second->count == 66 && second->words == 2);
  uint64_t mask[2] = {0, 1};
  assert(sq_native_supertype_mask_id(second, mask) == 65);
  sq_native_supertype_grammar_delete(second);

  supertype_fixture(&fixture, 16, true);
  second = sq_native_supertype_grammar_new(&fixture.language, 16, &error);
  assert(second && second->count == 65536);
  mask[0] = 65535;
  assert(sq_native_supertype_mask_id(second, mask) == 65535);
  sq_native_supertype_grammar_delete(second);

  // Aliases end inherited paths even when the raw child is hidden.
  supertype_fixture(&fixture, 9, true);
  TSSymbol alias_sequences[] = {0, 1};
  fixture.language.alias_sequences = alias_sequences;
  fixture.language.max_alias_sequence_length = 1;
  fixture.language.production_id_count = 2;
  for (unsigned i = 0; i < 9; i++) fixture.actions[i + 2].action.reduce.production_id = 1;
  second = sq_native_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 10);
  mask[0] = 3;
  assert(sq_native_supertype_mask_id(second, mask) == SQ_NONE);
  sq_native_supertype_grammar_delete(second);

  // A visible supertype alias contributes its own bit to the raw node's children.
  supertype_fixture(&fixture, 9, false);
  fixture.table[fixture.language.symbol_count + 4] = 2;
  fixture.table[2 * fixture.language.symbol_count] = 1;
  fixture.actions[1].entry.count = 1;
  fixture.actions[2].action.reduce.type = TSParseActionTypeReduce;
  fixture.actions[2].action.reduce.symbol = 2;
  fixture.actions[2].action.reduce.child_count = 1;
  fixture.public_symbols[2] = 3;
  second = sq_native_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 12);
  mask[0] = 6;
  assert(sq_native_supertype_mask_id(second, mask) != SQ_NONE);
  sq_native_supertype_grammar_delete(second);
  mask[0] = 3;

  // Ordinary recursive gotos must not make their symbols universal extras.
  supertype_fixture(&fixture, 9, false);
  fixture.table[fixture.language.symbol_count + 2] = 1;
  second = sq_native_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 10);
  assert(sq_native_supertype_mask_id(second, mask) == SQ_NONE);
  sq_native_supertype_grammar_delete(second);
  // Nonterminal extras end with a null lookahead and an EOF reduction.
  fixture.lex_modes[2].lex_state = UINT16_MAX;
  fixture.table[2 * fixture.language.symbol_count] = 1;
  fixture.actions[1].entry.count = 1;
  fixture.actions[2].action.reduce.type = TSParseActionTypeReduce;
  fixture.actions[2].action.reduce.symbol = 2;
  fixture.actions[2].action.reduce.child_count = 1;
  second = sq_native_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 18);
  assert(sq_native_supertype_mask_id(second, mask) != SQ_NONE);
  sq_native_supertype_grammar_delete(second);

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
  second = sq_native_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 11);
  mask[0] = 3;
  assert(sq_native_supertype_mask_id(second, mask) != SQ_NONE);
  sq_native_supertype_grammar_delete(second);
  TSSymbol two_child_aliases[] = {0, 0, 1, 0};
  fixture.language.alias_sequences = two_child_aliases;
  fixture.language.max_alias_sequence_length = 2;
  fixture.language.production_id_count = 2;
  fixture.actions[2].action.reduce.production_id = 1;
  second = sq_native_supertype_grammar_new(&fixture.language, 9, &error);
  assert(second && second->count == 10);
  assert(sq_native_supertype_mask_id(second, mask) == SQ_NONE);
  sq_native_supertype_grammar_delete(second);

  supertype_fixture(&fixture, 17, true);
  assert(!sq_native_supertype_grammar_new(&fixture.language, 17, &error));
  assert(error == SQ_ERROR_DICTIONARY_FULL);
}

static bool lex(TSLexer *lexer, TSStateId state) {
  if (state == 1) {
    // A failed speculative lex can consume whitespace before falling back.
    if (lexer->lookahead == '\n') lexer->advance(lexer, true);
    return false;
  }
  if (lexer->lookahead == '\n') lexer->advance(lexer, false);
  if (lexer->lookahead == 'x') {
    lexer->advance(lexer, false);
    lexer->result_symbol = 1;
    lexer->mark_end(lexer);
    return true;
  }
  lexer->result_symbol = 0;
  return lexer->eof(lexer);
}

static const TSLanguage language = {
  .abi_version = 15,
  .symbol_count = 3,
  .token_count = 2,
  .state_count = 4,
  .large_state_count = 4,
  .production_id_count = 1,
  .symbol_names = (const char *const[]){"end", "x", "root"},
  .symbol_metadata = (const TSSymbolMetadata[]){{0}, {.visible = true},
                                              {.visible = true, .named = true}},
  .public_symbol_map = (const TSSymbol[]){0, 1, 2},
  .alias_map = (const TSSymbol[]){0},
  .parse_table = (const uint16_t[]){0, 0, 0, 0, 1, 3, 3, 0, 0, 5, 0, 0},
  .parse_actions = (const TSParseActionEntry[]){
    {.entry = {0}}, {.entry = {.count = 1, .reusable = true}}, SHIFT(2),
    {.entry = {.count = 1, .reusable = true}}, REDUCE(2, 1, 0, 0),
    {.entry = {.count = 1, .reusable = true}}, ACCEPT_INPUT(),
  },
  .lex_modes = (const TSLexerMode[]){{0}, {.lex_state = 1}, {0}, {0}},
  .lex_fn = lex,
};


const TSLanguage *sq_test_parser_language(void) { return &language; }

void sq_test_lexer_fallback(void) {
  const char *message;
  TFLanguage *prepared = tf_language_load(&language, &message);
  assert(prepared);
  TFLexer lexer;
  TFToken token;
  tf_lexer_init(&lexer, prepared, "\nx", 2);
  assert(tf_lexer_next(&lexer, 1, &token));
  assert(token.start_byte == 0 && token.end_byte == 2);
  assert(token.start_point.row == 0 && token.end_point.row == 1);
  assert(lexer.token_lex_state == 1);
  tf_language_free(prepared);
}

const TSLanguage *sq_test_supertypes(unsigned count, bool connected) {
  SupertypeFixture *fixture = malloc(sizeof(*fixture));
  assert(fixture);
  supertype_fixture(fixture, count, connected);
  return &fixture->language;
}

void sq_test_supertypes_delete(const TSLanguage *language) { free((void *)language); }

typedef struct {
  TSLanguage language;
  TSSymbol *public_symbols;
  TSSymbolMetadata *metadata;
  uint16_t aliases[5];
} SymbolFixture;

const TSLanguage *sq_test_symbols(unsigned count) {
  SymbolFixture *fixture = calloc(1, sizeof(*fixture));
  assert(fixture);
  fixture->public_symbols = malloc(count * sizeof(TSSymbol));
  fixture->metadata = calloc(count, sizeof(TSSymbolMetadata));
  assert(fixture->public_symbols && fixture->metadata);
  for (unsigned symbol = 0; symbol < count; symbol++) {
    fixture->public_symbols[symbol] = symbol;
    fixture->metadata[symbol].visible = symbol != 0;
  }
  fixture->language = (TSLanguage){
    .abi_version = TREE_SITTER_LANGUAGE_VERSION,
    .symbol_count = count,
    .public_symbol_map = fixture->public_symbols,
    .symbol_metadata = fixture->metadata,
  };
  return &fixture->language;
}

const TSLanguage *sq_test_compact_symbols(unsigned count) {
  SymbolFixture *fixture = (SymbolFixture *)sq_test_symbols(count);
  fixture->metadata[2].visible = false;
  fixture->metadata[3].visible = false;
  for (unsigned symbol = 4; symbol < count; symbol++) fixture->public_symbols[symbol] = 1;
  fixture->public_symbols[5] = 5;
  fixture->aliases[0] = 2;
  fixture->aliases[1] = 2;
  fixture->aliases[2] = 2;
  fixture->aliases[3] = 1;
  fixture->language.alias_map = fixture->aliases;
  return &fixture->language;
}

void sq_test_symbols_delete(const TSLanguage *language) {
  SymbolFixture *fixture = (SymbolFixture *)language;
  free(fixture->metadata);
  free(fixture->public_symbols);
  free(fixture);
}

void sq_test_grammar_limits(void) {
  TSLanguage invalid = {.abi_version = TREE_SITTER_LANGUAGE_VERSION, .symbol_count = 65535};
  SQError error;
  assert(!sq_native_grammar_new(&invalid, &error) && error == SQ_ERROR_OVERFLOW);
  invalid.symbol_count = 1;
  invalid.alias_count = UINT32_MAX;
  assert(!sq_native_grammar_new(&invalid, &error) && error == SQ_ERROR_OVERFLOW);
  invalid.alias_count = 0;
  invalid.field_count = 65536;
  assert(!sq_native_grammar_new(&invalid, &error) && error == SQ_ERROR_OVERFLOW);
}

#include "reductions.h"

void sq_test_unsupported_parsers(void) {
  for (unsigned variant = 0; variant < 3; variant++) {
    TSLanguage unsupported = language;
    const TSLexerMode modes[] = {{0}, {.lex_state = UINT16_MAX}, {0}, {0}};
    if (variant == 0) unsupported.abi_version = 14;
    if (variant == 1) unsupported.external_token_count = 1;
    if (variant == 2) unsupported.lex_modes = modes;
    SQError error;
    SQGrammar *grammar = sq_native_grammar_new(&unsupported, &error);
    assert(grammar);
    SQParseError diagnostic;
    assert(!sq_native_parser_new(grammar, &diagnostic));
    assert(diagnostic.code == SQ_ERROR_LANGUAGE && diagnostic.message[0]);
    sq_native_grammar_delete(grammar);
  }
}

const TSLanguage *sq_test_clone_language(const TSLanguage *language) {
  TSLanguage *copy = malloc(sizeof(*copy));
  assert(copy);
  *copy = *language;
  return copy;
}
