#include "internal.h"
#include "reductions.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <tree_feller.h>

// owns a retained grammar, parser scratch, and the most recent reduction arena
// Traversal borrows the arena, so parsing, clearing, or trimming must wait for it to end.
struct SQParser {
  SQGrammar *grammar;
  const TFLanguage *language;
  TFParser *feller;

  uint32_t root;
  SQReduction *reductions;
  uint32_t count, capacity;

  SQParseError failure;
};

// Copy diagnostics into caller-owned storage; success also clears any old message.
static void parse_error(SQParseError *error, SQError code, uint32_t byte, TSPoint point,
                        const char *message) {
  if (!error) return;

  *error = (SQParseError){.code = code, .byte = byte, .point = point};
  if (code != SQ_OK) {
    snprintf(error->message, sizeof(error->message), "%s",
             message ? message : sq_native_error_string(code));
  }
}

// Preserve byte-based columns when crossing the two parsers' point types.
static TSPoint point_from_feller(TFPoint point) {
  return (TSPoint){point.row, point.column};
}

// Grow without invalidating integer child handles; record failures at the current reduction.
static bool grow_reductions(SQParser *parser, uint32_t byte, TSPoint point) {
  uint64_t capacity = parser->capacity ? (uint64_t)parser->capacity * 2 : 256;
  uint64_t limit = SIZE_MAX / sizeof(SQReduction);
  if (limit > UINT32_MAX) limit = UINT32_MAX;
  if (capacity > limit) capacity = limit;
  if (capacity <= parser->count) {
    parse_error(&parser->failure, SQ_ERROR_OVERFLOW, byte, point,
                "reduction arena exceeds addressable memory");
    return false;
  }

  SQReduction *next = realloc(parser->reductions, (size_t)capacity * sizeof(*next));
  if (!next) {
    parse_error(&parser->failure, SQ_ERROR_ALLOCATION, byte, point, NULL);
    return false;
  }

  parser->reductions = next;
  parser->capacity = (uint32_t)capacity;
  return true;
}

// Reserve one uninitialized entry. Later appends may invalidate the returned pointer.
static inline SQReduction *append_reduction(SQParser *parser, uint32_t byte, TSPoint point) {
  if (parser->count == parser->capacity && !grow_reductions(parser, byte, point)) return NULL;
  return &parser->reductions[parser->count++];
}

