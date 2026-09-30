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

typedef struct {
  const char *source;
  uint32_t size, chunk_size;
  uint32_t maximum, replays;
  char buffer[8];
} ChunkInput;

static const char *read_chunk(void *payload, uint32_t byte, TFPoint point, uint32_t *size) {
  ChunkInput *input = payload;
  assert(byte <= input->size);
  if (byte == 0 && input->maximum > 0) input->replays++;
  if (byte > input->maximum) input->maximum = byte;
  TFPoint expected = {0};
  for (uint32_t index = 0; index < byte; index++) {
    if (input->source[index] == '\n') expected.row++, expected.column = 0;
    else expected.column++;
  }
  assert(point.row == expected.row && point.column == expected.column);
  *size = input->chunk_size - byte % input->chunk_size;
  if (*size > input->size - byte) *size = input->size - byte;
  memset(input->buffer, 0xa5, sizeof(input->buffer));
  memcpy(input->buffer, input->source + byte, *size);
  return input->buffer;
}

static const char *overflow_chunk(void *payload, uint32_t byte, TFPoint point, uint32_t *size) {
  (void)payload;
  (void)point;
  *size = byte ? UINT32_MAX : 1;
  return "\n";
}

static bool lex_expression(TSLexer *lexer, TSStateId state) {
  (void)state;
  while (lexer->lookahead == ' ' || lexer->lookahead == '\n') lexer->advance(lexer, true);
  if (lexer->eof(lexer)) {
    lexer->result_symbol = 0;
    return true;
  }
  if (lexer->lookahead != 'x' && lexer->lookahead != '+') return false;
  lexer->result_symbol = lexer->lookahead == 'x' ? 1 : 2;
  lexer->advance(lexer, false);
  lexer->mark_end(lexer);
  return true;
}

// root = expression; expression = 'x' | expression '+' expression, with no associativity.
static const TSLanguage ambiguous_language = {
  .abi_version = 15, .symbol_count = 5, .token_count = 3,
  .state_count = 7, .large_state_count = 7, .production_id_count = 1,
  .symbol_names = (const char *const[]){"end", "x", "+", "root", "expression"},
  .symbol_metadata = (const TSSymbolMetadata[]){{0}, {.visible = true}, {.visible = true},
      {.visible = true, .named = true}, {.visible = true, .named = true}},
  .public_symbol_map = (const TSSymbol[]){0, 1, 2, 3, 4},
  .alias_map = (const TSSymbol[]){0},
  .parse_table = (const uint16_t[]){
      0, 0, 0, 0, 0,
      0, 1, 0, 6, 3,
      3, 0, 3, 0, 0,
      7, 0, 5, 0, 0,
      0, 1, 0, 0, 5,
      12, 0, 9, 0, 0,
      14, 0, 0, 0, 0,
  },
  .parse_actions = (const TSParseActionEntry[]){
      {.entry = {0}}, {.entry = {.count = 1}}, SHIFT(2),
      {.entry = {.count = 1}}, REDUCE(4, 1, 0, 0),
      {.entry = {.count = 1}}, SHIFT(4),
      {.entry = {.count = 1}}, REDUCE(3, 1, 0, 0),
      {.entry = {.count = 2}}, REDUCE(4, 3, 0, 0), SHIFT(4),
      {.entry = {.count = 1}}, REDUCE(4, 3, 0, 0),
      {.entry = {.count = 1}}, ACCEPT_INPUT(),
  },
  .lex_modes = (const TSLexerMode[7]){{0}},
  .lex_fn = lex_expression,
};

const TSLanguage *sq_test_ambiguous_language(void) { return &ambiguous_language; }

static bool lex_non_terminal_extra(TSLexer *lexer, TSStateId state) {
  assert(state != UINT16_MAX);
  while (lexer->lookahead == ' ' || lexer->lookahead == '\n') lexer->advance(lexer, true);
  if (lexer->lookahead != '[' && lexer->lookahead != ']') return lex_expression(lexer, state);
  lexer->result_symbol = lexer->lookahead == '[' ? 3 : 4;
  lexer->advance(lexer, false);
  lexer->mark_end(lexer);
  return true;
}

