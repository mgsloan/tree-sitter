// Context lifecycle, exact output, and recovery after every allocation failure.
#include "../internal.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include "language_clone.h"
#include "supertype_fixture.h"

static size_t fail_at, allocations;
void *__real_malloc(size_t);
void *__real_calloc(size_t, size_t);
void *__real_realloc(void *, size_t);
static bool fail(void) { return fail_at && ++allocations == fail_at; }
void *__wrap_malloc(size_t n) { return fail() ? NULL : __real_malloc(n); }
void *__wrap_calloc(size_t n, size_t size) { return fail() ? NULL : __real_calloc(n, size); }
void *__wrap_realloc(void *p, size_t n) { return fail() ? NULL : __real_realloc(p, n); }

static void equal(const SQTree *a, const SQTree *b) {
  assert(a && b);
  uint32_t an, bn;
  const void *ab = sq_tree_data(a, &an), *bb = sq_tree_data(b, &bn);
  assert(an == bn && !memcmp(ab, bb, an));
}

static void grammar_allocation_failures(bool multi_child) {
  SupertypeFixture fixture;
  supertype_fixture(&fixture, 9, true);
  if (multi_child) {
    for (unsigned i = 0; i < 9; i++) fixture.actions[i + 2].action.reduce.child_count = 2;
  }
  for (size_t nth = 1; ; nth++) {
    assert(nth < 256);
    SQError error = SQ_OK;
    allocations = 0;
    fail_at = nth;
    SQSupertypeGrammar *attempt = sq_supertype_grammar_new(&fixture.language, 9, &error);
    fail_at = 0;
    if (attempt) {
      assert(attempt->count == 512);
      sq_supertype_grammar_delete(attempt);
      break;
    }
    assert(error == SQ_ERROR_ALLOCATION);
    // A failed construction must not affect subsequent attempts.
    SQSupertypeGrammar *recovered = sq_supertype_grammar_new(&fixture.language, 9, &error);
    assert(recovered && recovered->count == 512);
    sq_supertype_grammar_delete(recovered);
  }
}

