#include "../internal.h"
#include "../../tree_feller/src/tf_lexer.h"
#include <assert.h>
#include <pthread.h>
#include <stdio.h>

static size_t fail_at, allocations;
void *__real_malloc(size_t);
void *__real_calloc(size_t, size_t);
void *__real_realloc(void *, size_t);
static bool fail(void) { return fail_at && ++allocations == fail_at; }
void *__wrap_malloc(size_t size) { return fail() ? NULL : __real_malloc(size); }
void *__wrap_calloc(size_t count, size_t size) {
  return fail() ? NULL : __real_calloc(count, size);
}
void *__wrap_realloc(void *pointer, size_t size) {
  return fail() ? NULL : __real_realloc(pointer, size);
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

static void equal(const SQTree *first, const SQTree *second) {
  assert(first && second);
  uint32_t first_size, second_size;
  const void *first_bytes = sq_tree_data(first, &first_size);
  const void *second_bytes = sq_tree_data(second, &second_size);
  assert(first_size == second_size && !memcmp(first_bytes, second_bytes, first_size));
}

typedef struct {
  SQGrammar *grammar;
  const SQTree *expected;
  SQPackOptions options;
  atomic_uint *ready;
  atomic_bool *start;
  const TFLanguage *prepared;
} ThreadArgument;

static void *parse_thread(void *argument) {
  ThreadArgument *thread = argument;
  atomic_fetch_add(thread->ready, 1);
  while (!atomic_load(thread->start)) {}
  SQParseError error;
  for (unsigned repeat = 0; repeat < 4; repeat++) {
    SQParser *parser = sq_parser_new(thread->grammar, &error);
    assert(parser);
    const TFLanguage *prepared = atomic_load(&thread->grammar->direct_language);
    assert(prepared && (!thread->prepared || thread->prepared == prepared));
    thread->prepared = prepared;
    SQTree *tree = sq_parser_parse(parser, "\nx", 2, thread->options, &error);
    sq_parser_delete(parser);
    equal(tree, thread->expected);
    sq_tree_delete(tree);
  }
  return NULL;
}

static void concurrent_preparation(const SQTree *expected, SQPackOptions options) {
  SQError error;
  SQGrammar *grammar = sq_grammar_new(&language, &error);
  assert(grammar && !atomic_load(&grammar->direct_language));
  atomic_uint ready = 0;
  atomic_bool start = false;
  pthread_t threads[4];
  ThreadArgument arguments[4];
  for (unsigned index = 0; index < 4; index++) {
    arguments[index] = (ThreadArgument){.grammar = sq_grammar_copy(grammar),
        .expected = expected, .options = options, .ready = &ready, .start = &start};
    assert(!pthread_create(&threads[index], NULL, parse_thread, &arguments[index]));
  }
  while (atomic_load(&ready) != 4) {}
  sq_grammar_delete(grammar);
  atomic_store(&start, true);
  for (unsigned index = 0; index < 4; index++) {
    assert(!pthread_join(threads[index], NULL));
    assert(arguments[index].prepared == arguments[0].prepared);
    sq_grammar_delete(arguments[index].grammar);
  }
}

int main(void) {
  SQError error;
  SQParseError diagnostic;
  SQPackOptions options = sq_pack_options_default();
  options.initial_group_capacity = 1;
  options.repack = true;
  SQGrammar *grammar = sq_grammar_new(&language, &error);
  assert(grammar);
  TSParser *mainline = ts_parser_new();
  assert(ts_parser_set_language(mainline, &language));
  TSTree *native = ts_parser_parse_string(mainline, NULL, "\nx", 2);
  assert(native && !ts_node_has_error(ts_tree_root_node(native)));
  SQTree *expected = sq_tree_pack(grammar, native, options, &error);
  assert(expected && sq_node_start_byte(sq_tree_root_node(expected)) == 0);
  assert(!atomic_load(&grammar->direct_language));

  // Fallback must restart at the original position and retain the parse state
  // used for keyword handling and speculative token-cache reuse.
  const char *message;
  TFLanguage *feller = tf_language_load(&language, &message);
  assert(feller);
  TFLexer lexer;
  TFToken token;
  tf_lexer_init(&lexer, feller, "\nx", 2);
  assert(tf_lexer_next(&lexer, 1, &token));
  assert(token.start_byte == 0 && token.end_byte == 2);
  assert(token.start_point.row == 0 && token.end_point.row == 1);
  assert(lexer.token_lex_state == 1);
  tf_language_free(feller);

  for (size_t attempt = 1;; attempt++) {
    assert(attempt < 128);
    SQGrammar *fresh = sq_grammar_new(&language, &error);
    assert(fresh);
    allocations = 0;
    fail_at = attempt;
    SQParser *parser = sq_parser_new(fresh, &diagnostic);
    fail_at = 0;
    if (parser) {
      sq_parser_delete(parser);
      sq_grammar_delete(fresh);
      break;
    }
    assert(diagnostic.code == SQ_ERROR_ALLOCATION && diagnostic.message[0]);
    parser = sq_parser_new(fresh, &diagnostic);
    assert(parser);
    sq_grammar_delete(fresh);
    sq_parser_delete(parser);
  }

  SQParser *parser = sq_parser_new(grammar, &diagnostic);
  assert(parser && diagnostic.code == SQ_OK);
  const TFLanguage *prepared = atomic_load(&grammar->direct_language);
  assert(prepared && !prepared->field_at && !prepared->aliasable);
  for (size_t attempt = 1;; attempt++) {
    assert(attempt < 128);
    sq_parser_trim(parser);
    allocations = 0;
    fail_at = attempt;
    SQTree *tree = sq_parser_parse(parser, "\nx", 2, options, &diagnostic);
    fail_at = 0;
    bool finished = tree != NULL;
    if (tree) { equal(tree, expected); sq_tree_delete(tree); }
    else assert(diagnostic.code == SQ_ERROR_ALLOCATION && diagnostic.message[0]);
    tree = sq_parser_parse(parser, "\nx", 2, options, &diagnostic);
    equal(tree, expected);
    sq_tree_delete(tree);
    if (finished) break;
  }
  assert(!sq_parser_parse(parser, NULL, 1, options, &diagnostic));
  assert(diagnostic.code == SQ_ERROR_ARGUMENT);
  assert(!sq_parser_parse(parser, "?", 1, options, &diagnostic));
  assert(diagnostic.code == SQ_ERROR_PARSE);
  assert(!sq_parser_parse(parser, NULL, 0, options, &diagnostic));
  assert(diagnostic.code == SQ_ERROR_PARSE);
  SQPackOptions invalid = options;
  invalid.initial_group_capacity = UINT32_MAX;
  assert(!sq_parser_parse(parser, "x", 1, invalid, &diagnostic));
  assert(diagnostic.code == SQ_ERROR_OVERFLOW);
  SQTree *retained = sq_parser_parse(parser, "\nx", 2, options, NULL);
  sq_parser_trim(parser);
  sq_parser_delete(parser);
  assert(atomic_load(&grammar->direct_language) == prepared);
  SQTree *one_shot = sq_tree_parse_direct(grammar, "\nx", 2, options, &diagnostic);
  assert(atomic_load(&grammar->direct_language) == prepared);
  equal(one_shot, expected);
  sq_tree_delete(one_shot);
  assert(!sq_parser_new(NULL, &diagnostic) && diagnostic.code == SQ_ERROR_ARGUMENT);
  assert(!sq_parser_parse(NULL, "", 0, options, &diagnostic));
  assert(diagnostic.code == SQ_ERROR_ARGUMENT);
  sq_parser_trim(NULL);
  sq_parser_delete(NULL);

  for (unsigned variant = 0; variant < 3; variant++) {
    TSLanguage unsupported = language;
    const TSLexerMode nonterminal_modes[] = {{0}, {.lex_state = UINT16_MAX}, {0}, {0}};
    if (variant == 0) unsupported.abi_version = 14;
    if (variant == 1) unsupported.external_token_count = 1;
    if (variant == 2) {
      unsupported.lex_modes = nonterminal_modes;
    }
    SQGrammar *other = sq_grammar_new(&unsupported, &error);
    assert(other);
    assert(!sq_parser_new(other, &diagnostic));
    assert(diagnostic.code == SQ_ERROR_LANGUAGE && diagnostic.message[0]);
    sq_grammar_delete(other);
  }

  concurrent_preparation(expected, options);
  sq_grammar_delete(grammar);
  equal(retained, expected);
  sq_tree_delete(retained);
  sq_tree_delete(expected);
  ts_tree_delete(native);
  ts_parser_delete(mainline);
  puts("ok: direct parsing, lexer fallback, allocation failures, reuse, shared grammar, unsupported grammars");
}