// The ambiguous expression grammar with comment = '[' ']'. The same comment
// can be a structural expression at the start and an extra everywhere else.
const TSLanguage *sq_test_non_terminal_extra_language(bool structural, bool conflict) {
  typedef struct {
    TSLanguage language;
    uint16_t table[9][8];
  } ExtraFixture;
  ExtraFixture *fixture = malloc(sizeof(*fixture));
  assert(fixture);
  *fixture = (ExtraFixture){.language = {
      .abi_version = 15, .symbol_count = 8, .token_count = 5,
      .state_count = 9, .large_state_count = 9, .production_id_count = 1,
      .lex_fn = lex_non_terminal_extra,
  }, .table = {
      {0, 0, 0, 0, 0, 0, 0, 0},
      {0, 1, 0, 16, 0, 6, 3, 1},
      {3, 0, 3, 16, 0, 0, 0, 2},
      {7, 0, 5, 16, 0, 0, 0, 3},
      {0, 1, 0, 16, 0, 0, 5, 4},
      {12, 0, 9, 16, 0, 0, 0, 5},
      {14, 0, 0, 16, 0, 0, 0, 6},
      {0, 0, 0, 0, 18, 0, 0, 0},
      {20, 0, 0, 0, 0, 0, 0, 0},
  }};
  static const char *const names[] = {"end", "x", "+", "[", "]", "root", "expression", "comment"};
  static const TSSymbolMetadata metadata[] = {{0}, {.visible = true}, {.visible = true},
      {.visible = true}, {.visible = true}, {.visible = true, .named = true},
      {.visible = true, .named = true}, {.visible = true, .named = true}};
  static const TSSymbol symbols[] = {0, 1, 2, 3, 4, 5, 6, 7}, aliases[] = {0};
  static const TSLexerMode modes[9] = {[8] = {.lex_state = UINT16_MAX}};
  static const TSParseActionEntry actions[] = {
      {.entry = {0}}, {.entry = {.count = 1}}, SHIFT(2),
      {.entry = {.count = 1}}, REDUCE(6, 1, 0, 0),
      {.entry = {.count = 1}}, SHIFT(4),
      {.entry = {.count = 1}}, REDUCE(5, 1, 0, 0),
      {.entry = {.count = 2}}, REDUCE(6, 3, 0, 0), SHIFT(4),
      {.entry = {.count = 1}}, REDUCE(6, 3, 0, 0),
      {.entry = {.count = 1}}, ACCEPT_INPUT(),
      {.entry = {.count = 1}}, SHIFT(7),
      {.entry = {.count = 1}}, SHIFT(8),
      {.entry = {.count = 1}}, REDUCE(7, 2, 0, 0),
      {.entry = {.count = 2}}, REDUCE(7, 2, 1, 0), REDUCE(7, 2, 0, 0),
  };
  fixture->language.symbol_names = names;
  fixture->language.symbol_metadata = metadata;
  fixture->language.public_symbol_map = symbols;
  fixture->language.alias_map = aliases;
  fixture->language.parse_table = &fixture->table[0][0];
  fixture->language.parse_actions = actions;
  fixture->language.lex_modes = modes;
  if (structural) fixture->table[1][7] = 2;
  if (conflict) fixture->table[8][0] = 22;
  return &fixture->language;
}

typedef struct {
  unsigned count;
} ScannerFixture;

static void *scanner_create(void) {
  return calloc(1, sizeof(ScannerFixture));
}

static void scanner_destroy(void *payload) {
  free(payload);
}

static unsigned scanner_serialize(void *payload, char *buffer) {
  ScannerFixture *scanner = payload;
  if (!scanner->count) return 0;
  memcpy(buffer, &scanner->count, sizeof(scanner->count));
  return sizeof(scanner->count);
}

static void scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  ScannerFixture *scanner = payload;
  scanner->count = 0;
  if (length) {
    assert(length == sizeof(scanner->count));
    memcpy(&scanner->count, buffer, length);
  }
}

static bool scanner_expression(void *payload, TSLexer *lexer, const bool *valid) {
  ScannerFixture *scanner = payload;
  while (lexer->lookahead == ' ' || lexer->lookahead == '\n') lexer->advance(lexer, true);
  if (valid[0] && lexer->lookahead == (int32_t)('a' + scanner->count)) {
    scanner->count = (scanner->count + 1) % 4;
    lexer->advance(lexer, false);
    lexer->mark_end(lexer);
    // Read past the token; the driver must return to mark_end.
    if (!lexer->eof(lexer)) lexer->advance(lexer, false);
    lexer->result_symbol = 0;
    return true;
  }
  // Neither failed scans nor another branch's scans may affect the next call.
  scanner->count = 100;
  if (!lexer->eof(lexer)) lexer->advance(lexer, true);
  return false;
}

static bool scanner_branch(void *payload, TSLexer *lexer, const bool *valid) {
  ScannerFixture *scanner = payload;
  if (lexer->lookahead == 'x' && (valid[0] || valid[1])) {
    scanner->count = valid[0] ? 1 : 2;
    lexer->result_symbol = valid[0] ? 0 : 1;
  } else if (lexer->lookahead == '!' && valid[2] && scanner->count == 2) {
    lexer->result_symbol = 2;
  } else {
    scanner->count = 100;
    return false;
  }
  lexer->advance(lexer, false);
  lexer->mark_end(lexer);
  return true;
}

