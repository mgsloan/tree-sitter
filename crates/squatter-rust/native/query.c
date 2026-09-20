// Compiler extracted from lib/squat/query.c at 0c3f79ab5; execution lives in Rust.
#ifndef _DEFAULT_SOURCE
#define _DEFAULT_SOURCE 1
#endif

#include "query.h"
#include "point.h"
#include "unicode.h"
#include <wctype.h>

#define MAX_STEP_CAPTURE_COUNT 3
#define MAX_NEGATED_FIELD_COUNT 8
#define MAX_STATE_PREDECESSOR_COUNT 256
#define MAX_ANALYSIS_STATE_DEPTH 8
#define MAX_ANALYSIS_ITERATION_COUNT 256

// borrowed UTF-8 query text with one decoded character of lookahead
typedef struct {
  const char *input;
  const char *start;
  const char *end;

  int32_t next;
  uint8_t next_size;
} Stream;

// owns interned strings; slice indexes are stable IDs even when the bytes move
typedef struct {
  Array(char) characters;
  Array(Slice) slices;
} SymbolTable;

// per-pattern capture counts, indexed by capture ID; absent entries mean zero
typedef Array(uint8_t) CaptureQuantifiers;

// maps compiled steps back to source bytes for structural error diagnostics
typedef struct {
  uint32_t byte_offset;
  uint16_t step_index;
} StepOffset;

// hypothetical grammar frame while checking whether query steps can match
typedef struct {
  TSStateId parse_state;
  TSSymbol parent_symbol;
  uint16_t child_index;
  TSFieldId field_id : 15;
  bool done : 1;
} AnalysisStateEntry;

// bounded stack of grammar frames and the current step in a query pattern
typedef struct {
  AnalysisStateEntry stack[MAX_ANALYSIS_STATE_DEPTH];

  uint16_t depth;
  uint16_t step_index;
  TSSymbol root_symbol;
} AnalysisState;

// owns states in analysis_state__compare order; state_pool uses the same container type
typedef Array(AnalysisState *) AnalysisStateSet;

// worklists and scratch for one analysis
// Hitting a limit clears guarantees so execution cannot assume that a step must match.
typedef struct {
  AnalysisStateSet states;
  AnalysisStateSet next_states;
  AnalysisStateSet deeper_states;
  AnalysisStateSet state_pool;

  Array(uint16_t) final_step_indices;
  Array(TSSymbol) finished_parent_symbols;

  bool did_abort;
} QueryAnalysis;

// parse state and production context on a path that can construct a given symbol
typedef struct {
  TSStateId state;
  uint16_t production_id;
  uint8_t child_index : 7;
  bool done : 1;
} AnalysisSubgraphNode;

// grammar paths for one symbol, including states where its construction can start
typedef struct {
  TSSymbol symbol;

  Array(TSStateId) start_states;
  Array(AnalysisSubgraphNode) nodes;
} AnalysisSubgraph;

typedef Array(AnalysisSubgraph) AnalysisSubgraphArray;

// fixed-width rows: count first, then up to MAX_STATE_PREDECESSOR_COUNT predecessors
// Analysis walks these backward from reductions to reconstruct possible children.
typedef struct {
  TSStateId *contents;
} StatePredecessorMap;

// owns the compiled program and retained language
// Rust borrows the finished arrays; step_offsets and string_buffer are compilation
// scratch released before publication.
struct SQQuery {
  SymbolTable captures, predicate_values;
  Array(CaptureQuantifiers) capture_quantifiers;
  Array(NativeView) quantifier_views;

  Array(QueryStep) steps;
  Array(PatternEntry) pattern_map;
  Array(QueryPredicateStep) predicate_steps;
  Array(QueryPattern) patterns;

  Array(StepOffset) step_offsets;
  Array(TSFieldId) negated_fields;
  Array(char) string_buffer;
  Array(TSSymbol) repeat_symbols_with_rootless_patterns;

  const TSLanguage *language;
  uint16_t wildcard_root_pattern_count;
};

// Closing delimiters return to the caller without becoming a syntax error.
static const TSQueryError PARENT_DONE = -1;

static const uint16_t PATTERN_DONE_MARKER = UINT16_MAX;
static const uint16_t NONE = UINT16_MAX;
static const TSSymbol WILDCARD_SYMBOL = 0;

// Consume the current lookahead and decode the next character within the input bounds.
static bool stream_advance(Stream *self) {
  self->input += self->next_size;
  if (self->input < self->end) {
    uint32_t size = ts_decode_utf8((const uint8_t *)self->input,
                                   (uint32_t)(self->end - self->input), &self->next);
    if (size > 0) {
      self->next_size = size;
      return true;
    }
  } else {
    self->next_size = 0;
    self->next = '\0';
  }

  return false;
}

// Rewind lookahead to a source position, usually the start of an invalid construct.
static void stream_reset(Stream *self, const char *input) {
  self->input = input;
  self->next_size = 0;
  stream_advance(self);
}

// Initialize lookahead without copying or requiring a NUL-terminated source string.
static Stream stream_new(const char *string, uint32_t length) {
  Stream self = {
      .next = 0,
      .input = string,
      .start = string,
      .end = string + length,
  };

  stream_advance(&self);
  return self;
}

// Semicolon comments consume the rest of the line and act as whitespace.
static void stream_skip_whitespace(Stream *self) {
  for (;;) {
    if (iswspace(self->next)) {
      stream_advance(self);
    } else if (self->next == ';') {
      // skip over comments
      stream_advance(self);
      while (self->next && self->next != '\n') {
        if (!stream_advance(self)) {
          break;
        }
      }
    } else {
      break;
    }
  }
}

// Dots are allowed within identifiers, but cannot start one because they mark anchors.
static bool stream_is_ident_start(Stream *self) {
  return iswalnum(self->next) || self->next == '_' || self->next == '-';
}

// Consume an identifier, leaving its delimiter in lookahead.
static void stream_scan_identifier(Stream *stream) {
  do {
    stream_advance(stream);
  } while (iswalnum(stream->next) || stream->next == '_' || stream->next == '-' ||
           stream->next == '.');
}

// Diagnostics report byte offsets even though lookahead uses decoded characters.
static uint32_t stream_offset(Stream *self) {
  return (uint32_t)(self->input - self->start);
}

// Compose capture counts with an enclosing repetition.
static TSQuantifier quantifier_mul(TSQuantifier left, TSQuantifier right) {
  switch (left) {
  case TSQuantifierZero:
    return TSQuantifierZero;
  case TSQuantifierZeroOrOne:
    switch (right) {
    case TSQuantifierZero:
      return TSQuantifierZero;
    case TSQuantifierZeroOrOne:
    case TSQuantifierOne:
      return TSQuantifierZeroOrOne;
    case TSQuantifierZeroOrMore:
    case TSQuantifierOneOrMore:
      return TSQuantifierZeroOrMore;
    };
    break;
  case TSQuantifierZeroOrMore:
    switch (right) {
    case TSQuantifierZero:
      return TSQuantifierZero;
    case TSQuantifierZeroOrOne:
    case TSQuantifierZeroOrMore:
    case TSQuantifierOne:
    case TSQuantifierOneOrMore:
      return TSQuantifierZeroOrMore;
    };
    break;
  case TSQuantifierOne:
    return right;
  case TSQuantifierOneOrMore:
    switch (right) {
    case TSQuantifierZero:
      return TSQuantifierZero;
    case TSQuantifierZeroOrOne:
    case TSQuantifierZeroOrMore:
      return TSQuantifierZeroOrMore;
    case TSQuantifierOne:
    case TSQuantifierOneOrMore:
      return TSQuantifierOneOrMore;
    };
    break;
  }

  return TSQuantifierZero; // unreachable for valid quantifiers
}

// Union the possible capture counts from alternative branches.
static TSQuantifier quantifier_join(TSQuantifier left, TSQuantifier right) {
  switch (left) {
  case TSQuantifierZero:
    switch (right) {
    case TSQuantifierZero:
      return TSQuantifierZero;
    case TSQuantifierZeroOrOne:
    case TSQuantifierOne:
      return TSQuantifierZeroOrOne;
    case TSQuantifierZeroOrMore:
    case TSQuantifierOneOrMore:
      return TSQuantifierZeroOrMore;
    };
    break;
  case TSQuantifierZeroOrOne:
    switch (right) {
    case TSQuantifierZero:
    case TSQuantifierZeroOrOne:
    case TSQuantifierOne:
      return TSQuantifierZeroOrOne;
      break;
    case TSQuantifierZeroOrMore:
    case TSQuantifierOneOrMore:
      return TSQuantifierZeroOrMore;
      break;
    };
    break;
  case TSQuantifierZeroOrMore:
    return TSQuantifierZeroOrMore;
  case TSQuantifierOne:
    switch (right) {
    case TSQuantifierZero:
    case TSQuantifierZeroOrOne:
      return TSQuantifierZeroOrOne;
    case TSQuantifierZeroOrMore:
      return TSQuantifierZeroOrMore;
    case TSQuantifierOne:
      return TSQuantifierOne;
    case TSQuantifierOneOrMore:
      return TSQuantifierOneOrMore;
    };
    break;
  case TSQuantifierOneOrMore:
    switch (right) {
    case TSQuantifierZero:
    case TSQuantifierZeroOrOne:
    case TSQuantifierZeroOrMore:
      return TSQuantifierZeroOrMore;
    case TSQuantifierOne:
    case TSQuantifierOneOrMore:
      return TSQuantifierOneOrMore;
    };
    break;
  }

  return TSQuantifierZero; // unreachable for valid quantifiers
}

// Sum capture counts from consecutive patterns in a sequence.
static TSQuantifier quantifier_add(TSQuantifier left, TSQuantifier right) {
  switch (left) {
  case TSQuantifierZero:
    return right;
  case TSQuantifierZeroOrOne:
    switch (right) {
    case TSQuantifierZero:
      return TSQuantifierZeroOrOne;
    case TSQuantifierZeroOrOne:
    case TSQuantifierZeroOrMore:
      return TSQuantifierZeroOrMore;
    case TSQuantifierOne:
    case TSQuantifierOneOrMore:
      return TSQuantifierOneOrMore;
    };
    break;
  case TSQuantifierZeroOrMore:
    switch (right) {
    case TSQuantifierZero:
      return TSQuantifierZeroOrMore;
    case TSQuantifierZeroOrOne:
    case TSQuantifierZeroOrMore:
      return TSQuantifierZeroOrMore;
    case TSQuantifierOne:
    case TSQuantifierOneOrMore:
      return TSQuantifierOneOrMore;
    };
    break;
  case TSQuantifierOne:
    switch (right) {
    case TSQuantifierZero:
      return TSQuantifierOne;
    case TSQuantifierZeroOrOne:
    case TSQuantifierZeroOrMore:
    case TSQuantifierOne:
    case TSQuantifierOneOrMore:
      return TSQuantifierOneOrMore;
    };
    break;
  case TSQuantifierOneOrMore:
    return TSQuantifierOneOrMore;
  }

  return TSQuantifierZero; // unreachable for valid quantifiers
}

// Create new capture quantifiers structure
static CaptureQuantifiers capture_quantifiers_new(void) {
  return (CaptureQuantifiers)array_new();
}

// Delete capture quantifiers structure
static void capture_quantifiers_delete(CaptureQuantifiers *self) {
  array_delete(self);
}

// Clear capture quantifiers structure
static void capture_quantifiers_clear(CaptureQuantifiers *self) {
  array_clear(self);
}

// Replace capture quantifiers with the given quantifiers
static void capture_quantifiers_replace(CaptureQuantifiers *self, CaptureQuantifiers *quantifiers) {
  array_clear(self);
  array_push_all(self, quantifiers);
}

// Captures absent from this pattern have an implicit zero count.
static TSQuantifier capture_quantifier_for_id(const CaptureQuantifiers *self, uint16_t id) {
  return (self->size <= id) ? TSQuantifierZero : (TSQuantifier)*array_get(self, id);
}

// Accumulate a capture's count, filling skipped capture IDs with zeros.
static void capture_quantifiers_add_for_id(CaptureQuantifiers *self, uint16_t id,
                                           TSQuantifier quantifier) {
  if (self->size <= id) {
    array_grow_by(self, id + 1 - self->size);
  }

  uint8_t *own_quantifier = array_get(self, id);
  *own_quantifier = (uint8_t)quantifier_add((TSQuantifier)*own_quantifier, quantifier);
}

// Point-wise add the given quantifiers to the current values
static void capture_quantifiers_add_all(CaptureQuantifiers *self, CaptureQuantifiers *quantifiers) {
  if (self->size < quantifiers->size) {
    array_grow_by(self, quantifiers->size - self->size);
  }

  for (uint16_t id = 0; id < (uint16_t)quantifiers->size; id++) {
    uint8_t *quantifier = array_get(quantifiers, id);
    uint8_t *own_quantifier = array_get(self, id);
    *own_quantifier =
        (uint8_t)quantifier_add((TSQuantifier)*own_quantifier, (TSQuantifier)*quantifier);
  }
}

