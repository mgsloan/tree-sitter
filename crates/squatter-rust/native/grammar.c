#include "internal.h"
#include <tree_feller.h>

bool sq_native_language_compatible(const TSLanguage *language) {
 return language && language->abi_version >= TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION &&
 language->abi_version <= TREE_SITTER_LANGUAGE_VERSION &&
 (uint64_t)language->symbol_count + language->alias_count <= ts_builtin_sym_error_repeat;
}
static SQGrammar *grammar_new(const TSLanguage *language, const void *grammar_cache,
                               size_t grammar_cache_length, SQError *error) {
  sq_native_fail(error, SQ_OK);
  if (language && ((uint64_t)language->symbol_count + language->alias_count + 1 > UINT16_MAX ||
                   language->field_count > UINT16_MAX)) {
    sq_native_fail(error, SQ_ERROR_OVERFLOW);
    return NULL;
  }
  if (!sq_native_language_compatible(language)) {
    sq_native_fail(error, SQ_ERROR_LANGUAGE);
    return NULL;
  }
  SQGrammar *grammar = calloc(1, sizeof(SQGrammar));
  if (!grammar) goto allocation;
  atomic_init(&grammar->references, 1);
  atomic_init(&grammar->direct_language, NULL);
  grammar->language = ts_language_copy(language);
  if (!sq_native_symbol_table_init(language, &grammar->symbols, error)) {
    sq_native_grammar_delete(grammar);
    return NULL;
  }
  uint32_t symbols = language->symbol_count + language->alias_count;
  size_t space = (size_t)symbols + 2;
  grammar->supertypes = calloc(3 * space, sizeof(uint16_t));
  if (!grammar->supertypes) {
    sq_native_grammar_delete(grammar);
    goto allocation;
  }
  grammar->supertype_indexes = grammar->supertypes + space;
  grammar->public_index = grammar->supertypes + 2 * space;
  for (uint32_t symbol = 0; symbol < symbols; symbol++) {
    if (language->symbol_metadata[symbol].supertype) {
      grammar->supertypes[grammar->supertype_count++] = (TSSymbol)symbol;
      grammar->supertype_indexes[symbol] = (uint16_t)grammar->supertype_count;
    }
    TSSymbol public = ts_language_public_symbol(language, (TSSymbol)symbol);
    grammar->public_index[symbol] = public == ts_builtin_sym_error ? symbols
        : public == ts_builtin_sym_error_repeat ? symbols + 1 : public;
  }
  if (grammar->supertype_count > 8) {
    grammar->supertype_grammar = grammar_cache
        ? sq_native_supertype_grammar_new_cached(language, grammar->supertype_count, grammar_cache,
                                          grammar_cache_length, error)
        : sq_native_supertype_grammar_new(language, grammar->supertype_count, error);
    if (!grammar->supertype_grammar) {
      sq_native_grammar_delete(grammar);
      return NULL;
    }
  } else if (grammar_cache_length) {
    sq_native_grammar_delete(grammar);
    sq_native_fail(error, SQ_ERROR_INVALID_SLAB);
    return NULL;
  }
  grammar->public_index[symbols] = (uint16_t)symbols;
  grammar->public_index[symbols + 1] = (uint16_t)(symbols + 1);
  if (language->field_count && language->production_id_count) {
    grammar->production_fields = calloc(language->production_id_count, sizeof(DirectFieldSlice));
    if (!grammar->production_fields) goto grammar_allocation;
    uint64_t total = 0;
    for (uint32_t id = 0; id < language->production_id_count; id++) {
      const TSFieldMapEntry *map, *end;
      ts_language_field_map(language, id, &map, &end);
      uint32_t length = 0;
      for (; map < end; map++) {
        if (!map->inherited && (uint32_t)map->child_index + 1 > length)
          length = (uint32_t)map->child_index + 1;
      }
      grammar->production_fields[id] = (DirectFieldSlice){(uint32_t)total, length};
      total += length;
      if (total > UINT32_MAX || total > SIZE_MAX / sizeof(TSFieldId))
        goto grammar_allocation;
    }
    if (total) {
      grammar->direct_fields = calloc((size_t)total, sizeof(TSFieldId));
      if (!grammar->direct_fields) goto grammar_allocation;
      for (uint32_t id = 0; id < language->production_id_count; id++) {
        const TSFieldMapEntry *map, *end;
        ts_language_field_map(language, id, &map, &end);
        uint32_t offset = grammar->production_fields[id].offset;
        for (; map < end; map++) {
          if (!map->inherited && !grammar->direct_fields[offset + map->child_index])
            grammar->direct_fields[offset + map->child_index] = map->field_id;
        }
      }
    }
  }
  grammar->symbol_flags = calloc(space, 1);
  if (!grammar->symbol_flags) goto grammar_allocation;
  for (uint32_t symbol = 0; symbol < space; symbol++) {
    TSSymbol actual = symbol == symbols ? ts_builtin_sym_error
      : symbol == symbols + 1 ? ts_builtin_sym_error_repeat : (TSSymbol)symbol;
    TSSymbolMetadata metadata = ts_language_symbol_metadata(language, actual);
    grammar->symbol_flags[symbol] = metadata.named | (metadata.visible << 1) | (metadata.supertype << 2);
  }
  grammar->view = (SQGrammarView){
    .language = grammar->language,
    .symbol_names = language->symbol_names, .field_names = language->field_names,
    .symbol_flags = grammar->symbol_flags, .public_symbols = grammar->public_index,
    .supertypes = grammar->supertypes, .supertype_indexes = grammar->supertype_indexes,
    .supertype_masks = grammar->supertype_grammar ? grammar->supertype_grammar->masks : NULL,
    .grammar_ids = grammar->symbols.grammar_ids, .default_codes = grammar->symbols.default_codes,
    .counts = grammar->symbols.counts, .defaults = grammar->symbols.defaults,
    .grammar_codes = grammar->symbols.grammar_codes,
    .symbol_count = symbols, .field_count = language->field_count,
    .supertype_count = grammar->supertype_count,
    .dictionary_count = grammar->supertype_grammar ? grammar->supertype_grammar->count : 0,
    .dictionary_words = grammar->supertype_grammar ? grammar->supertype_grammar->words : 0,
    .encoding = grammar->symbols.encoding, .dictionary_length = grammar->symbols.length,
    .symbol_shift = grammar->symbols.shift, .separate = grammar->symbols.separate,
  };
  return grammar;
grammar_allocation:
  sq_native_grammar_delete(grammar);
allocation:
  sq_native_fail(error, SQ_ERROR_ALLOCATION);
  return NULL;
}

