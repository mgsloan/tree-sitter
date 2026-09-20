#ifndef SQUAT_NATIVE_INTERNAL_H_
#define SQUAT_NATIVE_INTERNAL_H_
#include <tree_sitter/api.h>
#include "language.h"
#include <limits.h>
#include <stddef.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#define SQ_NONE UINT32_MAX
static inline uint8_t sq_native_width(uint32_t maximum) {
  uint8_t width = 0;
  while (maximum) { width++; maximum >>= 1; }
  return width;
}
typedef enum { SQ_OK, SQ_ERROR_ARGUMENT, SQ_ERROR_ALLOCATION, SQ_ERROR_OVERFLOW,
 SQ_ERROR_DICTIONARY_FULL, SQ_ERROR_INVALID_SLAB, SQ_ERROR_LANGUAGE, SQ_ERROR_PARSE } SQError;
const char *sq_native_error_string(SQError);
typedef struct SQGrammar SQGrammar;
typedef struct {
  const TSLanguage *language;
  const char *const *symbol_names, *const *field_names;
  const uint8_t *symbol_flags;
  const uint16_t *public_symbols, *supertypes, *supertype_indexes;
  const uint64_t *supertype_masks;
  const uint16_t *grammar_ids, *default_codes, *counts, *defaults, *grammar_codes;
  uint32_t symbol_count, field_count, supertype_count, dictionary_count, dictionary_words;
  uint32_t encoding, dictionary_length;
  uint8_t symbol_shift, separate;
} SQGrammarView;
typedef struct SQSupertypeGrammar {
  const TSLanguage *language;
  uint64_t *masks;
  uint32_t *table;
  uint32_t count, words, supertype_count, table_capacity;
} SQSupertypeGrammar;

SQSupertypeGrammar *sq_native_supertype_grammar_new(const TSLanguage *, uint32_t, SQError *);
SQSupertypeGrammar *sq_native_supertype_grammar_new_cached(const TSLanguage *, uint32_t,
                                                  const void *, size_t, SQError *);
void sq_native_supertype_grammar_delete(SQSupertypeGrammar *);
uint32_t sq_native_supertype_mask_id(const SQSupertypeGrammar *, const uint64_t *);
size_t sq_native_supertype_grammar_cache_size(const SQSupertypeGrammar *);
bool sq_native_supertype_grammar_copy_cache(const SQSupertypeGrammar *, void *, size_t, SQError *);

typedef struct {
  uint32_t offset, length;
} DirectFieldSlice;

// Codes place the public display ID immediately above a grammar-wide variant.
// Dictionaries are shared by trees. Zero selects the unique display default
// in global mode; local mode indexes the dictionary by the whole code.
typedef enum { SQ_SYMBOL_LOCAL, SQ_SYMBOL_GLOBAL, SQ_SYMBOL_BYTES } SQSymbolEncoding;

typedef struct {
  uint16_t *grammar_ids; // local code or global selector -> original ID
  uint16_t *default_codes; // original ID -> unaliased code
  uint16_t *counts; // variants per public display ID
  uint16_t *defaults, *grammar_codes; // global: unique display default, original ID -> selector
  SQSymbolEncoding encoding;
  uint32_t length;
  uint8_t shift;
  bool separate;
} SQSymbolTable;

bool sq_native_symbol_table_init(const TSLanguage *, SQSymbolTable *, SQError *);
void sq_native_symbol_table_delete(SQSymbolTable *);
uint32_t sq_native_symbol_code(const SQGrammar *, uint32_t display, uint32_t original);

struct SQGrammar {
  SQGrammarView view;
  uint8_t *symbol_flags;
  atomic_size_t references;
  _Atomic(struct TFLanguage *) direct_language;
  const TSLanguage *language;
  TSSymbol *supertypes;
  uint32_t supertype_count;
  uint16_t *supertype_indexes, *public_index;
  SQSymbolTable symbols;
  DirectFieldSlice *production_fields;
  TSFieldId *direct_fields;
  SQSupertypeGrammar *supertype_grammar;
};


SQGrammar *sq_native_grammar_new(const TSLanguage *, SQError *);
SQGrammar *sq_native_grammar_new_with_cache(const TSLanguage *, const void *, size_t, SQError *);
SQGrammar *sq_native_grammar_copy(SQGrammar *);
void sq_native_grammar_delete(SQGrammar *);
const TSLanguage *sq_native_grammar_language(const SQGrammar *);
uint32_t sq_native_grammar_cache_size(const SQGrammar *);
bool sq_native_grammar_copy_cache(const SQGrammar *, void *, size_t, SQError *);
bool sq_native_language_compatible(const TSLanguage *);
static inline bool sq_native_little_endian(void) {
  const uint16_t one = 1;
  return *(const uint8_t *)&one != 0;
}

static inline uint8_t sq_native_get_u8(const uint8_t *data, uint32_t offset, uint32_t index) {
  return data[offset + (uint64_t)index];
}

static inline void sq_native_set_u8(uint8_t *data, uint32_t offset, uint32_t index, uint8_t value) {
  data[offset + (uint64_t)index] = value;
}

static inline uint16_t sq_native_get_u16(const uint8_t *data, uint32_t offset, uint32_t index) {
  uint16_t value;
  memcpy(&value, data + offset + (uint64_t)index * 2, sizeof(value));
  return sq_native_little_endian() ? value : __builtin_bswap16(value);
}

static inline void sq_native_set_u16(uint8_t *data, uint32_t offset, uint32_t index, uint16_t value) {
  if (!sq_native_little_endian()) value = __builtin_bswap16(value);
  memcpy(data + offset + (uint64_t)index * 2, &value, sizeof(value));
}

static inline uint32_t sq_native_get_u32(const uint8_t *data, uint32_t offset, uint32_t index) {
  uint32_t value;
  memcpy(&value, data + offset + (uint64_t)index * 4, sizeof(value));
  return sq_native_little_endian() ? value : __builtin_bswap32(value);
}

static inline void sq_native_set_u32(uint8_t *data, uint32_t offset, uint32_t index, uint32_t value) {
  if (!sq_native_little_endian()) value = __builtin_bswap32(value);
  memcpy(data + offset + (uint64_t)index * 4, &value, sizeof(value));
}

static inline uint64_t sq_native_get_u64(const uint8_t *data, uint32_t offset, uint32_t index) {
  uint64_t value;
  memcpy(&value, data + offset + (uint64_t)index * 8, sizeof(value));
  return sq_native_little_endian() ? value : __builtin_bswap64(value);
}

static inline void sq_native_set_u64(uint8_t *data, uint32_t offset, uint32_t index, uint64_t value) {
  if (!sq_native_little_endian()) value = __builtin_bswap64(value);
  memcpy(data + offset + (uint64_t)index * 8, &value, sizeof(value));
}

static inline void sq_native_fail(SQError *error, SQError value) {
  if (error) {
    *error = value;
  }
}
#endif