// Attach children with visible output right to left, resolving aliases and direct fields.
// Return a one-based arena handle; NULL remains reserved for unmaterialized shifted tokens.
static void *reduce(void *payload, const TFReduction *reduction) {
  SQParser *parser = payload;

  // The sink cannot cancel tf_parse. Ignore later values after an allocation
  // failure, since their handles may now be NULL as well as shifted tokens.
  if (parser->failure.code != SQ_OK) return NULL;

  const SQGrammar *grammar = parser->grammar;
  const TSSymbolMetadata *metadata = grammar->language->symbol_metadata;
  const TSSymbol *aliases = ts_language_alias_sequence(grammar->language, reduction->production_id);
  DirectFieldSlice fields = grammar->production_fields
                                ? grammar->production_fields[reduction->production_id]
                                : (DirectFieldSlice){0};

  uint32_t first = SQ_NONE, previous = SQ_NONE;
  uint64_t descendants = 0;
  uint32_t structural = reduction->child_count;

  for (uint32_t offset = 0; offset < reduction->node_count; offset++) {
    uint32_t index = reduction->node_count - offset - 1;
    const TFNode *node = &reduction->children[index];
    structural -= !node->extra;
    TSSymbol alias = !node->extra && aliases ? aliases[structural] : 0;
    TSFieldId field = !node->extra && structural < fields.length
                          ? grammar->direct_fields[fields.offset + structural]
                          : 0;

    // Feller emits grammar symbols only, without recovery's built-in error nodes.
    bool visible = alias || metadata[node->symbol].visible;
    uintptr_t value = (uintptr_t)node->value;
    uint32_t child;
    if (!value) {
      // Shifted tokens already live in feller's stack. Materialize them only
      // after their parent determines visibility, aliases, and direct fields.
      if (!visible) continue;
      SQReduction *entry =
          append_reduction(parser, node->start_byte, point_from_feller(node->start_point));
      if (!entry) return NULL;

      child = parser->count - 1;
      *entry = (SQReduction){
          .first_child = SQ_NONE,
          .next_sibling = SQ_NONE,
          .start_byte = node->start_byte,
          .end_byte = node->end_byte,
          .start_point = point_from_feller(node->start_point),
          .end_point = point_from_feller(node->end_point),
          .symbol = node->symbol,
      };
    } else {
      if (value - 1 >= parser->count) {
        parse_error(&parser->failure, SQ_ERROR_PARSE, reduction->start_byte,
                    point_from_feller(reduction->start_point), "invalid reduction child handle");
        return NULL;
      }

      child = (uint32_t)(value - 1);
      if (!visible && !parser->reductions[child].visible_descendant_count) continue;
    }

    SQReduction *entry = &parser->reductions[child];
    entry->alias = alias;
    entry->field = field;
    entry->extra = node->extra;
    entry->visible = visible;
    entry->next_sibling = SQ_NONE;
    descendants += entry->visible_descendant_count + (uint64_t)visible;

    if (previous == SQ_NONE) first = child;
    else parser->reductions[previous].next_sibling = child;
    previous = child;
  }

  if (descendants >= UINT32_MAX) {
    parse_error(&parser->failure, SQ_ERROR_OVERFLOW, reduction->start_byte,
                point_from_feller(reduction->start_point), "too many visible descendants");
    return NULL;
  }

  SQReduction *entry =
      append_reduction(parser, reduction->start_byte, point_from_feller(reduction->start_point));
  if (!entry) return NULL;

  *entry = (SQReduction){
      .first_child = first,
      .next_sibling = SQ_NONE,
      .start_byte = reduction->start_byte,
      .end_byte = reduction->end_byte,
      .start_point = point_from_feller(reduction->start_point),
      .end_point = point_from_feller(reduction->end_point),
      .symbol = reduction->symbol,
      .visible_descendant_count = (uint32_t)descendants,
  };

  // The accepted root includes leading/trailing extras and EOF's end position.
  // Integer handles survive arena reallocations and are never dereferenced.
  return (void *)(uintptr_t)parser->count;
}

// Retain the grammar and reuse its lazily prepared direct-parser tables across parsers.
SQParser *sq_native_parser_new(SQGrammar *grammar, SQParseError *error) {
  parse_error(error, SQ_OK, 0, (TSPoint){0}, NULL);
  if (!grammar) {
    parse_error(error, SQ_ERROR_ARGUMENT, 0, (TSPoint){0}, NULL);
    return NULL;
  }

  SQParser *parser = calloc(1, sizeof(*parser));
  if (!parser) {
    parse_error(error, SQ_ERROR_ALLOCATION, 0, (TSPoint){0}, NULL);
    return NULL;
  }

  parser->grammar = sq_native_grammar_copy(grammar);
  parser->language = atomic_load_explicit(&grammar->direct_language, memory_order_acquire);
  if (!parser->language) {
    const char *message = NULL;
    TFLanguage *prepared = tf_language_load_parser(grammar->language, &message);
    if (!prepared) {
      SQError code = message && strcmp(message, "out of memory") == 0 ? SQ_ERROR_ALLOCATION
                                                                      : SQ_ERROR_LANGUAGE;
      parse_error(error, code, 0, (TSPoint){0}, message);
      sq_native_parser_delete(parser);
      return NULL;
    }

    // First users may prepare concurrently; retain only the published tables.
    // Allocation failures leave the cache empty so another attempt can retry.
    TFLanguage *existing = NULL;
    if (atomic_compare_exchange_strong_explicit(&grammar->direct_language, &existing, prepared,
                                                memory_order_acq_rel, memory_order_acquire)) {
      parser->language = prepared;
    } else {
      tf_language_free(prepared);
      parser->language = existing;
    }
  }

  parser->feller = tf_parser_new();
  if (!parser->feller) {
    parse_error(error, SQ_ERROR_ALLOCATION, 0, (TSPoint){0}, NULL);
    sq_native_parser_delete(parser);
    return NULL;
  }

  return parser;
}

