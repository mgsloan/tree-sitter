#include "internal.h"
#include <tree_feller.h>

// Stream generated table contents without hashing pointers or structure padding.
void sq_native_language_table_bytes(const TSLanguage *language,
                                    void (*visit)(const void *, size_t, void *), void *context) {
#define VALUE(value) visit(&(value), sizeof(value), context)
#define ARRAY(pointer, count) do { \
  uint64_t length = (uint64_t)(count); \
  VALUE(length); \
  if ((pointer) && length) visit((pointer), (size_t)length * sizeof(*(pointer)), context); \
} while (0)
  VALUE(language->abi_version);
  VALUE(language->symbol_count);
  VALUE(language->alias_count);
  VALUE(language->token_count);
  VALUE(language->external_token_count);
  VALUE(language->state_count);
  VALUE(language->large_state_count);
  VALUE(language->production_id_count);
  VALUE(language->field_count);
  VALUE(language->max_alias_sequence_length);

  uint32_t symbols = language->symbol_count + language->alias_count;
  for (uint32_t index = 0; index < symbols; index++) {
    const char *name = language->symbol_names[index];
    uint64_t length = strlen(name);
    VALUE(length);
    visit(name, length, context);
    const TSSymbolMetadata *metadata = &language->symbol_metadata[index];
    VALUE(metadata->visible);
    VALUE(metadata->named);
    VALUE(metadata->supertype);
  }
  for (uint32_t index = 1; index <= language->field_count; index++) {
    const char *name = language->field_names[index];
    uint64_t length = strlen(name);
    VALUE(length);
    visit(name, length, context);
  }
  ARRAY(language->public_symbol_map, symbols);
  ARRAY(language->parse_table, (size_t)language->large_state_count * language->symbol_count);

  uint32_t action_end = language->parse_actions[0].entry.count + 1;
  for (uint32_t state = 0; state < language->large_state_count; state++) {
    const uint16_t *row = language->parse_table + (size_t)state * language->symbol_count;
    for (uint32_t symbol = 0; symbol < language->token_count; symbol++) {
      uint32_t index = row[symbol];
      uint32_t end = index + language->parse_actions[index].entry.count + 1;
      if (end > action_end) action_end = end;
    }
  }
  uint32_t small_end = 0;
  if (language->small_parse_table_map) {
    uint32_t small_count = language->state_count - language->large_state_count;
    ARRAY(language->small_parse_table_map, small_count);
    for (uint32_t state = 0; state < small_count; state++) {
      uint32_t offset = language->small_parse_table_map[state];
      uint32_t groups = language->small_parse_table[offset++];
      for (uint32_t group = 0; group < groups; group++) {
        uint32_t value = language->small_parse_table[offset++];
        uint32_t count = language->small_parse_table[offset++];
        for (uint32_t index = 0; index < count; index++) {
          uint16_t symbol = language->small_parse_table[offset++];
          if (symbol < language->token_count) {
            uint32_t end = value + language->parse_actions[value].entry.count + 1;
            if (end > action_end) action_end = end;
          }
        }
      }
      if (offset > small_end) small_end = offset;
    }
  } else {
    uint32_t zero = 0;
    VALUE(zero);
  }
  ARRAY(language->small_parse_table, small_end);
  for (uint32_t index = 0; index < action_end;) {
    const TSParseActionEntry *entry = &language->parse_actions[index];
    VALUE(entry->entry.count);
    VALUE(entry->entry.reusable);
    for (uint32_t action = 1; action <= entry->entry.count; action++) {
      const TSParseAction *item = &language->parse_actions[index + action].action;
      VALUE(item->type);
      if (item->type == TSParseActionTypeShift) {
        VALUE(item->shift.state);
        VALUE(item->shift.extra);
        VALUE(item->shift.repetition);
      } else if (item->type == TSParseActionTypeReduce) {
        VALUE(item->reduce.child_count);
        VALUE(item->reduce.symbol);
        VALUE(item->reduce.dynamic_precedence);
        VALUE(item->reduce.production_id);
      }
    }
    index += entry->entry.count + 1;
  }

  uint32_t field_end = 0;
  if (language->field_map_slices) {
    for (uint32_t index = 0; index < language->production_id_count; index++) {
      TSMapSlice slice = language->field_map_slices[index];
      VALUE(slice.index);
      VALUE(slice.length);
      if (slice.index + slice.length > field_end) field_end = slice.index + slice.length;
    }
  }
  for (uint32_t index = 0; index < field_end; index++) {
    TSFieldMapEntry entry = language->field_map_entries[index];
    VALUE(entry.field_id);
    VALUE(entry.child_index);
    VALUE(entry.inherited);
  }
  ARRAY(language->alias_sequences,
        (size_t)language->production_id_count * language->max_alias_sequence_length);
  if (language->alias_map) {
    uint32_t index = 0;
    while (language->alias_map[index]) {
      uint32_t count = language->alias_map[index + 1];
      index += count + 2;
    }
    ARRAY(language->alias_map, index + 1);
  }
  if (language->abi_version >= LANGUAGE_VERSION_WITH_PRIMARY_STATES) {
    ARRAY(language->primary_state_ids, language->state_count);
  }
  VALUE(language->keyword_capture_token);

  uint32_t external_state_count = 0;
  uint32_t reserved_set_count = 0;
  for (uint32_t index = 0; index < language->state_count; index++) {
    const TSLexMode *mode = &((const TSLexMode *)language->lex_modes)[index];
    VALUE(mode->lex_state);
    VALUE(mode->external_lex_state);
    uint32_t external_end = (uint32_t)mode->external_lex_state + 1;
    if (external_end > external_state_count) external_state_count = external_end;
    if (language->abi_version >= 15) {
      const TSLexerMode *mode = &language->lex_modes[index];
      VALUE(mode->reserved_word_set_id);
      uint32_t reserved_end = (uint32_t)mode->reserved_word_set_id + 1;
      if (reserved_end > reserved_set_count) reserved_set_count = reserved_end;
    }
  }
  if (language->external_token_count) {
    ARRAY(language->external_scanner.states,
          (size_t)external_state_count * language->external_token_count);
    ARRAY(language->external_scanner.symbol_map, language->external_token_count);
  }
  if (language->abi_version >= 15) {
    VALUE(language->max_reserved_word_set_size);
    VALUE(language->supertype_count);
    ARRAY(language->reserved_words,
          (size_t)reserved_set_count * language->max_reserved_word_set_size);
    ARRAY(language->supertype_symbols, language->supertype_count);
    uint32_t supertype_end = 0;
    if (language->supertype_map_slices) {
      uint32_t slice_count = 0;
      for (uint32_t index = 0; index < language->supertype_count; index++) {
        uint32_t end = language->supertype_symbols[index] + 1;
        if (end > slice_count) slice_count = end;
      }
      for (uint32_t index = 0; index < slice_count; index++) {
        TSMapSlice slice = language->supertype_map_slices[index];
        VALUE(slice.index);
        VALUE(slice.length);
        if (slice.index + slice.length > supertype_end) supertype_end = slice.index + slice.length;
      }
    }
    ARRAY(language->supertype_map_entries, supertype_end);
  }