// Apply an enclosing repetition to every capture in the pattern.
static void capture_quantifiers_mul(CaptureQuantifiers *self, TSQuantifier quantifier) {
  for (uint16_t id = 0; id < (uint16_t)self->size; id++) {
    uint8_t *own_quantifier = array_get(self, id);
    *own_quantifier = (uint8_t)quantifier_mul((TSQuantifier)*own_quantifier, quantifier);
  }
}

// Point-wise join the quantifiers from a list of alternatives with the current values
static void capture_quantifiers_join_all(CaptureQuantifiers *self,
                                         CaptureQuantifiers *quantifiers) {
  if (self->size < quantifiers->size) {
    array_grow_by(self, quantifiers->size - self->size);
  }

  for (uint32_t id = 0; id < quantifiers->size; id++) {
    uint8_t *quantifier = array_get(quantifiers, id);
    uint8_t *own_quantifier = array_get(self, id);
    *own_quantifier =
        (uint8_t)quantifier_join((TSQuantifier)*own_quantifier, (TSQuantifier)*quantifier);
  }

  for (uint32_t id = quantifiers->size; id < self->size; id++) {
    uint8_t *own_quantifier = array_get(self, id);
    *own_quantifier = (uint8_t)quantifier_join((TSQuantifier)*own_quantifier, TSQuantifierZero);
  }
}

// Create an empty string owner; allocations grow as names are interned.
static SymbolTable symbol_table_new(void) {
  return (SymbolTable){
      .characters = array_new(),
      .slices = array_new(),
  };
}

// Release both string bytes and the slices that identify them.
static void symbol_table_delete(SymbolTable *self) {
  array_delete(&self->characters);
  array_delete(&self->slices);
}

// Find an existing name without modifying its ID table; -1 means absent.
static int symbol_table_id_for_name(const SymbolTable *self, const char *name, uint32_t length) {
  for (unsigned i = 0; i < self->slices.size; i++) {
    Slice slice = *array_get(&self->slices, i);
    if (slice.length == length &&
        !strncmp(array_get(&self->characters, slice.offset), name, length)) {
      return i;
    }
  }

  return -1;
}

// Borrow an interned string until the table grows or is deleted; id must be valid.
static const char *symbol_table_name_for_id(const SymbolTable *self, uint16_t id,
                                            uint32_t *length) {
  Slice slice = *(array_get(&self->slices, id));
  *length = slice.length;
  return array_get(&self->characters, slice.offset);
}

// Reuse an existing ID or append a terminated string; slices exclude the terminator.
static uint16_t symbol_table_insert_name(SymbolTable *self, const char *name, uint32_t length) {
  int id = symbol_table_id_for_name(self, name, length);
  if (id >= 0) {
    return (uint16_t)id;
  }

  Slice slice = {
      .offset = self->characters.size,
      .length = length,
  };
  array_grow_by(&self->characters, length + 1);
  memcpy(array_get(&self->characters, slice.offset), name, length);
  *array_get(&self->characters, self->characters.size - 1) = 0;
  array_push(&self->slices, slice);
  return self->slices.size - 1;
}

// Initialize a match step with no captures or alternative transition.
static QueryStep query_step__new(TSSymbol symbol, uint16_t depth, bool is_immediate) {
  QueryStep step = {
      .symbol = symbol,
      .depth = depth,
      .alternative_index = NONE,
      .flags = (is_immediate ? SQ_STEP_IS_IMMEDIATE : 0),
  };
  for (unsigned i = 0; i < MAX_STEP_CAPTURE_COUNT; i++) {
    step.capture_ids[i] = NONE;
  }

  return step;
}

// Append within the fixed capture slots; NONE marks the unused suffix.
static void query_step__add_capture(QueryStep *self, uint16_t capture_id) {
  for (unsigned i = 0; i < MAX_STEP_CAPTURE_COUNT; i++) {
    if (self->capture_ids[i] == NONE) {
      self->capture_ids[i] = capture_id;
      break;
    }
  }
}

// Compact remaining captures so NONE still terminates the occupied prefix.
static void query_step__remove_capture(QueryStep *self, uint16_t capture_id) {
  for (unsigned i = 0; i < MAX_STEP_CAPTURE_COUNT; i++) {
    if (self->capture_ids[i] == capture_id) {
      self->capture_ids[i] = NONE;
      while (i + 1 < MAX_STEP_CAPTURE_COUNT) {
        if (self->capture_ids[i + 1] == NONE) {
          break;
        }

        self->capture_ids[i] = self->capture_ids[i + 1];
        self->capture_ids[i + 1] = NONE;
        i++;
      }

      break;
    }
  }
}

// Allocate one zeroed count-and-predecessor row per parse state.
static inline StatePredecessorMap state_predecessor_map_new(const TSLanguage *language) {
  return (StatePredecessorMap){
      .contents = ts_calloc((size_t)language->state_count * (MAX_STATE_PREDECESSOR_COUNT + 1),
                            sizeof(TSStateId)),
  };
}

// All predecessor rows share one allocation.
static inline void state_predecessor_map_delete(StatePredecessorMap *self) {
  ts_free(self->contents);
}

// Predecessors arrive in state order, so comparing the last entry removes duplicates.
static inline void state_predecessor_map_add(StatePredecessorMap *self, TSStateId state,
                                             TSStateId predecessor) {
  size_t index = (size_t)state * (MAX_STATE_PREDECESSOR_COUNT + 1);
  TSStateId *count = &self->contents[index];

  if (*count == 0 ||
      (*count < MAX_STATE_PREDECESSOR_COUNT && self->contents[index + *count] != predecessor)) {
    (*count)++;
    self->contents[index + *count] = predecessor;
  }
}

// Borrow the populated portion of a row, excluding its leading count.
static inline const TSStateId *state_predecessor_map_get(const StatePredecessorMap *self,
                                                         TSStateId state, unsigned *count) {
  size_t index = (size_t)state * (MAX_STATE_PREDECESSOR_COUNT + 1);
  *count = self->contents[index];
  return &self->contents[index + 1];
}

// Count repeated parent symbols to limit recursive grammar expansion.
static unsigned analysis_state__recursion_depth(const AnalysisState *self) {
  unsigned result = 0;
  for (unsigned i = 0; i < self->depth; i++) {
    TSSymbol symbol = self->stack[i].parent_symbol;
    for (unsigned j = 0; j < i; j++) {
      if (self->stack[j].parent_symbol == symbol) {
        result++;
        break;
      }
    }
  }

  return result;
}

// Order by position in the hypothetical tree, then query step, to merge equivalent work.
static inline int analysis_state__compare(AnalysisState *const *self, AnalysisState *const *other) {
  if ((*self)->depth < (*other)->depth) {
    return 1;
  }

  for (unsigned i = 0; i < (*self)->depth; i++) {
    if (i >= (*other)->depth) {
      return -1;
    }

    AnalysisStateEntry s1 = (*self)->stack[i];
    AnalysisStateEntry s2 = (*other)->stack[i];
    if (s1.child_index < s2.child_index) {
      return -1;
    }

    if (s1.child_index > s2.child_index) {
      return 1;
    }

    if (s1.parent_symbol < s2.parent_symbol) {
      return -1;
    }

    if (s1.parent_symbol > s2.parent_symbol) {
      return 1;
    }

    if (s1.parse_state < s2.parse_state) {
      return -1;
    }

    if (s1.parse_state > s2.parse_state) {
      return 1;
    }

    if (s1.field_id < s2.field_id) {
      return -1;
    }

    if (s1.field_id > s2.field_id) {
      return 1;
    }
  }

  if ((*self)->step_index < (*other)->step_index) {
    return -1;
  }

  if ((*self)->step_index > (*other)->step_index) {
    return 1;
  }

  return 0;
}

// Keep the root entry accessible after its frame is marked complete at depth zero.
static inline AnalysisStateEntry *analysis_state__top(AnalysisState *self) {
  if (self->depth == 0) {
    return &self->stack[0];
  }

  return &self->stack[self->depth - 1];
}

// Hidden ancestor frames supply the supertype context for the current child.
static inline bool analysis_state__has_supertype(AnalysisState *self, TSSymbol symbol) {
  for (unsigned i = 0; i < self->depth; i++) {
    if (self->stack[i].parent_symbol == symbol) {
      return true;
    }
  }

  return false;
}

// Clone into a recycled state allocation when one is available.
static inline AnalysisState *analysis_state_pool__clone_or_reuse(AnalysisStateSet *self,
                                                                 AnalysisState *borrowed_item) {
  AnalysisState *new_item;
  if (self->size) {
    new_item = array_pop(self);
  } else {
    new_item = ts_malloc(sizeof(AnalysisState));
  }

  *new_item = *borrowed_item;
  return new_item;
}

// Insert an owned clone in sorted order unless an equivalent state already exists.
// The caller retains borrowed_item.
static inline void analysis_state_set__insert_sorted(AnalysisStateSet *self, AnalysisStateSet *pool,
                                                     AnalysisState *borrowed_item) {
  unsigned index, exists;
  array_search_sorted_with(self, analysis_state__compare, &borrowed_item, &index, &exists);
  if (!exists) {
    AnalysisState *new_item = analysis_state_pool__clone_or_reuse(pool, borrowed_item);
    array_insert(self, index, new_item);
  }
}

// Append an owned clone. The caller must ensure it sorts after every existing entry
// according to analysis_state__compare; otherwise later set lookups are invalid.
static inline void analysis_state_set__push(AnalysisStateSet *self, AnalysisStateSet *pool,
                                            AnalysisState *borrowed_item) {
  AnalysisState *new_item = analysis_state_pool__clone_or_reuse(pool, borrowed_item);
  array_push(self, new_item);
}

// Return state allocations to the pool while retaining this worklist's pointer array.
static inline void analysis_state_set__clear(AnalysisStateSet *self, AnalysisStateSet *pool) {
  array_push_all(pool, self);
  array_clear(self);
}

// Release both owned states and their pointer array.
static inline void analysis_state_set__delete(AnalysisStateSet *self) {
  for (unsigned i = 0; i < self->size; i++) {
    ts_free(self->contents[i]);
  }

  array_delete(self);
}

// Create empty worklists; state allocations are reused across pattern analyses.
static inline QueryAnalysis query_analysis__new(void) {
  return (QueryAnalysis){
      .states = array_new(),
      .next_states = array_new(),
      .deeper_states = array_new(),
      .state_pool = array_new(),
      .final_step_indices = array_new(),
      .finished_parent_symbols = array_new(),
      .did_abort = false,
  };
}

// Each state belongs to exactly one worklist or the pool, so each is freed once.
static inline void query_analysis__delete(QueryAnalysis *self) {
  analysis_state_set__delete(&self->states);
  analysis_state_set__delete(&self->next_states);
  analysis_state_set__delete(&self->deeper_states);
  analysis_state_set__delete(&self->state_pool);
  array_delete(&self->final_step_indices);
  array_delete(&self->finished_parent_symbols);
}

// Keep production variants adjacent for each parse state and child position.
static inline int analysis_subgraph_node__compare(const AnalysisSubgraphNode *self,
                                                  const AnalysisSubgraphNode *other) {
  if (self->state < other->state) {
    return -1;
  }

  if (self->state > other->state) {
    return 1;
  }

  if (self->child_index < other->child_index) {
    return -1;
  }

  if (self->child_index > other->child_index) {
    return 1;
  }

  if (self->done < other->done) {
    return -1;
  }

  if (self->done > other->done) {
    return 1;
  }

  if (self->production_id < other->production_id) {
    return -1;
  }

  if (self->production_id > other->production_id) {
    return 1;
  }

  return 0;
}

// Search entry points by root symbol after the wildcard prefix. Entries are sorted
// by symbol, then pattern ID; result is the insertion position if no symbol matches.
static inline bool sq_native_query__pattern_map_search(const SQQuery *self, TSSymbol needle,
                                                       uint32_t *result) {
  uint32_t base_index = self->wildcard_root_pattern_count;
  uint32_t size = self->pattern_map.size - base_index;
  if (size == 0) {
    *result = base_index;
    return false;
  }

  while (size > 1) {
    uint32_t half_size = size / 2;
    uint32_t mid_index = base_index + half_size;
    TSSymbol mid_symbol =
        array_get(&self->steps, array_get(&self->pattern_map, mid_index)->step_index)->symbol;
    if (needle > mid_symbol) {
      base_index = mid_index;
    }

    size -= half_size;
  }

  TSSymbol symbol =
      array_get(&self->steps, array_get(&self->pattern_map, base_index)->step_index)->symbol;

  if (needle > symbol) {
    base_index++;
    if (base_index < self->pattern_map.size) {
      symbol =
          array_get(&self->steps, array_get(&self->pattern_map, base_index)->step_index)->symbol;
    }
  }

  *result = base_index;
  return needle == symbol;
}