int main(int argc, char **argv) {
  assert(argc >= 4);
  if (getenv("CONTEXT_FAILURES")) {
    grammar_allocation_failures(false);
    grammar_allocation_failures(true);
  }
  SQError error;
  sq_pack_context_delete(NULL);
  sq_pack_context_trim(NULL);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  assert(library);
  const TSLanguage *(*language_fn)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  assert(language_fn);
  const TSLanguage *language = language_fn();
  TSLanguage synthetic = test_clone_language(language);
  TSSymbolMetadata *synthetic_metadata = NULL;
  if (getenv("CONTEXT_SUPERTYPES")) {
    unsigned count = (unsigned)atoi(getenv("CONTEXT_SUPERTYPES"));
    uint32_t symbols = language->symbol_count + language->alias_count;
    assert(count <= symbols);
    synthetic_metadata = malloc(symbols * sizeof(TSSymbolMetadata));
    assert(synthetic_metadata);
    memcpy(synthetic_metadata, language->symbol_metadata, symbols * sizeof(TSSymbolMetadata));
    for (uint32_t i = 0; i < symbols; i++) synthetic_metadata[i].supertype = i < count;
    synthetic.symbol_metadata = synthetic_metadata;
    language = &synthetic;
  }
  SQGrammar *grammar = sq_grammar_new(language, &error);
  assert(grammar);
  SQPackContext *context = sq_pack_context_new(&error);
  assert(context && error == SQ_OK);
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language));
  for (int file_index = 3; file_index < argc; file_index++) {
    FILE *file = fopen(argv[file_index], "rb");
    assert(file && !fseek(file, 0, SEEK_END));
    long length = ftell(file);
    assert(length >= 0 && (unsigned long)length <= UINT32_MAX);
    rewind(file);
    char *source = malloc((size_t)length + 1);
    assert(source && fread(source, 1, length, file) == (size_t)length);
    fclose(file);
    TSTree *parsed = ts_parser_parse_string(parser, NULL, source, (uint32_t)length);
    assert(parsed);
    free(source);
    TSLanguage other_language = test_clone_language(language);
    SQGrammar *other_grammar = sq_grammar_new(&other_language, &error);
    assert(other_grammar);
    SQPackContext *other = sq_pack_context_new(&error);
    assert(other);
    assert(!sq_pack_context_pack(other, other_grammar, parsed, sq_pack_options_default(), &error));
    assert(error == SQ_ERROR_LANGUAGE);
    sq_pack_context_delete(other);
    sq_grammar_delete(other_grammar);
    SQTree *retained = NULL, *reference = NULL;
    for (unsigned variant = 0; variant < 8; variant++) {
      SQPackOptions options = {.initial_group_capacity = variant & 1,
                              .repack = (variant & 2) != 0,
                              .symbol_presence = (variant & 4) != 0,
                              .points = true};
      SQTree *ordinary = sq_tree_pack(grammar, parsed, options, &error);
      SQTree *cached = sq_pack_context_pack(context, grammar, parsed, options, &error);
      assert(error == SQ_OK);
      equal(ordinary, cached);
      if (variant == 0) { retained = cached; reference = ordinary; }
      else { sq_tree_delete(ordinary); sq_tree_delete(cached); }
    }
    sq_pack_context_trim(context);
    equal(retained, reference);
    assert(!sq_pack_context_pack(context, NULL, parsed, sq_pack_options_default(), &error));
    assert(error == SQ_ERROR_ARGUMENT);
    assert(!sq_pack_context_pack(context, grammar, NULL, sq_pack_options_default(), &error));
    assert(error == SQ_ERROR_ARGUMENT);
    assert(!sq_pack_context_pack(NULL, grammar, parsed, sq_pack_options_default(), &error));
    assert(error == SQ_ERROR_ARGUMENT);
    SQPackOptions invalid = {.initial_group_capacity = UINT32_MAX};
    assert(!sq_pack_context_pack(context, grammar, parsed, invalid, &error));
    assert(error == SQ_ERROR_OVERFLOW);

    SQPackOptions options = sq_pack_options_default();
    options.initial_group_capacity = 1;
    options.repack = true;
    SQTree *expected = sq_tree_pack(grammar, parsed, options, &error);
    assert(expected);
    if (getenv("CONTEXT_FAILURES")) {
      for (size_t nth = 1; ; nth++) {
        assert(nth < 256);
        sq_pack_context_trim(context);
        allocations = 0;
        fail_at = nth;
        SQTree *attempt = sq_pack_context_pack(context, grammar, parsed, options, &error);
        fail_at = 0;
        bool finished = attempt != NULL;
        if (attempt) { equal(attempt, expected); sq_tree_delete(attempt); }
        else assert(error == SQ_ERROR_ALLOCATION);
        SQTree *recovered = sq_pack_context_pack(context, grammar, parsed, options, &error);
        equal(recovered, expected);
        sq_tree_delete(recovered);
        if (finished) break;
      }
      for (size_t nth = 1; ; nth++) {
        assert(nth < 256);
        allocations = 0;
        fail_at = nth;
        SQGrammar *attempt = sq_grammar_new(language, &error);
        fail_at = 0;
        if (!attempt) assert(error == SQ_ERROR_ALLOCATION);
        bool finished = attempt != NULL;
        sq_grammar_delete(attempt);
        if (finished) break;
      }
    }
    SQTree *again = sq_pack_context_pack(context, grammar, parsed, options, &error);
    equal(again, expected);
    sq_tree_delete(again);
    TSTree *empty = ts_parser_parse_string(parser, NULL, "", 0);
    assert(empty);
    SQTree *small_reference = sq_tree_pack(grammar, empty, options, &error);
    SQTree *small_cached = sq_pack_context_pack(context, grammar, empty, options, &error);
    equal(small_cached, small_reference);
    sq_tree_delete(small_cached);
    sq_tree_delete(small_reference);
    ts_tree_delete(empty);
    sq_tree_delete(expected);
    sq_pack_context_delete(context);
    equal(retained, reference);
    sq_tree_delete(retained);
    sq_tree_delete(reference);
    context = sq_pack_context_new(&error);
    assert(context);
    ts_tree_delete(parsed);
  }
  sq_pack_context_delete(context);
  ts_parser_delete(parser);
  sq_grammar_delete(grammar);
  free(synthetic_metadata);
  dlclose(library);
  printf("context: %d files passed\n", argc - 3);
}