#undef ARRAY
#undef VALUE
}

// Require a supported private language layout and room for the two built-in error IDs.
bool sq_native_language_compatible(const TSLanguage *language) {
  return language && language->abi_version >= TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION &&
         language->abi_version <= TREE_SITTER_LANGUAGE_VERSION &&
         (uint64_t)language->symbol_count + language->alias_count <= ts_builtin_sym_error_repeat;
}

// Prepare shared symbol, field, and supertype tables, then publish their borrowed view.
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
  // All four tables have the same lifetime. Share their allocation and collect
  // flags during the existing metadata walk, avoiding a second language-API pass.
  grammar->supertypes = calloc(space, 3 * sizeof(uint16_t) + sizeof(uint8_t));
  if (!grammar->supertypes) {
    sq_native_grammar_delete(grammar);
    goto allocation;
  }
  grammar->supertype_indexes = grammar->supertypes + space;
  grammar->public_index = grammar->supertypes + 2 * space;
  grammar->symbol_flags = (uint8_t *)(grammar->supertypes + 3 * space);

  for (uint32_t symbol = 0; symbol < symbols; symbol++) {
    TSSymbolMetadata metadata = language->symbol_metadata[symbol];
    grammar->symbol_flags[symbol] =
        metadata.named | (metadata.visible << 1) | (metadata.supertype << 2);
    if (metadata.supertype) {
      grammar->supertypes[grammar->supertype_count++] = (TSSymbol)symbol;
      grammar->supertype_indexes[symbol] = (uint16_t)grammar->supertype_count;
    }

    TSSymbol public = ts_language_public_symbol(language, (TSSymbol)symbol);
    grammar->public_index[symbol] = public == ts_builtin_sym_error          ? symbols
                                    : public == ts_builtin_sym_error_repeat ? symbols + 1
                                                                            : public;
  }

  // Up to eight supertypes fit directly in the event code, without a dictionary.
  if (grammar->supertype_count > 8) {
    grammar->supertype_grammar =
        grammar_cache
            ? sq_native_supertype_grammar_new_cached(language, grammar->supertype_count,
                                                     grammar_cache, grammar_cache_length, error)
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

  // Flatten direct fields by production so traversal can index them by child.
  // Inherited fields are carried through hidden nodes during traversal instead.
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
      if (total > UINT32_MAX || total > SIZE_MAX / sizeof(TSFieldId)) goto grammar_allocation;
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

  // Error symbols live outside the grammar's metadata arrays.
  for (uint32_t symbol = symbols; symbol < space; symbol++) {
    TSSymbol actual = symbol == symbols ? ts_builtin_sym_error : ts_builtin_sym_error_repeat;
    TSSymbolMetadata metadata = ts_language_symbol_metadata(language, actual);
    grammar->symbol_flags[symbol] =
        metadata.named | (metadata.visible << 1) | (metadata.supertype << 2);
  }

  grammar->view = (SQGrammarView){
      .language = grammar->language,
      .symbol_names = language->symbol_names,
      .field_names = language->field_names,
      .symbol_flags = grammar->symbol_flags,
      .public_symbols = grammar->public_index,
      .supertypes = grammar->supertypes,
      .supertype_indexes = grammar->supertype_indexes,
      .supertype_masks = grammar->supertype_grammar ? grammar->supertype_grammar->masks : NULL,
      .grammar_ids = grammar->symbols.grammar_ids,
      .default_codes = grammar->symbols.default_codes,
      .counts = grammar->symbols.counts,
      .defaults = grammar->symbols.defaults,
      .grammar_codes = grammar->symbols.grammar_codes,
      .symbol_count = symbols,
      .grammar_symbol_count = language->symbol_count,
      .field_count = language->field_count,
      .supertype_count = grammar->supertype_count,
      .dictionary_count = grammar->supertype_grammar ? grammar->supertype_grammar->count : 0,
      .dictionary_words = grammar->supertype_grammar ? grammar->supertype_grammar->words : 0,
      .encoding = grammar->symbols.encoding,
      .dictionary_length = grammar->symbols.length,
      .symbol_shift = grammar->symbols.shift,
      .separate = grammar->symbols.separate,
      .production_fields = grammar->production_fields,
      .direct_fields = grammar->direct_fields,
      .alias_sequences = language->alias_sequences,
      .max_alias_sequence_length = language->max_alias_sequence_length,
      .supertype_table = grammar->supertype_grammar ? grammar->supertype_grammar->table : NULL,
      .supertype_table_capacity =
          grammar->supertype_grammar ? grammar->supertype_grammar->table_capacity : 0,
  };
  return grammar;

grammar_allocation:
  sq_native_grammar_delete(grammar);

allocation:
  sq_native_fail(error, SQ_ERROR_ALLOCATION);
  return NULL;
}