// Insert a new pattern's start index into the pattern map, maintaining
// the pattern map's ordering invariant.
static inline void sq_native_query__pattern_map_insert(SQQuery *self, TSSymbol symbol,
                                                       PatternEntry new_entry) {
  uint32_t index;
  sq_native_query__pattern_map_search(self, symbol, &index);

  // Ensure that the entries are sorted not only by symbol, but also
  // by pattern_index. This way, states for earlier patterns will be
  // initiated first, which allows the ordering of the states array
  // to be maintained more efficiently.
  while (index < self->pattern_map.size) {
    PatternEntry *entry = array_get(&self->pattern_map, index);
    if (array_get(&self->steps, entry->step_index)->symbol == symbol &&
        entry->pattern_index < new_entry.pattern_index) {
      index++;
    } else {
      break;
    }
  }

  array_insert(&self->pattern_map, index, new_entry);
}

// Walk the subgraph for this non-terminal, tracking all of the possible
// sequences of progress within the pattern.
static void sq_native_query__perform_analysis(SQQuery *self, const AnalysisSubgraphArray *subgraphs,
                                              QueryAnalysis *analysis) {
  unsigned recursion_depth_limit = 0;
  unsigned prev_final_step_count = 0;
  array_clear(&analysis->final_step_indices);
  array_clear(&analysis->finished_parent_symbols);

  for (unsigned iteration = 0;; iteration++) {
    if (iteration == MAX_ANALYSIS_ITERATION_COUNT) {
      analysis->did_abort = true;
      break;
    }

#ifdef DEBUG_ANALYZE_QUERY
    printf("Iteration: %u. Final step indices:", iteration);
    for (unsigned j = 0; j < analysis->final_step_indices.size; j++) {
      printf(" %4u", *array_get(&analysis->final_step_indices, j));
    }

    printf("\n");
    for (unsigned j = 0; j < analysis->states.size; j++) {
      AnalysisState *state = *array_get(&analysis->states, j);
      printf("  %3u: step: %u, stack: [", j, state->step_index);
      for (unsigned k = 0; k < state->depth; k++) {
        printf(" {%s, child: %u, state: %4u",
               self->language->symbol_names[state->stack[k].parent_symbol],
               state->stack[k].child_index, state->stack[k].parse_state);
        if (state->stack[k].field_id) {
          printf(", field: %s", self->language->field_names[state->stack[k].field_id]);
        }

        if (state->stack[k].done) {
          printf(", DONE");
        }

        printf("}");
      }

      printf(" ]\n");
    }
#endif

    // If no further progress can be made within the current recursion depth limit, then
    // bump the depth limit by one, and continue to process the states the exceeded the
    // limit. But only allow this if progress has been made since the last time the depth
    // limit was increased.
    if (analysis->states.size == 0) {
      if (analysis->deeper_states.size > 0 &&
          analysis->final_step_indices.size > prev_final_step_count) {
#ifdef DEBUG_ANALYZE_QUERY
        printf("Increase recursion depth limit to %u\n", recursion_depth_limit + 1);
#endif

        prev_final_step_count = analysis->final_step_indices.size;
        recursion_depth_limit++;
        AnalysisStateSet _states = analysis->states;
        analysis->states = analysis->deeper_states;
        analysis->deeper_states = _states;
        continue;
      }

      break;
    }

    analysis_state_set__clear(&analysis->next_states, &analysis->state_pool);
    for (unsigned j = 0; j < analysis->states.size; j++) {
      AnalysisState *const state = *array_get(&analysis->states, j);

      // For efficiency, it's important to avoid processing the same analysis state more
      // than once. To achieve this, keep the states in order of ascending position within
      // their hypothetical syntax trees. In each iteration of this loop, start by advancing
      // the states that have made the least progress. Avoid advancing states that have already
      // made more progress.
      if (analysis->next_states.size > 0) {
        int comparison = analysis_state__compare(&state, array_back(&analysis->next_states));
        if (comparison == 0) {
          analysis_state_set__insert_sorted(&analysis->next_states, &analysis->state_pool, state);
          continue;
        } else if (comparison > 0) {
#ifdef DEBUG_ANALYZE_QUERY
          printf("Terminate iteration at state %u\n", j);
#endif
          while (j < analysis->states.size) {
            analysis_state_set__push(&analysis->next_states, &analysis->state_pool,
                                     *array_get(&analysis->states, j));
            j++;
          }

          break;
        }
      }

      const TSStateId parse_state = analysis_state__top(state)->parse_state;
      const TSSymbol parent_symbol = analysis_state__top(state)->parent_symbol;
      const TSFieldId parent_field_id = analysis_state__top(state)->field_id;
      const unsigned child_index = analysis_state__top(state)->child_index;
      const QueryStep *const step = array_get(&self->steps, state->step_index);

      unsigned subgraph_index, exists;
      array_search_sorted_by(subgraphs, .symbol, parent_symbol, &subgraph_index, &exists);
      if (!exists) {
        continue;
      }

      const AnalysisSubgraph *subgraph = array_get(subgraphs, subgraph_index);

      // Follow every possible path in the parse table, but only visit states that
      // are part of the subgraph for the current symbol.
      LookaheadIterator lookahead_iterator = ts_language_lookaheads(self->language, parse_state);
      while (ts_lookahead_iterator__next(&lookahead_iterator)) {
        TSSymbol sym = lookahead_iterator.symbol;

        AnalysisSubgraphNode successor = {
            .state = parse_state,
            .child_index = child_index,
        };
        if (lookahead_iterator.action_count) {
          const TSParseAction *action =
              &lookahead_iterator.actions[lookahead_iterator.action_count - 1];
          if (action->type == TSParseActionTypeShift) {
            if (!action->shift.extra) {
              successor.state = action->shift.state;
              successor.child_index++;
            }
          } else {
            continue;
          }
        } else if (lookahead_iterator.next_state != 0) {
          successor.state = lookahead_iterator.next_state;
          successor.child_index++;
        } else {
          continue;
        }

        unsigned node_index;
        array_search_sorted_with(&subgraph->nodes, analysis_subgraph_node__compare, &successor,
                                 &node_index, &exists);
        while (node_index < subgraph->nodes.size) {
          AnalysisSubgraphNode *node = array_get(&subgraph->nodes, node_index);
          node_index++;
          if (node->state != successor.state || node->child_index != successor.child_index) {
            break;
          }

          // Use the subgraph to determine what alias and field will eventually be applied
          // to this child node.
          TSSymbol alias = ts_language_alias_at(self->language, node->production_id, child_index);
          TSSymbol visible_symbol = alias ? alias
                                    : self->language->symbol_metadata[sym].visible
                                        ? self->language->public_symbol_map[sym]
                                        : 0;
          TSFieldId field_id = parent_field_id;
          if (!field_id) {
            const TSFieldMapEntry *field_map, *field_map_end;
            ts_language_field_map(self->language, node->production_id, &field_map, &field_map_end);
            for (; field_map != field_map_end; field_map++) {
              if (!field_map->inherited && field_map->child_index == child_index) {
                field_id = field_map->field_id;
                break;
              }
            }
          }

          // Create a new state that has advanced past this hypothetical subtree.
          AnalysisState next_state = *state;
          AnalysisStateEntry *next_state_top = analysis_state__top(&next_state);
          next_state_top->child_index = successor.child_index;
          next_state_top->parse_state = successor.state;
          if (node->done) {
            next_state_top->done = true;
          }

          // Determine if this hypothetical child node would match the current step
          // of the query pattern.
          bool does_match = false;

          // ERROR nodes can appear anywhere, so if the step is
          // looking for an ERROR node, consider it potentially matchable.
          if (step->symbol == ts_builtin_sym_error) {
            does_match = true;
          } else if (visible_symbol) {
            does_match = true;
            if (step->symbol == WILDCARD_SYMBOL) {
              if (((step->flags & SQ_STEP_IS_NAMED) != 0) &&
                  !self->language->symbol_metadata[visible_symbol].named) {
                does_match = false;
              }
            } else if (step->symbol != visible_symbol) {
              does_match = false;
            }

            if (step->field && step->field != field_id) {
              does_match = false;
            }

            if (step->supertype_symbol &&
                !analysis_state__has_supertype(state, step->supertype_symbol)) {
              does_match = false;
            }
          }

          // If this child is hidden, then descend into it and walk through its children.
          // If the top entry of the stack is at the end of its rule, then that entry can
          // be replaced. Otherwise, push a new entry onto the stack.
          else if (sym >= self->language->token_count) {
            if (!next_state_top->done) {
              if (next_state.depth + 1 >= MAX_ANALYSIS_STATE_DEPTH) {
#ifdef DEBUG_ANALYZE_QUERY
                printf("Exceeded depth limit for state %u\n", j);
#endif

                analysis->did_abort = true;
                continue;
              }

              next_state.depth++;
              next_state_top = analysis_state__top(&next_state);
            }

            *next_state_top = (AnalysisStateEntry){
                .parse_state = parse_state,
                .parent_symbol = sym,
                .child_index = 0,
                .field_id = field_id,
                .done = false,
            };

            if (analysis_state__recursion_depth(&next_state) > recursion_depth_limit) {
              analysis_state_set__insert_sorted(&analysis->deeper_states, &analysis->state_pool,
                                                &next_state);
              continue;
            }
          }

          // Pop from the stack when this state reached the end of its current syntax node.
          while (next_state.depth > 0 && next_state_top->done) {
            next_state.depth--;
            next_state_top = analysis_state__top(&next_state);
          }

          // If this hypothetical child did match the current step of the query pattern,
          // then advance to the next step at the current depth. This involves skipping
          // over any descendant steps of the current child.
          const QueryStep *next_step = step;
          if (does_match) {
            for (;;) {
              next_state.step_index++;
              next_step = array_get(&self->steps, next_state.step_index);
              if (next_step->depth == PATTERN_DONE_MARKER || next_step->depth <= step->depth) {
                break;
              }
            }
          } else if (successor.state == parse_state) {
            continue;
          }

          for (;;) {
            // Skip pass-through states. Although these states have alternatives, they are only
            // used to implement repetitions, and query analysis does not need to process
            // repetitions in order to determine whether steps are possible and definite.
            if (((next_step->flags & SQ_STEP_IS_PASS_THROUGH) != 0)) {
              next_state.step_index++;
              next_step++;
              continue;
            }

            // If the pattern is finished or hypothetical parent node is complete, then
            // record that matching can terminate at this step of the pattern. Otherwise,
            // add this state to the list of states to process on the next iteration.
            if (!((next_step->flags & SQ_STEP_IS_DEAD_END) != 0)) {
              bool did_finish_pattern =
                  array_get(&self->steps, next_state.step_index)->depth != step->depth;
              if (did_finish_pattern) {
                array_insert_sorted_by(&analysis->finished_parent_symbols, , state->root_symbol);
              } else if (next_state.depth == 0) {
                array_insert_sorted_by(&analysis->final_step_indices, , next_state.step_index);
              } else {
                analysis_state_set__insert_sorted(&analysis->next_states, &analysis->state_pool,
                                                  &next_state);
              }
            }

            // If the state has advanced to a step with an alternative step, then add another state
            // at that alternative step. This process is simpler than the process of actually
            // matching a pattern during query execution, because for the purposes of query
            // analysis, there is no need to process repetitions.
            if (does_match && next_step->alternative_index != NONE &&
                next_step->alternative_index > next_state.step_index) {
              next_state.step_index = next_step->alternative_index;
              next_step = array_get(&self->steps, next_state.step_index);
            } else {
              break;
            }
          }
        }
      }
    }

    AnalysisStateSet _states = analysis->states;
    analysis->states = analysis->next_states;
    analysis->next_states = _states;
  }
}