static bool lex_branch(TSLexer *lexer, TSStateId state) {
  (void)state;
  if (lexer->eof(lexer)) {
    lexer->result_symbol = 0;
    return true;
  }
  if (lexer->lookahead != 'a' && lexer->lookahead != ':') return false;
  lexer->result_symbol = lexer->lookahead == 'a' ? 1 : 2;
  lexer->advance(lexer, false);
  lexer->mark_end(lexer);
  return true;
}

// Both branches reach state 7 at the same byte with different scanner states.
// Only the second can scan '!'; merging or sharing its lookahead loses a branch.
static const TSLanguage scanner_branch_language = {
  .abi_version = 15, .symbol_count = 8, .token_count = 5,
  .state_count = 10, .large_state_count = 10, .production_id_count = 1,
  .symbol_names = (const char *const[]){"end", "a", ":", "x", "!", "root", "left", "right"},
  .symbol_metadata = (const TSSymbolMetadata[]){{0}, {.visible = true}, {.visible = true},
      {.visible = true}, {.visible = true}, {.visible = true, .named = true},
      {.visible = true, .named = true}, {.visible = true, .named = true}},
  .public_symbol_map = (const TSSymbol[]){0, 1, 2, 3, 4, 5, 6, 7},
  .alias_map = (const TSSymbol[]){0},
  .parse_table = (const uint16_t[]){
      0, 0, 0, 0, 0, 0, 0, 0,
      0, 1, 0, 0, 0, 8, 3, 4,
      0, 0, 3, 0, 0, 0, 0, 0,
      0, 0, 6, 0, 0, 0, 0, 0,
      0, 0, 8, 0, 0, 0, 0, 0,
      0, 0, 0, 10, 0, 0, 0, 0,
      0, 0, 0, 10, 0, 0, 0, 0,
      0, 0, 0, 0, 12, 0, 0, 0,
      14, 0, 0, 0, 0, 0, 0, 0,
      16, 0, 0, 0, 0, 0, 0, 0,
  },
  .parse_actions = (const TSParseActionEntry[]){
      {.entry = {0}}, {.entry = {.count = 1}}, SHIFT(2),
      {.entry = {.count = 2}}, REDUCE(6, 1, 0, 0), REDUCE(7, 1, 0, 0),
      {.entry = {.count = 1}}, SHIFT(5),
      {.entry = {.count = 1}}, SHIFT(6),
      {.entry = {.count = 1, .reusable = true}}, SHIFT(7),
      {.entry = {.count = 1, .reusable = true}}, SHIFT(9),
      {.entry = {.count = 1}}, ACCEPT_INPUT(),
      {.entry = {.count = 1}}, REDUCE(5, 4, 0, 0),
  },
  .lex_modes = (const TSLexerMode[]){{0}, {0}, {0}, {0}, {0},
      {.external_lex_state = 1}, {.external_lex_state = 2},
      {.external_lex_state = 3}, {0}, {0}},
  .lex_fn = lex_branch,
  .external_token_count = 3,
  .external_scanner = {
      .states = (const bool[]){false, false, false, true, false, false,
                               false, true, false, false, false, true},
      .symbol_map = (const TSSymbol[]){3, 3, 4},
      .create = scanner_create, .destroy = scanner_destroy,
      .scan = scanner_branch, .serialize = scanner_serialize, .deserialize = scanner_deserialize,
  },
};

const TSLanguage *sq_test_external_language(bool branches) {
  TSLanguage *result = malloc(sizeof(*result));
  assert(result);
  if (branches) {
    *result = scanner_branch_language;
  } else {
    *result = ambiguous_language;
    result->external_token_count = 1;
    static const bool states[] = {false, true};
    static const TSSymbol symbols[] = {1};
    static const TSLexerMode modes[7] = {
        {0}, {.external_lex_state = 1}, {.external_lex_state = 1},
        {.external_lex_state = 1}, {.external_lex_state = 1},
        {.external_lex_state = 1}, {.external_lex_state = 1}};
    result->lex_modes = modes;
    result->external_scanner.states = states;
    result->external_scanner.symbol_map = symbols;
    result->external_scanner.create = scanner_create;
    result->external_scanner.destroy = scanner_destroy;
    result->external_scanner.scan = scanner_expression;
    result->external_scanner.serialize = scanner_serialize;
    result->external_scanner.deserialize = scanner_deserialize;
  }
  return result;
}