// Prepare a grammar by deriving its supertype dictionary from the parse tables.
SQGrammar *sq_native_grammar_new(const TSLanguage *language, SQError *error) {
  return grammar_new(language, NULL, 0, error);
}

// Copy a serialized supertype dictionary; no pointer into bytes is retained.
SQGrammar *sq_native_grammar_new_with_cache(const TSLanguage *language, const void *bytes,
                                            size_t length, SQError *error) {
  if (!bytes) {
    sq_native_fail(error, SQ_ERROR_INVALID_SLAB);
    return NULL;
  }

  return grammar_new(language, bytes, length, error);
}

// Retain the shared owner. Abort before reference-count overflow could free it early.
SQGrammar *sq_native_grammar_copy(SQGrammar *grammar) {
  if (grammar &&
      atomic_fetch_add_explicit(&grammar->references, 1, memory_order_relaxed) >= SIZE_MAX / 2)
    abort();

  return grammar;
}

// Release all prepared tables only when the last user drops its reference.
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
  free(grammar);
}

// Return metadata borrowed for the retained grammar's lifetime.
const SQGrammarView *sq_native_grammar_view(const SQGrammar *grammar) {
  return &grammar->view;
}

// Borrow the retained Tree-sitter language, including its private parse tables.
const TSLanguage *sq_native_grammar_language(const SQGrammar *grammar) {
  return grammar ? grammar->language : NULL;
}

// Zero means there is no dictionary, or its serialized size cannot fit the API.
uint32_t sq_native_grammar_cache_size(const SQGrammar *grammar) {
  size_t size = grammar ? sq_native_supertype_grammar_cache_size(grammar->supertype_grammar) : 0;
  return size <= UINT32_MAX ? (uint32_t)size : 0;
}

// Serialize into an exactly sized caller buffer; the grammar retains its dictionary.
bool sq_native_grammar_copy_cache(const SQGrammar *grammar, void *destination, size_t length,
                                  SQError *error) {
  if (!grammar) {
    sq_native_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }

  return sq_native_supertype_grammar_copy_cache(grammar->supertype_grammar, destination, length,
                                                error);
}

// static messages shared by native failures and direct-parser diagnostics
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