#ifdef DEBUG_DUMP_STEPS
// Inspect compiler output before and after grammar analysis.
static void sq_native_query__dump_steps(const SQQuery *self, const char *label) {
  printf("=== STEPS (%s) ===\n", label);
  for (unsigned i = 0; i < self->steps.size; i++) {
    const QueryStep *s = array_get(&self->steps, i);
    if (s->depth == PATTERN_DONE_MARKER) {
      printf("%3u: DONE\n", i);
      continue;
    }

    printf("%3u: depth=%u sym=%s", i, s->depth,
           s->symbol == WILDCARD_SYMBOL ? "_" : ts_language_symbol_name(self->language, s->symbol));
    if (s->supertype_symbol) {
      printf(" super=%s", ts_language_symbol_name(self->language, s->supertype_symbol));
    }

    if (s->field) {
      printf(" field=%s", ts_language_field_name_for_id(self->language, s->field));
    }

    if (s->alternative_index != NONE) {
      printf(" alt=%u", s->alternative_index);
    }

    if (((s->flags & SQ_STEP_IS_IMMEDIATE) != 0)) {
      printf(" IMM");
    }

    if (((s->flags & SQ_STEP_IS_PASS_THROUGH) != 0)) {
      printf(" PASS");
    }

    if (((s->flags & SQ_STEP_IS_DEAD_END) != 0)) {
      printf(" DEAD");
    }

    if (((s->flags & SQ_STEP_IS_LAST_CHILD) != 0)) {
      printf(" LAST");
    }

    if (((s->flags & SQ_STEP_IS_NAMED) != 0)) {
      printf(" NAMED");
    }

    if (((s->flags & SQ_STEP_IS_MISSING) != 0)) {
      printf(" MISSING");
    }

    if (((s->flags & SQ_STEP_IS_INSIDE_ALTERNATION) != 0)) {
      printf(" INALT");
    }

    if (((s->flags & SQ_STEP_CONTAINS_CAPTURES) != 0)) {
      printf(" HASCAP");
    }

    if (((s->flags & SQ_STEP_PARENT_PATTERN_GUARANTEED) != 0)) {
      printf(" PPG");
    }

    if (((s->flags & SQ_STEP_ROOT_PATTERN_GUARANTEED) != 0)) {
      printf(" RPG");
    }

    for (unsigned c = 0; c < MAX_STEP_CAPTURE_COUNT && s->capture_ids[c] != NONE; c++) {
      uint32_t cap_len;
      const char *cap_name = symbol_table_name_for_id(&self->captures, s->capture_ids[c], &cap_len);
      printf(" @%.*s", (int)cap_len, cap_name);
    }

    printf("\n");
  }
}
#endif