static bool scanner_empty(void *payload, TSLexer *lexer, const bool *valid) {
  (void)valid;
  if (payload) {
    ScannerFixture *scanner = payload;
    if (scanner->count == 2) return false;
    scanner->count++;
  }
  lexer->result_symbol = 0;
  lexer->mark_end(lexer);
  return true;
}

static unsigned scanner_stateless_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

static void scanner_stateless_deserialize(void *payload, const char *buffer, unsigned length) {
  (void)payload;
  (void)buffer;
  assert(length == 0);
}

static bool scanner_column(void *payload, TSLexer *lexer, const bool *valid) {
  (void)payload;
  assert(valid[0]);
  while (lexer->lookahead == ' ' || lexer->lookahead == 0x03c0 || lexer->lookahead == 0x1f600) {
    lexer->advance(lexer, true);
  }
  if (lexer->lookahead != 'x') return false;
  assert(lexer->get_column(lexer) == 3);
  assert(!lexer->is_at_included_range_start(lexer));
  lexer->advance(lexer, false);
  assert(lexer->eof(lexer));
  lexer->mark_end(lexer);
  lexer->result_symbol = 0;
  return true;
}

static bool unexpected_keyword(TSLexer *lexer, TSStateId state) {
  (void)lexer;
  (void)state;
  assert(false);
  return false;
}

void sq_test_external_lexer(void) {
  TSLanguage external = language;
  const TSLexerMode modes[] = {{.external_lex_state = 1}, {.external_lex_state = 1}, {0}, {0}};
  external.lex_modes = modes;
  external.external_token_count = 1;
  external.external_scanner.states = (const bool[]){false, true};
  external.external_scanner.symbol_map = (const TSSymbol[]){1};
  external.external_scanner.scan = scanner_empty;
  external.external_scanner.serialize = scanner_stateless_serialize;
  external.external_scanner.deserialize = scanner_stateless_deserialize;
  external.keyword_capture_token = 1;
  external.keyword_lex_fn = unexpected_keyword;
  const char *message;
  TFLanguage *prepared = tf_language_load_parser(&external, &message);
  assert(prepared);
  // A stateless zero-width token can advance the parse state, even at EOF.
  TFError error;
  assert(tf_parse(prepared, "", 0, NULL, NULL, &error));
  TFParser *parser = tf_parser_new();
  assert(parser);
  TFLanguage *internal = tf_language_load_parser(&language, &message);
  assert(internal);
  for (unsigned iteration = 0; iteration < 2; iteration++) {
    assert(tf_parser_parse(parser, prepared, "", 0, NULL, NULL, &error));
    assert(tf_parser_parse(parser, internal, "\nx", 2, NULL, NULL, &error));
    tf_parser_drop_scratch(parser);
  }
  tf_language_free(internal);
  tf_parser_delete(parser);
  tf_language_free(prepared);

  // Zero-width extras are rejected unless their serialized state changes.
  TSParseActionEntry actions[7];
  memcpy(actions, language.parse_actions, sizeof(actions));
  actions[2].action.shift.extra = true;
  external.parse_actions = actions;
  external.keyword_capture_token = 0;
  prepared = tf_language_load_parser(&external, &message);
  assert(prepared);
  TFLexer lexer;
  TFToken token;
  TFScanner scanner = {0};
  tf_lexer_init(&lexer, prepared, "x", 1);
  lexer.scanner = &scanner;
  assert(tf_lexer_next(&lexer, 1, &token));
  assert(token.end_byte == 1 && !scanner.token_external);
  tf_language_free(prepared);

  external.external_scanner.serialize = scanner_serialize;
  external.external_scanner.deserialize = scanner_deserialize;
  prepared = tf_language_load_parser(&external, &message);
  assert(prepared);
  ScannerFixture payload = {0};
  scanner = (TFScanner){.payload = &payload};
  tf_lexer_init(&lexer, prepared, "x", 1);
  lexer.scanner = &scanner;
  for (unsigned count = 1; count <= 2; count++) {
    assert(tf_lexer_next(&lexer, 1, &token));
    assert(token.end_byte == 0 && scanner.token_external && payload.count == count);
  }
  assert(tf_lexer_next(&lexer, 1, &token));
  assert(token.end_byte == 1 && !scanner.token_external);
  tf_language_free(prepared);

  // Column callbacks count codepoints, omit the BOM, and may invalidate chunks.
  external = language;
  external.lex_modes = modes;
  external.external_token_count = 1;
  external.external_scanner.states = (const bool[]){false, true};
  external.external_scanner.symbol_map = (const TSSymbol[]){1};
  external.external_scanner.scan = scanner_column;
  external.external_scanner.serialize = scanner_stateless_serialize;
  external.external_scanner.deserialize = scanner_stateless_deserialize;
  prepared = tf_language_load_parser(&external, &message);
  assert(prepared);
  const char source[] = "\xef\xbb\xbf" "π😀 x";
  assert(tf_parse(prepared, source, sizeof(source) - 1, NULL, NULL, &error));
  for (unsigned chunk_size = 1; chunk_size <= 8; chunk_size++) {
    ChunkInput chunks = {.source = source, .size = sizeof(source) - 1, .chunk_size = chunk_size};
    assert(tf_parse_with_callback(prepared, (TFInput){&chunks, read_chunk}, NULL, NULL, &error));
  }
  tf_language_free(prepared);
}