SQGrammar *sq_native_grammar_new(const TSLanguage *language, SQError *error) {
  return grammar_new(language, NULL, 0, error);
}

SQGrammar *sq_native_grammar_new_with_cache(const TSLanguage *language, const void *bytes,
                                    size_t length, SQError *error) {
  if (!bytes) {
    sq_native_fail(error, SQ_ERROR_INVALID_SLAB);
    return NULL;
  }
  return grammar_new(language, bytes, length, error);
}

SQGrammar *sq_native_grammar_copy(SQGrammar *grammar) {
  if (grammar && atomic_fetch_add_explicit(&grammar->references, 1, memory_order_relaxed) >= SIZE_MAX / 2)
    abort();
  return grammar;
}

void sq_native_grammar_delete(SQGrammar *grammar) {
  if (!grammar || atomic_fetch_sub_explicit(&grammar->references, 1, memory_order_acq_rel) != 1)
    return;
  tf_language_free(atomic_load_explicit(&grammar->direct_language, memory_order_relaxed));
  sq_native_supertype_grammar_delete(grammar->supertype_grammar);
  ts_language_delete(grammar->language);
  sq_native_symbol_table_delete(&grammar->symbols);
  free(grammar->supertypes);
  free(grammar->production_fields);
  free(grammar->direct_fields);
  free(grammar->symbol_flags);
  free(grammar);
}

const SQGrammarView *sq_native_grammar_view(const SQGrammar *grammar) {
  return &grammar->view;
}

const TSLanguage *sq_native_grammar_language(const SQGrammar *grammar) {
  return grammar ? grammar->language : NULL;
}

uint32_t sq_native_grammar_cache_size(const SQGrammar *grammar) {
  size_t size = grammar ? sq_native_supertype_grammar_cache_size(grammar->supertype_grammar) : 0;
  return size <= UINT32_MAX ? (uint32_t)size : 0;
}

bool sq_native_grammar_copy_cache(const SQGrammar *grammar, void *destination,
                           size_t length, SQError *error) {
  if (!grammar) {
    sq_native_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }
  return sq_native_supertype_grammar_copy_cache(grammar->supertype_grammar, destination, length, error);
}

const char *sq_native_error_string(SQError error) {
  switch (error) {
  case SQ_OK:
    return "success";
  case SQ_ERROR_ARGUMENT:
    return "invalid argument";
  case SQ_ERROR_ALLOCATION:
    return "allocation failed";
  case SQ_ERROR_OVERFLOW:
    return "grammar IDs or slab size exceed representation limits";
  case SQ_ERROR_DICTIONARY_FULL:
    return "more than 65536 supertype masks";
  case SQ_ERROR_INVALID_SLAB:
    return "invalid or incompatible slab";
  case SQ_ERROR_PARSE:
    return "parse failed";
  case SQ_ERROR_LANGUAGE:
    return "unsupported language";
  default:
    return "unknown error";
  }
}