// Reject structurally impossible patterns and annotate where matching can fail.
// Grammar walks are bounded; incomplete walks leave conservative execution flags.
static bool sq_native_query__analyze_patterns(SQQuery *self, unsigned *error_offset) {
  Array(uint16_t) non_rooted_pattern_start_steps = array_new();
  for (unsigned i = 0; i < self->pattern_map.size; i++) {
    PatternEntry *pattern = array_get(&self->pattern_map, i);
    if (!((pattern->flags & SQ_PATTERN_IS_ROOTED) != 0)) {
      QueryStep *step = array_get(&self->steps, pattern->step_index);
      if (step->symbol != WILDCARD_SYMBOL) {
        array_push(&non_rooted_pattern_start_steps, i);
      }
    }
  }

  // Walk forward through all of the steps in the query, computing some
  // basic information about each step. Mark all of the steps that contain
  // captures, and record the indices of all of the steps that have child steps.
  Array(uint32_t) parent_step_indices = array_new();
  bool all_patterns_are_valid = true;
  for (unsigned i = 0; i < self->steps.size; i++) {
    QueryStep *step = array_get(&self->steps, i);
    if (step->depth == PATTERN_DONE_MARKER) {
      step->flags = (step->flags & ~SQ_STEP_PARENT_PATTERN_GUARANTEED) |
                    ((true) ? SQ_STEP_PARENT_PATTERN_GUARANTEED : 0);
      step->flags = (step->flags & ~SQ_STEP_ROOT_PATTERN_GUARANTEED) |
                    ((true) ? SQ_STEP_ROOT_PATTERN_GUARANTEED : 0);
      continue;
    }

    bool has_children = false;
    bool is_wildcard = step->symbol == WILDCARD_SYMBOL;
    step->flags = (step->flags & ~SQ_STEP_CONTAINS_CAPTURES) |
                  ((step->capture_ids[0] != NONE) ? SQ_STEP_CONTAINS_CAPTURES : 0);
    for (unsigned j = i + 1; j < self->steps.size; j++) {
      QueryStep *next_step = array_get(&self->steps, j);
      if (next_step->depth == PATTERN_DONE_MARKER || next_step->depth <= step->depth) {
        break;
      }

      if (next_step->capture_ids[0] != NONE) {
        step->flags =
            (step->flags & ~SQ_STEP_CONTAINS_CAPTURES) | ((true) ? SQ_STEP_CONTAINS_CAPTURES : 0);
      }

      if (!is_wildcard) {
        next_step->flags = (next_step->flags & ~SQ_STEP_ROOT_PATTERN_GUARANTEED) |
                           ((true) ? SQ_STEP_ROOT_PATTERN_GUARANTEED : 0);
        next_step->flags = (next_step->flags & ~SQ_STEP_PARENT_PATTERN_GUARANTEED) |
                           ((true) ? SQ_STEP_PARENT_PATTERN_GUARANTEED : 0);
      }

      has_children = true;
    }

    if (has_children) {
      if (!is_wildcard) {
        array_push(&parent_step_indices, i);
      } else if (step->supertype_symbol &&
                 self->language->abi_version >= LANGUAGE_VERSION_WITH_RESERVED_WORDS) {
        // Look at the child steps to see if any aren't valid subtypes for this supertype.
        uint32_t subtype_length;
        const TSSymbol *subtypes =
            ts_language_subtypes(self->language, step->supertype_symbol, &subtype_length);

        for (unsigned j = i + 1; j < self->steps.size; j++) {
          QueryStep *child_step = array_get(&self->steps, j);
          if (child_step->depth == PATTERN_DONE_MARKER || child_step->depth <= step->depth) {
            break;
          }

          if (child_step->depth == step->depth + 1 && child_step->symbol != WILDCARD_SYMBOL) {
            bool is_valid_subtype = false;
            for (uint32_t k = 0; k < subtype_length; k++) {
              if (child_step->symbol == subtypes[k]) {
                is_valid_subtype = true;
                break;
              }
            }

            if (!is_valid_subtype) {
              for (unsigned offset_idx = 0; offset_idx < self->step_offsets.size; offset_idx++) {
                StepOffset *step_offset = array_get(&self->step_offsets, offset_idx);
                if (step_offset->step_index >= j) {
                  *error_offset = step_offset->byte_offset;
                  all_patterns_are_valid = false;
                  goto supertype_cleanup;
                }
              }
            }
          }
        }
      }
    }
  }

  // For every parent symbol in the query, initialize an 'analysis subgraph'.
  // This subgraph lists all of the states in the parse table that are directly
  // involved in building subtrees for this symbol.
  //
  // In addition to the parent symbols in the query, construct subgraphs for all
  // of the hidden symbols in the grammar, because these might occur within
  // one of the parent nodes, such that their children appear to belong to the
  // parent.
  AnalysisSubgraphArray subgraphs = array_new();
  for (unsigned i = 0; i < parent_step_indices.size; i++) {
    uint32_t parent_step_index = *array_get(&parent_step_indices, i);
    TSSymbol parent_symbol = array_get(&self->steps, parent_step_index)->symbol;
    AnalysisSubgraph subgraph = {.symbol = parent_symbol};
    array_insert_sorted_by(&subgraphs, .symbol, subgraph);
  }

  for (TSSymbol sym = (uint16_t)self->language->token_count;
       sym < (uint16_t)self->language->symbol_count; sym++) {
    if (!ts_language_symbol_metadata(self->language, sym).visible) {
      AnalysisSubgraph subgraph = {.symbol = sym};
      array_insert_sorted_by(&subgraphs, .symbol, subgraph);
    }
  }

  // Scan the parse table to find the data needed to populate these subgraphs.
  // Collect three things during this scan:
  //   1) All of the parse states where one of these symbols can start.
  //   2) All of the parse states where one of these symbols can end, along
  //      with information about the node that would be created.
  //   3) A list of predecessor states for each state.
  StatePredecessorMap predecessor_map = state_predecessor_map_new(self->language);
  for (TSStateId state = 1; state < (uint16_t)self->language->state_count; state++) {
    unsigned subgraph_index, exists;
    LookaheadIterator lookahead_iterator = ts_language_lookaheads(self->language, state);
    while (ts_lookahead_iterator__next(&lookahead_iterator)) {
      if (lookahead_iterator.action_count) {
        for (unsigned i = 0; i < lookahead_iterator.action_count; i++) {
          const TSParseAction *action = &lookahead_iterator.actions[i];
          if (action->type == TSParseActionTypeReduce) {
            const TSSymbol *aliases, *aliases_end;
            ts_language_aliases_for_symbol(self->language, action->reduce.symbol, &aliases,
                                           &aliases_end);
            for (const TSSymbol *symbol = aliases; symbol < aliases_end; symbol++) {
              array_search_sorted_by(&subgraphs, .symbol, *symbol, &subgraph_index, &exists);
              if (exists) {
                AnalysisSubgraph *subgraph = array_get(&subgraphs, subgraph_index);
                if (subgraph->nodes.size == 0 || array_back(&subgraph->nodes)->state != state) {
                  array_push(&subgraph->nodes, ((AnalysisSubgraphNode){
                                                   .state = state,
                                                   .production_id = action->reduce.production_id,
                                                   .child_index = action->reduce.child_count,
                                                   .done = true,
                                               }));
                }
              }
            }
          } else if (action->type == TSParseActionTypeShift && !action->shift.extra) {
            TSStateId next_state = action->shift.state;
            state_predecessor_map_add(&predecessor_map, next_state, state);
          }
        }
      } else if (lookahead_iterator.next_state != 0) {
        if (lookahead_iterator.next_state != state) {
          state_predecessor_map_add(&predecessor_map, lookahead_iterator.next_state, state);
        }

        if (ts_language_state_is_primary(self->language, state)) {
          const TSSymbol *aliases, *aliases_end;
          ts_language_aliases_for_symbol(self->language, lookahead_iterator.symbol, &aliases,
                                         &aliases_end);
          for (const TSSymbol *symbol = aliases; symbol < aliases_end; symbol++) {
            array_search_sorted_by(&subgraphs, .symbol, *symbol, &subgraph_index, &exists);
            if (exists) {
              AnalysisSubgraph *subgraph = array_get(&subgraphs, subgraph_index);
              if (subgraph->start_states.size == 0 ||
                  *array_back(&subgraph->start_states) != state) {
                array_push(&subgraph->start_states, state);
              }
            }
          }
        }
      }
    }
  }

  // For each subgraph, compute the preceding states by walking backward
  // from the end states using the predecessor map.
  Array(AnalysisSubgraphNode) next_nodes = array_new();
  for (unsigned i = 0; i < subgraphs.size; i++) {
    AnalysisSubgraph *subgraph = array_get(&subgraphs, i);
    if (subgraph->nodes.size == 0) {
      array_delete(&subgraph->start_states);
      array_erase(&subgraphs, i);
      i--;
      continue;
    }

    array_assign(&next_nodes, &subgraph->nodes);
    while (next_nodes.size > 0) {
      AnalysisSubgraphNode node = array_pop(&next_nodes);
      if (node.child_index > 1) {
        unsigned predecessor_count;
        const TSStateId *predecessors =
            state_predecessor_map_get(&predecessor_map, node.state, &predecessor_count);
        for (unsigned j = 0; j < predecessor_count; j++) {
          AnalysisSubgraphNode predecessor_node = {
              .state = predecessors[j],
              .child_index = node.child_index - 1,
              .production_id = node.production_id,
              .done = false,
          };
          unsigned index, exists;
          array_search_sorted_with(&subgraph->nodes, analysis_subgraph_node__compare,
                                   &predecessor_node, &index, &exists);
          if (!exists) {
            array_insert(&subgraph->nodes, index, predecessor_node);
            array_push(&next_nodes, predecessor_node);
          }
        }
      }
    }
  }

#ifdef DEBUG_ANALYZE_QUERY
  printf("\nSubgraphs:\n");
  for (unsigned i = 0; i < subgraphs.size; i++) {
    AnalysisSubgraph *subgraph = array_get(&subgraphs, i);
    printf("  %u, %s:\n", subgraph->symbol,
           ts_language_symbol_name(self->language, subgraph->symbol));
    for (unsigned j = 0; j < subgraph->start_states.size; j++) {
      printf("    {state: %u}\n", *array_get(&subgraph->start_states, j));
    }

    for (unsigned j = 0; j < subgraph->nodes.size; j++) {
      AnalysisSubgraphNode *node = array_get(&subgraph->nodes, j);
      printf("    {state: %u, child_index: %u, production_id: %u, done: %d}\n", node->state,
             node->child_index, node->production_id, node->done);
    }

    printf("\n");
  }
#endif

  // For each non-terminal pattern, determine if the pattern can successfully match,
  // and identify all of the possible children within the pattern where matching could fail.
  QueryAnalysis analysis = query_analysis__new();
  for (unsigned i = 0; i < parent_step_indices.size; i++) {
    uint16_t parent_step_index = *array_get(&parent_step_indices, i);
    uint16_t parent_depth = array_get(&self->steps, parent_step_index)->depth;
    TSSymbol parent_symbol = array_get(&self->steps, parent_step_index)->symbol;
    if (parent_symbol == ts_builtin_sym_error) {
      continue;
    }

    // Find the subgraph that corresponds to this pattern's root symbol. If the pattern's
    // root symbol is a terminal, then return an error.
    unsigned subgraph_index, exists;
    array_search_sorted_by(&subgraphs, .symbol, parent_symbol, &subgraph_index, &exists);
    if (!exists) {
      unsigned first_child_step_index = parent_step_index + 1;
      uint32_t j, child_exists;
      array_search_sorted_by(&self->step_offsets, .step_index, first_child_step_index, &j,
                             &child_exists);
      ts_assert(child_exists);
      *error_offset = array_get(&self->step_offsets, j)->byte_offset;
      all_patterns_are_valid = false;
      break;
    }

    // Initialize an analysis state at every parse state in the table where
    // this parent symbol can occur.
    AnalysisSubgraph *subgraph = array_get(&subgraphs, subgraph_index);
    analysis_state_set__clear(&analysis.states, &analysis.state_pool);
    analysis_state_set__clear(&analysis.deeper_states, &analysis.state_pool);
    for (unsigned j = 0; j < subgraph->start_states.size; j++) {
      TSStateId parse_state = *array_get(&subgraph->start_states, j);
      analysis_state_set__push(&analysis.states, &analysis.state_pool,
                               &((AnalysisState){
                                   .step_index = parent_step_index + 1,
                                   .stack =
                                       {
                                           [0] =
                                               {
                                                   .parse_state = parse_state,
                                                   .parent_symbol = parent_symbol,
                                                   .child_index = 0,
                                                   .field_id = 0,
                                                   .done = false,
                                               },
                                       },
                                   .depth = 1,
                                   .root_symbol = parent_symbol,
                               }));
    }

#ifdef DEBUG_ANALYZE_QUERY
    printf("\nWalk states for %s:\n",
           ts_language_symbol_name(self->language,
                                   (*array_get(&analysis.states, 0))->stack[0].parent_symbol));
#endif

    analysis.did_abort = false;
    sq_native_query__perform_analysis(self, &subgraphs, &analysis);

    // If this pattern could not be fully analyzed, then every step should
    // be considered fallible.
    if (analysis.did_abort) {
      for (unsigned j = parent_step_index + 1; j < self->steps.size; j++) {
        QueryStep *step = array_get(&self->steps, j);
        if (step->depth <= parent_depth || step->depth == PATTERN_DONE_MARKER) {
          break;
        }

        if (!((step->flags & SQ_STEP_IS_DEAD_END) != 0)) {
          step->flags = (step->flags & ~SQ_STEP_PARENT_PATTERN_GUARANTEED) |
                        ((false) ? SQ_STEP_PARENT_PATTERN_GUARANTEED : 0);
          step->flags = (step->flags & ~SQ_STEP_ROOT_PATTERN_GUARANTEED) |
                        ((false) ? SQ_STEP_ROOT_PATTERN_GUARANTEED : 0);
        }
      }

      continue;
    }

    // If this pattern cannot match, store the pattern index so that it can be
    // returned to the caller.
    if (analysis.finished_parent_symbols.size == 0) {
      uint16_t impossible_step_index;
      if (analysis.final_step_indices.size > 0) {
        impossible_step_index = *array_back(&analysis.final_step_indices);
      } else {
        // If there isn't a final step, then that means the parent step itself is unreachable.
        impossible_step_index = parent_step_index;
      }

      uint32_t j, impossible_exists;
      array_search_sorted_by(&self->step_offsets, .step_index, impossible_step_index, &j,
                             &impossible_exists);
      if (j >= self->step_offsets.size) {
        j = self->step_offsets.size - 1;
      }

      *error_offset = array_get(&self->step_offsets, j)->byte_offset;
      all_patterns_are_valid = false;
      break;
    }

    // Mark as fallible any step where a match terminated.
    // Later, this property will be propagated to all of the step's predecessors.
    for (unsigned j = 0; j < analysis.final_step_indices.size; j++) {
      uint32_t final_step_index = *array_get(&analysis.final_step_indices, j);
      QueryStep *step = array_get(&self->steps, final_step_index);
      if (step->depth != PATTERN_DONE_MARKER && step->depth > parent_depth &&
          !((step->flags & SQ_STEP_IS_DEAD_END) != 0)) {
        step->flags = (step->flags & ~SQ_STEP_PARENT_PATTERN_GUARANTEED) |
                      ((false) ? SQ_STEP_PARENT_PATTERN_GUARANTEED : 0);
        step->flags = (step->flags & ~SQ_STEP_ROOT_PATTERN_GUARANTEED) |
                      ((false) ? SQ_STEP_ROOT_PATTERN_GUARANTEED : 0);
      }
    }
  }

  // Mark as indefinite any step with captures that are used in predicates.
  Array(uint16_t) predicate_capture_ids = array_new();
  for (unsigned i = 0; i < self->patterns.size; i++) {
    QueryPattern *pattern = array_get(&self->patterns, i);

    // Gather all of the captures that are used in predicates for this pattern.
    array_clear(&predicate_capture_ids);
    for (unsigned start = pattern->predicate_steps.offset,
                  end = start + pattern->predicate_steps.length, j = start;
         j < end; j++) {
      QueryPredicateStep *step = array_get(&self->predicate_steps, j);
      if (step->type == TSQueryPredicateStepTypeCapture) {
        uint16_t value_id = step->value_id;
        array_insert_sorted_by(&predicate_capture_ids, , value_id);
      }
    }

    // Find all of the steps that have these captures.
    for (unsigned start = pattern->steps.offset, end = start + pattern->steps.length, j = start;
         j < end; j++) {
      QueryStep *step = array_get(&self->steps, j);
      for (unsigned k = 0; k < MAX_STEP_CAPTURE_COUNT; k++) {
        uint16_t capture_id = step->capture_ids[k];
        if (capture_id == NONE) {
          break;
        }

        unsigned index, exists;
        array_search_sorted_by(&predicate_capture_ids, , capture_id, &index, &exists);
        if (exists) {
          step->flags = (step->flags & ~SQ_STEP_ROOT_PATTERN_GUARANTEED) |
                        ((false) ? SQ_STEP_ROOT_PATTERN_GUARANTEED : 0);
          break;
        }
      }
    }
  }

  // Propagate fallibility. If a pattern is fallible at a given step, then it is
  // fallible at all of its preceding steps.
  bool done = self->steps.size == 0;
  while (!done) {
    done = true;
    for (unsigned i = self->steps.size - 1; i > 0; i--) {
      QueryStep *step = array_get(&self->steps, i);
      if (step->depth == PATTERN_DONE_MARKER) {
        continue;
      }

      // Determine if this step is definite or has definite alternatives.
      bool parent_pattern_guaranteed = false;
      for (;;) {
        if (((step->flags & SQ_STEP_ROOT_PATTERN_GUARANTEED) != 0)) {
          parent_pattern_guaranteed = true;
          break;
        }

        if (step->alternative_index == NONE || step->alternative_index < i) {
          break;
        }

        step = array_get(&self->steps, step->alternative_index);
      }

      // If not, mark its predecessor as indefinite.
      if (!parent_pattern_guaranteed) {
        QueryStep *prev_step = array_get(&self->steps, i - 1);
        if (!((prev_step->flags & SQ_STEP_IS_DEAD_END) != 0) &&
            prev_step->depth != PATTERN_DONE_MARKER &&
            ((prev_step->flags & SQ_STEP_ROOT_PATTERN_GUARANTEED) != 0)) {
          prev_step->flags = (prev_step->flags & ~SQ_STEP_ROOT_PATTERN_GUARANTEED) |
                             ((false) ? SQ_STEP_ROOT_PATTERN_GUARANTEED : 0);
          done = false;
        }
      }
    }
  }

#ifdef DEBUG_ANALYZE_QUERY
  sq_native_query__dump_steps(self, "analysis");
#endif

  // Determine which repetition symbols in this language have the possibility
  // of matching non-rooted patterns in this query. These repetition symbols
  // prevent certain optimizations with range restrictions.
  analysis.did_abort = false;
  for (uint32_t i = 0; i < non_rooted_pattern_start_steps.size; i++) {
    uint16_t pattern_entry_index = *array_get(&non_rooted_pattern_start_steps, i);
    PatternEntry *pattern_entry = array_get(&self->pattern_map, pattern_entry_index);

    analysis_state_set__clear(&analysis.states, &analysis.state_pool);
    analysis_state_set__clear(&analysis.deeper_states, &analysis.state_pool);
    for (unsigned j = 0; j < subgraphs.size; j++) {
      AnalysisSubgraph *subgraph = array_get(&subgraphs, j);
      TSSymbolMetadata metadata = ts_language_symbol_metadata(self->language, subgraph->symbol);
      if (metadata.visible || metadata.named) {
        continue;
      }

      for (uint32_t k = 0; k < subgraph->start_states.size; k++) {
        TSStateId parse_state = *array_get(&subgraph->start_states, k);
        analysis_state_set__push(&analysis.states, &analysis.state_pool,
                                 &((AnalysisState){
                                     .step_index = pattern_entry->step_index,
                                     .stack =
                                         {
                                             [0] =
                                                 {
                                                     .parse_state = parse_state,
                                                     .parent_symbol = subgraph->symbol,
                                                     .child_index = 0,
                                                     .field_id = 0,
                                                     .done = false,
                                                 },
                                         },
                                     .root_symbol = subgraph->symbol,
                                     .depth = 1,
                                 }));
      }
    }

#ifdef DEBUG_ANALYZE_QUERY
    printf("\nWalk states for rootless pattern step %u:\n", pattern_entry->step_index);
#endif

    sq_native_query__perform_analysis(self, &subgraphs, &analysis);

    if (analysis.finished_parent_symbols.size > 0) {
      array_get(&self->patterns, pattern_entry->pattern_index)->flags |= SQ_PATTERN_IS_NON_LOCAL;
    }

    for (unsigned k = 0; k < analysis.finished_parent_symbols.size; k++) {
      TSSymbol symbol = *array_get(&analysis.finished_parent_symbols, k);
      array_insert_sorted_by(&self->repeat_symbols_with_rootless_patterns, , symbol);
    }
  }

#ifdef DEBUG_ANALYZE_QUERY
  if (self->repeat_symbols_with_rootless_patterns.size > 0) {
    printf("\nRepetition symbols with rootless patterns:\n");
    printf("aborted analysis: %d\n", analysis.did_abort);
    for (unsigned i = 0; i < self->repeat_symbols_with_rootless_patterns.size; i++) {
      TSSymbol symbol = *array_get(&self->repeat_symbols_with_rootless_patterns, i);
      printf("  %u, %s\n", symbol, ts_language_symbol_name(self->language, symbol));
    }

    printf("\n");
  }
#endif

  for (unsigned i = 0; i < subgraphs.size; i++) {
    array_delete(&array_get(&subgraphs, i)->start_states);
    array_delete(&array_get(&subgraphs, i)->nodes);
  }

  array_delete(&subgraphs);
  query_analysis__delete(&analysis);
  array_delete(&next_nodes);
  array_delete(&predicate_capture_ids);
  state_predecessor_map_delete(&predecessor_map);

supertype_cleanup:
  array_delete(&non_rooted_pattern_start_steps);
  array_delete(&parent_step_indices);

  return all_patterns_are_valid;
}

// Intern a zero-terminated field list and store its starting index in the step.
static void sq_native_query__add_negated_fields(SQQuery *self, uint16_t step_index,
                                                TSFieldId *field_ids, uint16_t field_count) {
  QueryStep *step = array_get(&self->steps, step_index);

  // The negated field array stores a list of field lists, separated by zeros.
  // Try to find the start index of an existing list that matches this new list.
  bool failed_match = false;
  unsigned match_count = 0;
  unsigned start_i = 0;
  for (unsigned i = 0; i < self->negated_fields.size; i++) {
    TSFieldId existing_field_id = *array_get(&self->negated_fields, i);

    // At each zero value, terminate the match attempt. If we've exactly
    // matched the new field list, then reuse this index. Otherwise,
    // start over the matching process.
    if (existing_field_id == 0) {
      if (match_count == field_count) {
        step->negated_field_list_id = start_i;
        return;
      } else {
        start_i = i + 1;
        match_count = 0;
        failed_match = false;
      }
    }

    // If the existing list matches our new list so far, then advance
    // to the next element of the new list.
    else if (match_count < field_count && existing_field_id == field_ids[match_count] &&
             !failed_match) {
      match_count++;
    }

    // Otherwise, this existing list has failed to match.
    else {
      match_count = 0;
      failed_match = true;
    }
  }

  step->negated_field_list_id = self->negated_fields.size;
  array_extend(&self->negated_fields, field_count, field_ids);
  array_push(&self->negated_fields, 0);
}