void sq_test_chunked_lexer(void) {
  // Includes split BOM/codepoints, embedded NUL, invalid sequences, and truncated EOF.
  const char source[] = "\xef\xbb\xbf" "a\xcf\x80\xf0\x9f\x98\x80\n"
                        "\xe2\n\xa0\0\xc0\xaf\xed\xa0\x80\xf4\x90\x80\x80\xf0\x9f";
  ChunkInput chunks = {.source = source, .size = sizeof(source) - 1};
  for (chunks.chunk_size = 1; chunks.chunk_size <= sizeof(chunks.buffer); chunks.chunk_size++) {
    TFInputState input = {.input = {&chunks, read_chunk}};
    TFLexer contiguous, chunked;
    tf_lexer_init(&contiguous, NULL, source, chunks.size);
    tf_lexer_init_with_callback(&chunked, NULL, &input);
    for (;;) {
      assert(contiguous.byte == chunked.byte);
      assert(contiguous.point.row == chunked.point.row);
      assert(contiguous.point.column == chunked.point.column);
      assert(contiguous.data.lookahead == chunked.data.lookahead);
      assert(contiguous.lookahead_size == chunked.lookahead_size);
      assert(contiguous.data.get_column(&contiguous.data) == chunked.data.get_column(&chunked.data));
      assert(contiguous.data.eof(&contiguous.data) == chunked.data.eof(&chunked.data));
      if (contiguous.data.eof(&contiguous.data)) break;
      contiguous.data.advance(&contiguous.data, false);
      chunked.data.advance(&chunked.data, false);
    }
    // EOF and column rescans must not prevent subsequent backward seeks.
    for (uint32_t byte = chunks.size; byte > 0;) {
      byte--;
      TFPoint point = {0};
      for (uint32_t index = 0; index < byte; index++) {
        if (source[index] == '\n') point.row++, point.column = 0;
        else point.column++;
      }
      tf_lexer_seek(&contiguous, byte, point);
      tf_lexer_seek(&chunked, byte, point);
      assert(contiguous.data.lookahead == chunked.data.lookahead);
      assert(contiguous.lookahead_size == chunked.lookahead_size);
    }
  }

  const char *message;
  TFLanguage *prepared = tf_language_load(&language, &message);
  assert(prepared);
  TFError error;
  assert(!tf_parse_with_callback(prepared, (TFInput){NULL, overflow_chunk}, NULL, NULL, &error));
  assert(!strcmp(error.message, "input is larger than 4 GiB"));
  tf_language_free(prepared);

  // Associativity ties force materialization of an inherited expression.
  // The private replay must refetch the outer lexer's overwritten input buffer.
  prepared = tf_language_load_parser(&ambiguous_language, &message);
  assert(prepared);
  chunks = (ChunkInput){.source = "x + x + x + x\n", .size = 14, .chunk_size = 1};
  assert(tf_parse(prepared, chunks.source, chunks.size, NULL, NULL, &error));
  assert(tf_parse_with_callback(prepared, (TFInput){&chunks, read_chunk}, NULL, NULL, &error));
  assert(chunks.replays > 0);
  tf_language_free(prepared);
}

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
  ChunkInput chunks = {.source = "\nx", .size = 2, .chunk_size = 1};
  TFInputState input = {.input = {&chunks, read_chunk}};
  tf_lexer_init_with_callback(&lexer, prepared, &input);
  assert(tf_lexer_next(&lexer, 1, &token));
  assert(token.start_byte == 0 && token.end_byte == 2);
  assert(token.start_point.row == 0 && token.end_point.row == 1);
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
  for (unsigned variant = 0; variant < 2; variant++) {
    TSLanguage unsupported = language;
    if (variant == 0) unsupported.abi_version = 14;
    if (variant == 1) unsupported.external_token_count = 1;
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