// Release reduction and parser scratch while retaining the grammar for reuse.
void sq_native_parser_drop_scratch(SQParser *parser) {
  if (!parser) return;

  free(parser->reductions);
  parser->reductions = NULL;
  parser->count = parser->capacity = 0;
  parser->failure = (SQParseError){0};
  tf_parser_drop_scratch(parser->feller);
}

// Release this parser's scratch and grammar reference; NULL is accepted on failure paths.
void sq_native_parser_delete(SQParser *parser) {
  if (!parser) return;

  sq_native_parser_drop_scratch(parser);
  tf_parser_delete(parser->feller);
  sq_native_grammar_delete(parser->grammar);
  free(parser);
}

// Invalidate the current parse result while retaining arena capacity.
void sq_native_parser_clear(SQParser *parser) {
  parser->count = 0;
  parser->root = SQ_NONE;
  parser->failure = (SQParseError){0};
}

// Parse into the reusable arena. Sink failures take precedence over parser diagnostics;
// only an accepted root may be exposed to traversal.
static bool parse(SQParser *parser, const char *source, uint32_t length, const TFInput *input,
                   SQParseError *error) {
  sq_native_parser_clear(parser);
  parse_error(error, SQ_OK, 0, (TSPoint){0}, NULL);

  TFSink sink = {.payload = parser, .on_reduce = reduce};
  TFError diagnostic = {0};
  void *root = NULL;
  bool success = input
      ? tf_parser_parse_with_callback(parser->feller, parser->language, *input, &sink, &root,
                                      &diagnostic)
      : tf_parser_parse(parser->feller, parser->language, source ? source : "", length,
                         &sink, &root, &diagnostic);

  if (parser->failure.code != SQ_OK) {
    *error = parser->failure;
  } else if (!success) {
    SQError code =
        strcmp(diagnostic.message, "out of memory") == 0 ? SQ_ERROR_ALLOCATION : SQ_ERROR_PARSE;
    if (strcmp(diagnostic.message, "input is larger than 4 GiB") == 0) code = SQ_ERROR_OVERFLOW;
    parse_error(error, code, diagnostic.byte, point_from_feller(diagnostic.point),
                diagnostic.message);
  } else if (!root || (uintptr_t)root - 1 >= parser->count) {
    parse_error(error, SQ_ERROR_PARSE, 0, (TSPoint){0}, "parse produced no valid root");
  } else {
    parser->root = (uint32_t)((uintptr_t)root - 1);
    return true;
  }

  sq_native_parser_clear(parser);
  return false;
}

bool sq_native_parser_parse(SQParser *parser, const char *source, uint32_t length,
                            SQParseError *error) {
  return parse(parser, source, length, NULL, error);
}

bool sq_native_parser_parse_with_callback(SQParser *parser, TFInput input, SQParseError *error) {
  return parse(parser, NULL, 0, &input, error);
}

// The Rust parse guard retains the arena and excludes parser mutation.
const SQReduction *sq_native_parser_reductions(const SQParser *parser, uint32_t *count,
                                               uint32_t *root) {
  *count = parser->count;
  *root = parser->root;
  return parser->reductions;
}

SQReduction *sq_native_parser_take_reductions(SQParser *parser, uint32_t *count,
                                             uint32_t *root) {
  *count = parser->count;
  *root = parser->root;
  SQReduction *reductions = parser->reductions;
  parser->reductions = NULL;
  parser->capacity = 0;
  sq_native_parser_clear(parser);
  return reductions;
}

void sq_native_reductions_delete(SQReduction *reductions) {
  free(reductions);
}