// Decode escapes into reusable scratch; malformed literals report their opening quote.
static TSQueryError sq_native_query__parse_string_literal(SQQuery *self, Stream *stream) {
  const char *string_start = stream->input;
  if (stream->next != '"') {
    return TSQueryErrorSyntax;
  }

  stream_advance(stream);
  const char *prev_position = stream->input;

  bool is_escaped = false;
  array_clear(&self->string_buffer);
  for (;;) {
    if (is_escaped) {
      is_escaped = false;
      switch (stream->next) {
      case 'n':
        array_push(&self->string_buffer, '\n');
        break;
      case 'r':
        array_push(&self->string_buffer, '\r');
        break;
      case 't':
        array_push(&self->string_buffer, '\t');
        break;
      case '0':
        array_push(&self->string_buffer, '\0');
        break;
      default:
        array_extend(&self->string_buffer, stream->next_size, stream->input);
        break;
      }

      prev_position = stream->input + stream->next_size;
    } else {
      if (stream->next == '\\') {
        array_extend(&self->string_buffer, (uint32_t)(stream->input - prev_position),
                     prev_position);
        prev_position = stream->input + 1;
        is_escaped = true;
      } else if (stream->next == '"') {
        array_extend(&self->string_buffer, (uint32_t)(stream->input - prev_position),
                     prev_position);
        stream_advance(stream);
        return TSQueryErrorNone;
      } else if (stream->next == '\n') {
        stream_reset(stream, string_start);
        return TSQueryErrorSyntax;
      }
    }

    if (!stream_advance(stream)) {
      stream_reset(stream, string_start);
      return TSQueryErrorSyntax;
    }
  }
}

// Compile a predicate into capture and string tokens, ending with a Done marker.
// Evaluation belongs to the Rust layer; this compiler only resolves its arguments.
static TSQueryError sq_native_query__parse_predicate(SQQuery *self, Stream *stream) {
  if (!stream_is_ident_start(stream)) {
    return TSQueryErrorSyntax;
  }

  const char *predicate_name = stream->input;
  stream_scan_identifier(stream);
  if (stream->next != '?' && stream->next != '!') {
    return TSQueryErrorSyntax;
  }

  stream_advance(stream);
  uint32_t length = (uint32_t)(stream->input - predicate_name);
  uint16_t id = symbol_table_insert_name(&self->predicate_values, predicate_name, length);
  array_push(&self->predicate_steps, ((QueryPredicateStep){
                                         .type = TSQueryPredicateStepTypeString,
                                         .value_id = id,
                                     }));
  stream_skip_whitespace(stream);

  for (;;) {
    if (stream->next == ')') {
      stream_advance(stream);
      stream_skip_whitespace(stream);
      array_push(&self->predicate_steps, ((QueryPredicateStep){
                                             .type = TSQueryPredicateStepTypeDone,
                                             .value_id = 0,
                                         }));
      break;
    }

    // Parse an '@'-prefixed capture name
    else if (stream->next == '@') {
      stream_advance(stream);

      // Parse the capture name
      if (!stream_is_ident_start(stream)) {
        return TSQueryErrorSyntax;
      }

      const char *capture_name = stream->input;
      stream_scan_identifier(stream);
      uint32_t capture_length = (uint32_t)(stream->input - capture_name);

      // Add the capture id to the first step of the pattern
      int capture_id = symbol_table_id_for_name(&self->captures, capture_name, capture_length);
      if (capture_id == -1) {
        stream_reset(stream, capture_name);
        return TSQueryErrorCapture;
      }

      array_push(&self->predicate_steps, ((QueryPredicateStep){
                                             .type = TSQueryPredicateStepTypeCapture,
                                             .value_id = capture_id,
                                         }));
    }

    // Parse a string literal
    else if (stream->next == '"') {
      TSQueryError e = sq_native_query__parse_string_literal(self, stream);
      if (e) {
        return e;
      }

      uint16_t query_id = symbol_table_insert_name(
          &self->predicate_values, self->string_buffer.contents, self->string_buffer.size);
      array_push(&self->predicate_steps, ((QueryPredicateStep){
                                             .type = TSQueryPredicateStepTypeString,
                                             .value_id = query_id,
                                         }));
    }

    // Parse a bare symbol
    else if (stream_is_ident_start(stream)) {
      const char *symbol_start = stream->input;
      stream_scan_identifier(stream);
      uint32_t symbol_length = (uint32_t)(stream->input - symbol_start);
      uint16_t query_id =
          symbol_table_insert_name(&self->predicate_values, symbol_start, symbol_length);
      array_push(&self->predicate_steps, ((QueryPredicateStep){
                                             .type = TSQueryPredicateStepTypeString,
                                             .value_id = query_id,
                                         }));
    }

    else {
      return TSQueryErrorSyntax;
    }

    stream_skip_whitespace(stream);
  }

  return 0;
}

// Recursively compile one pattern into match steps and alternative transitions.
// Each recursive call needs its own capture quantifiers, merged by the caller.
static TSQueryError sq_native_query__parse_pattern(SQQuery *self, Stream *stream, uint32_t depth,
                                                   bool is_immediate, bool is_inside_alternation,
                                                   CaptureQuantifiers *capture_quantifiers) {
  if (stream->next == 0) {
    return TSQueryErrorSyntax;
  }

  if (stream->next == ')' || stream->next == ']') {
    return PARENT_DONE;
  }

  const uint32_t starting_step_index = self->steps.size;

  // Store the byte offset of each step in the query.
  if (self->step_offsets.size == 0 ||
      array_back(&self->step_offsets)->step_index != starting_step_index) {
    array_push(&self->step_offsets, ((StepOffset){
                                        .step_index = starting_step_index,
                                        .byte_offset = stream_offset(stream),
                                    }));
  }

  // An open bracket is the start of an alternation.
  if (stream->next == '[') {
    stream_advance(stream);
    stream_skip_whitespace(stream);

    // Parse each branch, and add a placeholder step in between the branches.
    Array(uint32_t) branch_step_indices = array_new();
    CaptureQuantifiers branch_capture_quantifiers = capture_quantifiers_new();
    for (;;) {
      uint32_t start_index = self->steps.size;
      TSQueryError e = sq_native_query__parse_pattern(self, stream, depth, is_immediate, true,
                                                      &branch_capture_quantifiers);

      if (e == PARENT_DONE) {
        if (stream->next == ']' && branch_step_indices.size > 0) {
          stream_advance(stream);
          break;
        }

        e = TSQueryErrorSyntax;
      }

      if (e) {
        capture_quantifiers_delete(&branch_capture_quantifiers);
        array_delete(&branch_step_indices);
        return e;
      }

      if (start_index == starting_step_index) {
        capture_quantifiers_replace(capture_quantifiers, &branch_capture_quantifiers);
      } else {
        capture_quantifiers_join_all(capture_quantifiers, &branch_capture_quantifiers);
      }

      array_push(&branch_step_indices, start_index);
      array_push(&self->steps, query_step__new(0, depth, false));
      capture_quantifiers_clear(&branch_capture_quantifiers);
    }

    (void)array_pop(&self->steps);

    // For all of the branches except for the last one, add the subsequent branch as an
    // alternative, and link the end of the branch to the current end of the steps.
    for (unsigned i = 0; i < branch_step_indices.size - 1; i++) {
      uint32_t step_index = *array_get(&branch_step_indices, i);
      uint32_t next_step_index = *array_get(&branch_step_indices, i + 1);
      QueryStep *start_step = array_get(&self->steps, step_index);
      QueryStep *end_step = array_get(&self->steps, next_step_index - 1);
      start_step->alternative_index = next_step_index;
      end_step->alternative_index = self->steps.size;
      end_step->flags =
          (end_step->flags & ~SQ_STEP_IS_DEAD_END) | ((true) ? SQ_STEP_IS_DEAD_END : 0);
    }

    capture_quantifiers_delete(&branch_capture_quantifiers);
    array_delete(&branch_step_indices);
  }

  // An open parenthesis can be the start of three possible constructs:
  // * A grouped sequence
  // * A predicate
  // * A named node
  else if (stream->next == '(') {
    stream_advance(stream);
    stream_skip_whitespace(stream);

    // If this parenthesis is followed by a node, then it represents a grouped sequence.
    if (stream->next == '(' || stream->next == '"' || stream->next == '[') {
      bool child_is_immediate = is_immediate;
      CaptureQuantifiers child_capture_quantifiers = capture_quantifiers_new();
      for (;;) {
        if (stream->next == '.') {
          const char *anchor_start = stream->input;
          child_is_immediate = true;
          stream_advance(stream);
          stream_skip_whitespace(stream);

          // A `.` at a group's end has no sibling to anchor, and a group is not a
          // node, so there is no last child to anchor against.
          if (stream->next == ')') {
            stream_reset(stream, anchor_start);
            capture_quantifiers_delete(&child_capture_quantifiers);
            return TSQueryErrorSyntax;
          }
        }

        TSQueryError e =
            sq_native_query__parse_pattern(self, stream, depth, child_is_immediate,
                                           is_inside_alternation, &child_capture_quantifiers);
        if (e == PARENT_DONE) {
          if (stream->next == ')') {
            stream_advance(stream);
            break;
          }

          e = TSQueryErrorSyntax;
        }

        if (e) {
          capture_quantifiers_delete(&child_capture_quantifiers);
          return e;
        }

        capture_quantifiers_add_all(capture_quantifiers, &child_capture_quantifiers);
        capture_quantifiers_clear(&child_capture_quantifiers);
        child_is_immediate = false;
      }

      capture_quantifiers_delete(&child_capture_quantifiers);
    }

    // A dot/pound character indicates the start of a predicate.
    else if (stream->next == '.' || stream->next == '#') {
      stream_advance(stream);
      return sq_native_query__parse_predicate(self, stream);
    }

    // Otherwise, this parenthesis is the start of a named node.
    else {
      TSSymbol symbol;
      bool is_missing = false;
      const char *node_name = stream->input;

      // Parse a normal node name
      if (stream_is_ident_start(stream)) {
        stream_scan_identifier(stream);
        uint32_t length = (uint32_t)(stream->input - node_name);

        // Parse the wildcard symbol
        if (length == 1 && node_name[0] == '_') {
          symbol = WILDCARD_SYMBOL;
        } else if (length == 7 && !strncmp(node_name, "MISSING", length)) {
          is_missing = true;
          stream_skip_whitespace(stream);

          if (stream_is_ident_start(stream)) {
            const char *missing_node_name = stream->input;
            stream_scan_identifier(stream);
            uint32_t missing_node_length = (uint32_t)(stream->input - missing_node_name);
            symbol = ts_language_symbol_for_name(self->language, missing_node_name,
                                                 missing_node_length, true);
            if (!symbol) {
              stream_reset(stream, missing_node_name);
              return TSQueryErrorNodeType;
            }
          }

          else if (stream->next == '"') {
            const char *string_start = stream->input;
            TSQueryError e = sq_native_query__parse_string_literal(self, stream);
            if (e) {
              return e;
            }

            symbol = ts_language_symbol_for_name(self->language, self->string_buffer.contents,
                                                 self->string_buffer.size, false);
            if (!symbol) {
              stream_reset(stream, string_start + 1);
              return TSQueryErrorNodeType;
            }
          }

          else if (stream->next == ')') {
            symbol = WILDCARD_SYMBOL;
          }

          else {
            stream_reset(stream, stream->input);
            return TSQueryErrorSyntax;
          }
        }

        else {
          symbol = ts_language_symbol_for_name(self->language, node_name, length, true);
          if (!symbol) {
            stream_reset(stream, node_name);
            return TSQueryErrorNodeType;
          }
        }
      } else {
        return TSQueryErrorSyntax;
      }

      // Add a step for the node.
      array_push(&self->steps, query_step__new(symbol, depth, is_immediate));
      QueryStep *step = array_back(&self->steps);
      if (ts_language_symbol_metadata(self->language, symbol).supertype) {
        step->supertype_symbol = step->symbol;
        step->symbol = WILDCARD_SYMBOL;
      }

      if (is_missing) {
        step->flags = (step->flags & ~SQ_STEP_IS_MISSING) | ((true) ? SQ_STEP_IS_MISSING : 0);
      }

      if (symbol == WILDCARD_SYMBOL) {
        step->flags = (step->flags & ~SQ_STEP_IS_NAMED) | ((true) ? SQ_STEP_IS_NAMED : 0);
      }

      // Parse a supertype symbol
      if (stream->next == '/') {
        if (!step->supertype_symbol) {
          stream_reset(stream, node_name - 1); // reset to the start of the node
          return TSQueryErrorStructure;
        }

        stream_advance(stream);

        const char *subtype_node_name = stream->input;

        if (stream_is_ident_start(stream)) { // Named node
          stream_scan_identifier(stream);
          uint32_t length = (uint32_t)(stream->input - subtype_node_name);
          step->symbol =
              ts_language_symbol_for_name(self->language, subtype_node_name, length, true);
        } else if (stream->next == '"') { // Anonymous leaf node
          TSQueryError e = sq_native_query__parse_string_literal(self, stream);
          if (e) {
            return e;
          }

          step->symbol = ts_language_symbol_for_name(self->language, self->string_buffer.contents,
                                                     self->string_buffer.size, false);
        } else {
          return TSQueryErrorSyntax;
        }

        if (!step->symbol) {
          stream_reset(stream, subtype_node_name);
          return TSQueryErrorNodeType;
        }

        // Get all the possible subtypes for the given supertype,
        // and check if the given subtype is valid.
        if (self->language->abi_version >= LANGUAGE_VERSION_WITH_RESERVED_WORDS) {
          uint32_t subtype_length;
          const TSSymbol *subtypes =
              ts_language_subtypes(self->language, step->supertype_symbol, &subtype_length);

          bool subtype_is_valid = false;
          for (uint32_t i = 0; i < subtype_length; i++) {
            if (subtypes[i] == step->symbol) {
              subtype_is_valid = true;
              break;
            }
          }

          // This subtype is not valid for the given supertype.
          if (!subtype_is_valid) {
            stream_reset(stream, node_name - 1); // reset to the start of the node
            return TSQueryErrorStructure;
          }
        }
      }

      stream_skip_whitespace(stream);

      // Parse the child patterns
      bool child_is_immediate = false;
      uint16_t last_child_step_index = 0;
      uint16_t negated_field_count = 0;
      TSFieldId negated_field_ids[MAX_NEGATED_FIELD_COUNT];
      CaptureQuantifiers child_capture_quantifiers = capture_quantifiers_new();
      for (;;) {
        // Parse a negated field assertion
        if (stream->next == '!') {
          stream_advance(stream);
          stream_skip_whitespace(stream);
          if (!stream_is_ident_start(stream)) {
            capture_quantifiers_delete(&child_capture_quantifiers);
            return TSQueryErrorSyntax;
          }

          const char *field_name = stream->input;
          stream_scan_identifier(stream);
          uint32_t length = (uint32_t)(stream->input - field_name);
          stream_skip_whitespace(stream);

          TSFieldId field_id = ts_language_field_id_for_name(self->language, field_name, length);
          if (!field_id) {
            stream->input = field_name;
            capture_quantifiers_delete(&child_capture_quantifiers);
            return TSQueryErrorField;
          }

          // Keep at most the fixed number of negated fields per step.
          if (negated_field_count < MAX_NEGATED_FIELD_COUNT) {
            negated_field_ids[negated_field_count] = field_id;
            negated_field_count++;
          }

          continue;
        }

        // Parse a sibling anchor
        if (stream->next == '.') {
          child_is_immediate = true;
          stream_advance(stream);
          stream_skip_whitespace(stream);
        }

        uint16_t step_index = self->steps.size;
        TSQueryError e =
            sq_native_query__parse_pattern(self, stream, depth + 1, child_is_immediate,
                                           is_inside_alternation, &child_capture_quantifiers);

        // In the event we only parsed a predicate, meaning no new steps were added,
        // then subtract one so we're not indexing past the end of the array
        if (step_index == self->steps.size) {
          step_index--;
        }

        if (e == PARENT_DONE) {
          if (stream->next == ')') {
            if (child_is_immediate) {
              if (last_child_step_index == 0) {
                capture_quantifiers_delete(&child_capture_quantifiers);
                return TSQueryErrorSyntax;
              }

              // Mark this step *and* its alternatives as the last child of the parent.
              QueryStep *last_child_step = array_get(&self->steps, last_child_step_index);
              last_child_step->flags = (last_child_step->flags & ~SQ_STEP_IS_LAST_CHILD) |
                                       ((true) ? SQ_STEP_IS_LAST_CHILD : 0);
              if (last_child_step->alternative_index != NONE &&
                  last_child_step->alternative_index < self->steps.size) {
                QueryStep *alternative_step =
                    array_get(&self->steps, last_child_step->alternative_index);
                alternative_step->flags = (alternative_step->flags & ~SQ_STEP_IS_LAST_CHILD) |
                                          ((true) ? SQ_STEP_IS_LAST_CHILD : 0);
                while (alternative_step->alternative_index != NONE &&
                       alternative_step->alternative_index < self->steps.size) {
                  alternative_step = array_get(&self->steps, alternative_step->alternative_index);
                  alternative_step->flags = (alternative_step->flags & ~SQ_STEP_IS_LAST_CHILD) |
                                            ((true) ? SQ_STEP_IS_LAST_CHILD : 0);
                }
              }
            }

            if (negated_field_count) {
              sq_native_query__add_negated_fields(self, starting_step_index, negated_field_ids,
                                                  negated_field_count);
            }

            stream_advance(stream);
            break;
          }

          e = TSQueryErrorSyntax;
        }

        if (e) {
          capture_quantifiers_delete(&child_capture_quantifiers);
          return e;
        }

        capture_quantifiers_add_all(capture_quantifiers, &child_capture_quantifiers);

        last_child_step_index = step_index;
        child_is_immediate = false;
        capture_quantifiers_clear(&child_capture_quantifiers);
      }

      capture_quantifiers_delete(&child_capture_quantifiers);
    }
  }

  // Parse a wildcard pattern
  else if (stream->next == '_') {
    stream_advance(stream);
    stream_skip_whitespace(stream);

    // Add a step that matches any kind of node
    array_push(&self->steps, query_step__new(WILDCARD_SYMBOL, depth, is_immediate));
  }

  // Parse a double-quoted anonymous leaf node expression
  else if (stream->next == '"') {
    const char *string_start = stream->input;
    TSQueryError e = sq_native_query__parse_string_literal(self, stream);
    if (e) {
      return e;
    }

    // Add a step for the node
    TSSymbol symbol = ts_language_symbol_for_name(self->language, self->string_buffer.contents,
                                                  self->string_buffer.size, false);
    if (!symbol) {
      stream_reset(stream, string_start + 1);
      return TSQueryErrorNodeType;
    }

    array_push(&self->steps, query_step__new(symbol, depth, is_immediate));
  }

  // Parse a field-prefixed pattern
  else if (stream_is_ident_start(stream)) {
    // Parse the field name
    const char *field_name = stream->input;
    stream_scan_identifier(stream);
    uint32_t length = (uint32_t)(stream->input - field_name);
    stream_skip_whitespace(stream);

    if (stream->next != ':') {
      stream_reset(stream, field_name);
      return TSQueryErrorSyntax;
    }

    stream_advance(stream);
    stream_skip_whitespace(stream);

    // Parse the pattern
    CaptureQuantifiers field_capture_quantifiers = capture_quantifiers_new();
    TSQueryError e = sq_native_query__parse_pattern(
        self, stream, depth, is_immediate, is_inside_alternation, &field_capture_quantifiers);
    if (e) {
      capture_quantifiers_delete(&field_capture_quantifiers);
      if (e == PARENT_DONE) {
        e = TSQueryErrorSyntax;
      }

      return e;
    }

    // Add the field name to the first step of the pattern
    TSFieldId field_id = ts_language_field_id_for_name(self->language, field_name, length);
    if (!field_id) {
      stream->input = field_name;
      return TSQueryErrorField;
    }

    uint32_t step_index = starting_step_index;
    QueryStep *step = array_get(&self->steps, step_index);
    for (;;) {
      step->field = field_id;
      if (step->alternative_index != NONE && step->alternative_index > step_index &&
          step->alternative_index < self->steps.size) {
        step_index = step->alternative_index;
        step = array_get(&self->steps, step_index);
      } else {
        break;
      }
    }

    capture_quantifiers_add_all(capture_quantifiers, &field_capture_quantifiers);
    capture_quantifiers_delete(&field_capture_quantifiers);
  }

  else {
    return TSQueryErrorSyntax;
  }

  stream_skip_whitespace(stream);

  // Parse suffix modifiers for this pattern.
  TSQuantifier quantifier = TSQuantifierOne;
  for (;;) {
    // Parse the one-or-more operator.
    if (stream->next == '+') {
      quantifier = quantifier_join(TSQuantifierOneOrMore, quantifier);

      stream_advance(stream);
      stream_skip_whitespace(stream);
    }

    // Parse the zero-or-more repetition operator.
    else if (stream->next == '*') {
      quantifier = quantifier_join(TSQuantifierZeroOrMore, quantifier);

      stream_advance(stream);
      stream_skip_whitespace(stream);
    }

    // Parse the optional operator.
    else if (stream->next == '?') {
      quantifier = quantifier_join(TSQuantifierZeroOrOne, quantifier);

      stream_advance(stream);
      stream_skip_whitespace(stream);
    }

    // Parse an '@'-prefixed capture pattern
    else if (stream->next == '@') {
      stream_advance(stream);
      if (!stream_is_ident_start(stream)) {
        return TSQueryErrorSyntax;
      }

      const char *capture_name = stream->input;
      stream_scan_identifier(stream);
      uint32_t length = (uint32_t)(stream->input - capture_name);
      stream_skip_whitespace(stream);

      // Add the capture id to the first step of the pattern
      uint16_t capture_id = symbol_table_insert_name(&self->captures, capture_name, length);

      // Add the capture quantifier
      capture_quantifiers_add_for_id(capture_quantifiers, capture_id, TSQuantifierOne);

      uint32_t step_index = starting_step_index;
      for (;;) {
        QueryStep *step = array_get(&self->steps, step_index);
        query_step__add_capture(step, capture_id);
        if (step->alternative_index != NONE && step->alternative_index > step_index &&
            step->alternative_index < self->steps.size) {
          step_index = step->alternative_index;
        } else {
          break;
        }
      }
    }

    // No more suffix modifiers
    else {
      break;
    }
  }

  // Lower repetition into explicit loop and skip transitions for the executor.
  QueryStep repeat_step;
  QueryStep *step;
  switch (quantifier) {
  case TSQuantifierOneOrMore:
    repeat_step = query_step__new(WILDCARD_SYMBOL, depth, false);
    repeat_step.flags = (repeat_step.flags & ~SQ_STEP_IS_INSIDE_ALTERNATION) |
                        ((is_inside_alternation) ? SQ_STEP_IS_INSIDE_ALTERNATION : 0);
    repeat_step.alternative_index = starting_step_index;
    repeat_step.flags =
        (repeat_step.flags & ~SQ_STEP_IS_PASS_THROUGH) | ((true) ? SQ_STEP_IS_PASS_THROUGH : 0);
    array_push(&self->steps, repeat_step);
    break;
  case TSQuantifierZeroOrMore:
    repeat_step = query_step__new(WILDCARD_SYMBOL, depth, false);
    repeat_step.flags = (repeat_step.flags & ~SQ_STEP_IS_INSIDE_ALTERNATION) |
                        ((is_inside_alternation) ? SQ_STEP_IS_INSIDE_ALTERNATION : 0);
    repeat_step.alternative_index = starting_step_index;
    repeat_step.flags =
        (repeat_step.flags & ~SQ_STEP_IS_PASS_THROUGH) | ((true) ? SQ_STEP_IS_PASS_THROUGH : 0);
    array_push(&self->steps, repeat_step);

    // Stop when `step->alternative_index` is `NONE` or it points to
    // `repeat_step` or beyond. Note that having just been pushed,
    // `repeat_step` occupies slot `self->steps.size - 1`.
    step = array_get(&self->steps, starting_step_index);
    while (step->alternative_index != NONE && step->alternative_index < self->steps.size - 1) {
      step = array_get(&self->steps, step->alternative_index);
    }

    step->alternative_index = self->steps.size;
    step->flags =
        (step->flags & ~SQ_STEP_ALTERNATIVE_IS_SKIP) | ((true) ? SQ_STEP_ALTERNATIVE_IS_SKIP : 0);
    break;
  case TSQuantifierZeroOrOne:
    step = array_get(&self->steps, starting_step_index);
    while (step->alternative_index != NONE && step->alternative_index < self->steps.size) {
      step = array_get(&self->steps, step->alternative_index);
    }

    step->alternative_index = self->steps.size;
    step->flags =
        (step->flags & ~SQ_STEP_ALTERNATIVE_IS_SKIP) | ((true) ? SQ_STEP_ALTERNATIVE_IS_SKIP : 0);
    break;
  default:
    break;
  }

  capture_quantifiers_mul(capture_quantifiers, quantifier);

  return 0;
}

// Compile and analyze a query, retaining its language and all arrays borrowed by Rust.
// Compilation scratch is discarded before the result is exposed.
SQQuery *sq_native_query_new(const TSLanguage *language, const char *source, uint32_t source_len,
                             uint32_t *error_offset, TSQueryError *error_type) {
  if (!language || language->abi_version > TREE_SITTER_LANGUAGE_VERSION ||
      language->abi_version < TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION) {
    *error_type = TSQueryErrorLanguage;
    return NULL;
  }

  SQQuery *self = ts_malloc(sizeof(SQQuery));
  *self = (SQQuery){
      .steps = array_new(),
      .pattern_map = array_new(),
      .captures = symbol_table_new(),
      .capture_quantifiers = array_new(),
      .predicate_values = symbol_table_new(),
      .predicate_steps = array_new(),
      .patterns = array_new(),
      .step_offsets = array_new(),
      .string_buffer = array_new(),
      .negated_fields = array_new(),
      .repeat_symbols_with_rootless_patterns = array_new(),
      .wildcard_root_pattern_count = 0,
      .language = ts_language_copy(language),
  };

  array_push(&self->negated_fields, 0);

  // Parse all of the S-expressions in the given string.
  Stream stream = stream_new(source, source_len);
  stream_skip_whitespace(&stream);
  while (stream.input < stream.end) {
    uint32_t pattern_index = self->patterns.size;
    uint32_t start_step_index = self->steps.size;
    uint32_t start_predicate_step_index = self->predicate_steps.size;

    array_push(&self->patterns,
               ((QueryPattern){
                   .steps = (Slice){.offset = start_step_index},
                   .predicate_steps = (Slice){.offset = start_predicate_step_index},
                   .start_byte = stream_offset(&stream),
                   .flags = (false ? SQ_PATTERN_IS_NON_LOCAL : 0),
               }));

    CaptureQuantifiers capture_quantifiers = capture_quantifiers_new();
    *error_type =
        sq_native_query__parse_pattern(self, &stream, 0, false, false, &capture_quantifiers);
    array_push(&self->steps, query_step__new(0, PATTERN_DONE_MARKER, false));

    QueryPattern *pattern = array_back(&self->patterns);
    pattern->steps.length = self->steps.size - start_step_index;
    pattern->predicate_steps.length = self->predicate_steps.size - start_predicate_step_index;
    pattern->end_byte = stream_offset(&stream);

    // If any pattern could not be parsed, then report the error information
    // and terminate.
    if (*error_type) {
      if (*error_type == PARENT_DONE) {
        *error_type = TSQueryErrorSyntax;
      }

      *error_offset = stream_offset(&stream);
      capture_quantifiers_delete(&capture_quantifiers);
      sq_native_query_delete(self);
      return NULL;
    }

    // Maintain a list of capture quantifiers for each pattern
    array_push(&self->capture_quantifiers, capture_quantifiers);

    // Maintain a map that can look up patterns for a given root symbol.
    uint16_t wildcard_root_alternative_index = NONE;
    for (;;) {
      QueryStep *step = array_get(&self->steps, start_step_index);

      // If a pattern has a wildcard at its root, but it has a non-wildcard child,
      // then optimize the matching process by skipping matching the wildcard.
      // Later, during the matching process, the query cursor will check that
      // there is a parent node, and capture it if necessary.
      if (step->symbol == WILDCARD_SYMBOL && step->depth == 0 && !step->field) {
        QueryStep *second_step = array_get(&self->steps, start_step_index + 1);
        if (second_step->symbol != WILDCARD_SYMBOL && second_step->depth == 1 &&
            !((second_step->flags & SQ_STEP_IS_IMMEDIATE) != 0)) {
          wildcard_root_alternative_index = step->alternative_index;
          start_step_index += 1;
          step = second_step;
        }
      }

      // Determine whether the pattern has a single root node. This affects
      // decisions about whether or not to start matching the pattern when
      // a query cursor has a range restriction or when immediately within an
      // error node.
      uint32_t start_depth = step->depth;
      bool is_rooted = start_depth == 0;
      for (uint32_t step_index = start_step_index + 1; step_index < self->steps.size;
           step_index++) {
        QueryStep *child_step = array_get(&self->steps, step_index);
        if (((child_step->flags & SQ_STEP_IS_DEAD_END) != 0)) {
          break;
        }

        if (child_step->depth == start_depth) {
          is_rooted = false;
          break;
        }
      }

      sq_native_query__pattern_map_insert(
          self, step->symbol,
          (PatternEntry){.step_index = start_step_index,
                         .pattern_index = pattern_index,
                         .flags = (is_rooted ? SQ_PATTERN_IS_ROOTED : 0)});
      if (step->symbol == WILDCARD_SYMBOL) {
        self->wildcard_root_pattern_count++;
      }

      // If there are alternatives or options at the root of the pattern,
      // then add multiple entries to the pattern map.
      if (step->alternative_index != NONE) {
        start_step_index = step->alternative_index;
      } else if (wildcard_root_alternative_index != NONE) {
        start_step_index = wildcard_root_alternative_index;
        wildcard_root_alternative_index = NONE;
      } else {
        break;
      }
    }

    // A quantified branch must loop to its own start without taking the next branch's
    // alternative. Clone the first step without that link and redirect the loop there.
    {
      uint32_t pat_start = pattern->steps.offset;
      uint32_t pat_end = pat_start + pattern->steps.length - 1; // exclude DONE

      for (uint32_t i = pat_start; i < pat_end; i++) {
        QueryStep *s = array_get(&self->steps, i);

        // Ensure this step is a pass_through with a _backward_ alternative (a quantifier loop-back)
        if (!((s->flags & SQ_STEP_IS_PASS_THROUGH) != 0) ||
            !((s->flags & SQ_STEP_IS_INSIDE_ALTERNATION) != 0) || s->alternative_index == NONE ||
            s->alternative_index >= i) {
          continue;
        }

        uint32_t target_idx = s->alternative_index;
        QueryStep *target = array_get(&self->steps, target_idx);

        // Check if the target has a forward alternative from alternation linking
        uint16_t target_alt_index = target->alternative_index;
        if (target_alt_index == NONE || target_alt_index <= target_idx ||
            target_alt_index >= pat_end) {
          continue;
        }

        // Create a clean copy of the target step without the alternation alternative.
        uint32_t copy_idx = self->steps.size;
        QueryStep copy = *target;
        copy.alternative_index = NONE;
        uint16_t target_depth = target->depth;
        array_push(&self->steps, copy);

        // Add a dead_end that redirects to the pass through step after the target,
        // so the pattern continues correctly after the cleaned copy matches.
        QueryStep redirect = query_step__new(0, target_depth, false);
        redirect.flags =
            (redirect.flags & ~SQ_STEP_IS_DEAD_END) | ((true) ? SQ_STEP_IS_DEAD_END : 0);
        redirect.alternative_index = target_idx + 1;
        array_push(&self->steps, redirect);

        // Update the pass_through to loop back to the copy. Reacquire `s` since
        // `self->steps` may have been reallocated.
        s = array_get(&self->steps, i);
        s->alternative_index = copy_idx;
      }
    }
  }

#ifdef DEBUG_DUMP_STEPS
  sq_native_query__dump_steps(self, "post-parse");
#endif

  if (!sq_native_query__analyze_patterns(self, error_offset)) {
    *error_type = TSQueryErrorStructure;
    sq_native_query_delete(self);
    return NULL;
  }

#ifdef DEBUG_DUMP_STEPS
  sq_native_query__dump_steps(self, "post-analysis");
#endif

  array_delete(&self->string_buffer);
  array_delete(&self->step_offsets);

  // Quantifier arrays can no longer grow, so their borrowed views are now stable.
  array_reserve(&self->quantifier_views, self->capture_quantifiers.size);
  for (uint32_t index = 0; index < self->capture_quantifiers.size; index++) {
    CaptureQuantifiers *quantifiers = &self->capture_quantifiers.contents[index];
    array_push(&self->quantifier_views, ((NativeView){quantifiers->contents, quantifiers->size}));
  }

  return self;
}

// Release the program owner, including nested capture arrays and partial compilation state.
void sq_native_query_delete(SQQuery *self) {
  if (!self) return;

  array_delete(&self->steps);
  array_delete(&self->pattern_map);
  array_delete(&self->predicate_steps);
  array_delete(&self->patterns);
  array_delete(&self->step_offsets);
  array_delete(&self->negated_fields);
  array_delete(&self->string_buffer);
  array_delete(&self->repeat_symbols_with_rootless_patterns);

  symbol_table_delete(&self->captures);
  symbol_table_delete(&self->predicate_values);

  for (uint32_t index = 0; index < self->capture_quantifiers.size; index++)
    capture_quantifiers_delete(&self->capture_quantifiers.contents[index]);
  array_delete(&self->capture_quantifiers);
  array_delete(&self->quantifier_views);

  ts_language_delete(self->language);
  ts_free(self);
}

// Publish borrowed pointer/length pairs without copying the compiled program.
// Recreate the view after disabling patterns, which can change pattern_map's length.
void sq_native_query_view(const SQQuery *self, SQQueryView *view) {
#define VIEW(array) ((NativeView){(array).contents, (array).size})
  *view = (SQQueryView){
      .language = self->language,
      .symbol_count = self->language->symbol_count + self->language->alias_count,
      .public_symbols = {self->language->public_symbol_map,
                         self->language->symbol_count + self->language->alias_count},
      .steps = VIEW(self->steps),
      .pattern_entries = VIEW(self->pattern_map),
      .patterns = VIEW(self->patterns),
      .predicate_steps = VIEW(self->predicate_steps),
      .capture_names = {VIEW(self->captures.characters), VIEW(self->captures.slices)},
      .predicate_values = {VIEW(self->predicate_values.characters),
                           VIEW(self->predicate_values.slices)},
      .capture_quantifiers = VIEW(self->quantifier_views),
      .negated_fields = VIEW(self->negated_fields),
      .rootless_repeat_symbols = VIEW(self->repeat_symbols_with_rootless_patterns),
      .wildcard_root_pattern_count = self->wildcard_root_pattern_count,
  };
#undef VIEW
}

// Counts describe the original ID spaces, including disabled patterns and captures.
uint32_t sq_native_query_pattern_count(const SQQuery *self) {
  return self->patterns.size;
}

uint32_t sq_native_query_capture_count(const SQQuery *self) {
  return self->captures.slices.size;
}

uint32_t sq_native_query_string_count(const SQQuery *self) {
  return self->predicate_values.slices.size;
}

// Borrow a capture name by its stable ID; length excludes the stored terminator.
const char *sq_native_query_capture_name_for_id(const SQQuery *self, uint32_t index,
                                                uint32_t *length) {
  return symbol_table_name_for_id(&self->captures, index, length);
}

// Captures not present in the selected pattern have quantifier Zero.
TSQuantifier sq_native_query_capture_quantifier_for_id(const SQQuery *self, uint32_t pattern_index,
                                                       uint32_t capture_index) {
  CaptureQuantifiers *capture_quantifiers = array_get(&self->capture_quantifiers, pattern_index);
  return capture_quantifier_for_id(capture_quantifiers, capture_index);
}

// Borrow a predicate string by ID; embedded NUL bytes are included in length.
const char *sq_native_query_string_value_for_id(const SQQuery *self, uint32_t index,
                                                uint32_t *length) {
  return symbol_table_name_for_id(&self->predicate_values, index, length);
}

// Borrow one pattern's predicate token range; an empty range returns NULL.
const QueryPredicateStep *sq_native_query_predicates_for_pattern(const SQQuery *self,
                                                                 uint32_t pattern_index,
                                                                 uint32_t *step_count) {
  Slice slice = array_get(&self->patterns, pattern_index)->predicate_steps;
  *step_count = slice.length;
  if (slice.length == 0) {
    return NULL;
  }

  return array_get(&self->predicate_steps, slice.offset);
}

// Source offsets refer to the original query text, even after patterns are disabled.
uint32_t sq_native_query_start_byte_for_pattern(const SQQuery *self, uint32_t pattern_index) {
  return array_get(&self->patterns, pattern_index)->start_byte;
}

// The source range's end is exclusive.
uint32_t sq_native_query_end_byte_for_pattern(const SQQuery *self, uint32_t pattern_index) {
  return array_get(&self->patterns, pattern_index)->end_byte;
}

// Remove capture emissions without renumbering names or predicate references.
void sq_native_query_disable_capture(SQQuery *self, const char *name, uint32_t length) {
  int id = symbol_table_id_for_name(&self->captures, name, length);
  if (id != -1) {
    for (unsigned i = 0; i < self->steps.size; i++) {
      QueryStep *step = array_get(&self->steps, i);
      query_step__remove_capture(step, id);
    }
  }
}

// Remove every entry point while keeping pattern IDs and compiled steps stable.
void sq_native_query_disable_pattern(SQQuery *self, uint32_t pattern_index) {
  for (uint32_t index = 0; index < self->pattern_map.size;) {
    if (self->pattern_map.contents[index].pattern_index == pattern_index) {
      if (index < self->wildcard_root_pattern_count) self->wildcard_root_pattern_count--;
      array_erase(&self->pattern_map, index);
    } else index++;
  }
}
