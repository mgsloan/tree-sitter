#ifndef _DEFAULT_SOURCE
#define _DEFAULT_SOURCE 1
#endif
// Adapted from ../main query_packed.c; see QUERY_PROVENANCE.md.
// On NetBSD, defining standard requirements like this removes symbols
// from the namespace; however, we need non-standard symbols for
// endian.h.
#if defined(__NetBSD__) && defined(_POSIX_C_SOURCE)
#undef _POSIX_C_SOURCE
#endif

#include "internal.h"
#include "include/tree_sitter/squat_query.h"
#include "../src/alloc.h"
#include "../src/array.h"
#include "../src/point.h"
#include "../src/unicode.h"
#include "query_internal.h"
#include <wctype.h>

typedef struct {
  SQCursor *cursor;
} QueryTreeCursor;

static SQNode query_identity_node(SQNode node) {
  return node;
}

static SQNode query_tree_cursor_node(const QueryTreeCursor *cursor) {
  return sq_cursor_node(cursor->cursor);
}

static SQNode query_tree_cursor_parent(const QueryTreeCursor *cursor) {
  return sq_cursor_parent_node(cursor->cursor);
}

static void query_tree_cursor_delete(QueryTreeCursor *cursor) {
  sq_cursor_delete(cursor->cursor);
  cursor->cursor = NULL;
}

static void query_tree_cursor_reset(QueryTreeCursor *cursor, SQNode node) {
  query_tree_cursor_delete(cursor);
  cursor->cursor = sq_cursor_new(node);
  ts_assert(!node.tree || cursor->cursor);
}

static bool query_tree_cursor_goto_first_child(QueryTreeCursor *cursor) {
  return sq_cursor_goto_first_child(cursor->cursor);
}

static bool query_tree_cursor_goto_next_sibling(QueryTreeCursor *cursor) {
  return sq_cursor_goto_next_sibling(cursor->cursor);
}

static bool query_tree_cursor_goto_parent(QueryTreeCursor *cursor) {
  return sq_cursor_goto_parent(cursor->cursor);
}

static unsigned query_ctz(uint64_t bits) {
  return (unsigned)__builtin_ctzll(bits);
}

static TSSymbol query_decode_symbol(uint32_t raw, uint32_t count) {
  return raw == count       ? ts_builtin_sym_error
         : raw == count + 1 ? ts_builtin_sym_error_repeat
                            : (TSSymbol)raw;
}

// #define DEBUG_ANALYZE_QUERY
// #define DEBUG_EXECUTE_QUERY
// #define DEBUG_QUERY_STEPS

#if defined(DEBUG_QUERY_STEPS) || defined(DEBUG_ANALYZE_QUERY) || defined(DEBUG_EXECUTE_QUERY)
#define DEBUG_DUMP_STEPS
#endif

#define MAX_STEP_CAPTURE_COUNT 3
#define MAX_NEGATED_FIELD_COUNT 8
#define MAX_STATE_PREDECESSOR_COUNT 256
#define MAX_ANALYSIS_STATE_DEPTH 8
#define MAX_ANALYSIS_ITERATION_COUNT 256

// Stream - A sequence of unicode characters derived from a UTF8 string.
// This struct is used in parsing queries from S-expressions.
typedef struct {
  const char *input;
  const char *start;
  const char *end;
  int32_t next;
  uint8_t next_size;
} Stream;

// QueryStep - A step in the process of matching a query. Each node within
// a query S-expression corresponds to one of these steps. An entire pattern
// is represented as a sequence of these steps. The basic properties of a
// node are represented by these fields:
// - `symbol` - The grammar symbol to match. A zero value represents the
//    wildcard symbol, '_'.
// - `field` - The field name to match. A zero value means that a field name
//    was not specified.
// - `capture_ids` - An array of integers representing the names of captures
//    associated with this node in the pattern, terminated by a `NONE` value.
// - `depth` - The depth where this node occurs in the pattern. The root node
//    of the pattern has depth zero.
// - `negated_field_list_id` - An id representing a set of fields that must
//    not be present on a node matching this step.
//
// Steps have some additional fields in order to handle the `.` (or "anchor") operator,
// which forbids additional child nodes:
// - `is_immediate` - Indicates that the node matching this step cannot be preceded
//    by other sibling nodes that weren't specified in the pattern.
// - `is_last_child` - Indicates that the node matching this step cannot have any
//    subsequent named siblings.
//
// For simple patterns, steps are matched in sequential order. But in order to
// handle alternative/repeated/optional sub-patterns, query steps are not always
// structured as a linear sequence; they sometimes need to split and merge. This
// is done using the following fields:
// - `alternative_index` - The index of a different query step that serves as
//    an alternative to this step. A `NONE` value represents no alternative.
//    When a query state reaches a step with an alternative index, the state
//    is duplicated, with one copy remaining at the original step, and one copy
//    moving to the alternative step. The alternative may have its own alternative
//    step, so this splitting is an iterative process.
// - `is_dead_end` - Indicates that this state cannot be passed directly, and
//    exists only in order to redirect to an alternative index, with no splitting.
// - `is_pass_through` - Indicates that state has no matching logic of its own,
//    and exists only to split a state. One copy of the state advances immediately
//    to the next step, and one moves to the alternative step.
// - `alternative_is_skip` - Indicates that this step's `alternative_index` is the
//    forward skip introduced by a `?` or `*` quantifier (the branch taken when the
//    quantifier matches zero occurrences). For a state that follows it, an
//    immediately-following anchor is vacuous.
// - `is_inside_alternation` - Indicates that state is inside an alternation.
//    Currently only written to quantifier steps, read by logic that maintains
//    correctness for quantifiers inside alternations.
//
// Steps also store some derived state that summarizes how they relate to other
// steps within the same pattern. This is used to optimize the matching process:
// - `contains_captures` - Indicates that this step or one of its child steps
//    has a non-empty `capture_ids` list.
// - `parent_pattern_guaranteed` - Indicates that if this step is reached, then
//    it and all of its subsequent sibling steps within the same parent pattern
//    are guaranteed to match.
// - `root_pattern_guaranteed` - Similar to `parent_pattern_guaranteed`, but
//    for the entire top-level pattern. When iterating through a query's
//    captures using `sq_query_cursor_next_capture`, this field is used to
//    detect that a capture can safely be returned from a match that has not
//    even completed yet.
typedef struct {
  TSSymbol symbol;
  TSSymbol supertype_symbol;
  TSFieldId field;
  uint16_t capture_ids[MAX_STEP_CAPTURE_COUNT];
  uint16_t depth;
  uint16_t alternative_index;
  uint16_t negated_field_list_id;
  bool is_named : 1;
  bool is_immediate : 1;
  bool is_last_child : 1;
  bool is_pass_through : 1;
  bool is_dead_end : 1;
  bool is_inside_alternation : 1;
  bool contains_captures : 1;
  bool root_pattern_guaranteed : 1;
  bool parent_pattern_guaranteed : 1;
  bool is_missing : 1;
  bool alternative_is_skip : 1;
  bool is_local : 1; // a rooted step that completes without visiting another node
} QueryStep;

// Slice - A slice of an external array. Within a query, capture names,
// literal string values, and predicate step information are stored in three
// contiguous arrays. Individual captures, string values, and predicates are
// represented as slices of these three arrays.
typedef struct {
  uint32_t offset;
  uint32_t length;
} Slice;

// SymbolTable - a two-way mapping of strings to ids.
typedef struct {
  Array(char) characters;
  Array(Slice) slices;
} SymbolTable;

// CaptureQuantifiers - a data structure holding the quantifiers of pattern captures.
typedef Array(uint8_t) CaptureQuantifiers;

// PatternEntry - Information about the starting point for matching a particular
// pattern. These entries are stored in a 'pattern map' - a sorted array that
// makes it possible to efficiently lookup patterns based on the symbol for their
// first step. The entry consists of the following fields:
// - `pattern_index` - the index of the pattern within the query
// - `step_index` - the index of the pattern's first step in the shared `steps` array
// - `is_rooted` - whether or not the pattern has a single root node. This property
//   affects decisions about whether or not to start the pattern for nodes outside
//   of a QueryCursor's range restriction.
typedef struct {
  uint16_t step_index;
  uint16_t pattern_index;
  bool is_rooted;
  uint16_t presence_requirement;
} PatternEntry;

typedef struct {
  Slice steps;
  Slice predicate_steps;
  uint32_t start_byte;
  uint32_t end_byte;
  bool is_non_local;
} QueryPattern;

typedef struct {
  uint32_t byte_offset;
  uint16_t step_index;
} StepOffset;

// QueryState - The state of an in-progress match of a particular pattern
// in a query. While executing, a `SQQueryCursor` must keep track of a number
// of possible in-progress matches. Each of those possible matches is
// represented as one of these states. Fields:
// - `id` - A numeric id that is exposed to the public API. This allows the
//    caller to remove a given match, preventing any more of its captures
//    from being returned.
// - `start_depth` - The depth in the tree where the first step of the state's
//    pattern was matched.
// - `pattern_index` - The pattern that the state is matching.
// - `consumed_capture_count` - The number of captures from this match that
//    have already been returned.
// - `capture_list_id` - A numeric id that can be used to retrieve the state's
//    list of captures from the `CaptureListPool`.
// - `heap_insert_order` - A sequence number used to preserve discovery order
//    among finished states with the same capture position and pattern.
// - `seeking_immediate_match` - A flag that indicates that the state's next
//    step must be matched by the very next sibling. This is used when
//    processing repetitions, or when processing a wildcard node followed by
//    an anchor.
// - `has_in_progress_alternatives` - A flag that indicates that there are
//    other states that have the same captures as this state, but are at
//    different steps in their pattern. This means that in order to obey the
//    'longest-match' rule, this state should not be returned as a match until
//    it is clear that there can be no other alternative match with more captures.
typedef struct {
  uint32_t id;
  uint32_t capture_list_id;
  uint32_t heap_insert_order;
  union {
    struct {
      uint16_t start_depth;
      uint16_t step_index;
    };
    uint32_t next_capture_byte; // finished states no longer need NFA positions
  };
  uint16_t pattern_index;
  uint16_t consumed_capture_count : 12;
  bool seeking_immediate_match : 1;
  bool has_in_progress_alternatives : 1;
  bool dead : 1;
  bool needs_parent : 1;
  bool skipped_quantifier : 1;
  bool removed : 1; // already erased by the dedup pass; dead states still await it
  bool captures_exhausted : 1;
} QueryState;

typedef Array(QueryState) QueryStateList;
typedef struct {
  uint32_t next, end, capture_count, first_start_byte;
} CaptureComparisonEntry;

typedef struct {
  uint64_t bits[128];
  uint64_t valid, common[2], combined[2];
  uint64_t cached_set[2], cached_candidates;
} CaptureComparisonBlock;

typedef struct {
  SQQueryCapture *contents;
  uint32_t size, capacity;
  union {
    uint32_t first_start_byte;
    uint32_t next_free; // size == UINT32_MAX
  };
  uint32_t last_end_byte;
  uint32_t storage_id, prefix_size;
  uint64_t prefix_id;
  uint64_t capture_hash, capture_set[2];
} CaptureList;

typedef struct {
  SQQueryCapture *contents;
  uint32_t capacity, references, next_free;
} CaptureListStorage;

// CaptureListPool - A collection of *lists* of captures. Each query state needs
// to maintain its own list of captures. To avoid repeated allocations, this struct
// maintains a fixed set of capture lists, and keeps track of which ones are
// currently in use by a query state.
typedef struct {
  Array(CaptureList) list;
  CaptureList empty_list;

  // The maximum number of capture lists that we are allowed to allocate. We
  // never allow `list` to allocate more entries than this, dropping pending
  // matches if needed to stay under the limit.
  uint32_t max_capture_list_count;

  // Unused lists have size UINT32_MAX and link through next_free.
  // Releasing a list leaves its capture storage intact.
  uint32_t free_capture_list_head;

  // Logical lists account for match limits; their buffers may be shared.
  Array(CaptureListStorage) storage;
  uint32_t free_storage_head;
  uint64_t next_prefix_id;
} CaptureListPool;

// AnalysisState - The state needed for walking the parse table when analyzing
// a query pattern, to determine at which steps the pattern might fail to match.
typedef struct {
  TSStateId parse_state;
  TSSymbol parent_symbol;
  uint16_t child_index;
  TSFieldId field_id : 15;
  bool done : 1;
} AnalysisStateEntry;

typedef struct {
  AnalysisStateEntry stack[MAX_ANALYSIS_STATE_DEPTH];
  uint16_t depth;
  uint16_t step_index;
  TSSymbol root_symbol;
} AnalysisState;

typedef Array(AnalysisState *) AnalysisStateSet;

typedef struct {
  AnalysisStateSet states;
  AnalysisStateSet next_states;
  AnalysisStateSet deeper_states;
  AnalysisStateSet state_pool;
  Array(uint16_t) final_step_indices;
  Array(TSSymbol) finished_parent_symbols;
  bool did_abort;
} QueryAnalysis;

// AnalysisSubgraph - A subset of the states in the parse table that are used
// in constructing nodes with a certain symbol. Each state is accompanied by
// some information about the possible node that could be produced in
// downstream states.
typedef struct {
  TSStateId state;
  uint16_t production_id;
  uint8_t child_index : 7;
  bool done : 1;
} AnalysisSubgraphNode;

typedef struct {
  TSSymbol symbol;
  Array(TSStateId) start_states;
  Array(AnalysisSubgraphNode) nodes;
} AnalysisSubgraph;

typedef Array(AnalysisSubgraph) AnalysisSubgraphArray;

// StatePredecessorMap - A map that stores the predecessors of each parse state.
// This is used during query analysis to determine which parse states can lead
// to which reduce actions.
typedef struct {
  TSStateId *contents;
} StatePredecessorMap;

typedef struct {
  uint64_t values[8], masks[8];
  uint64_t low_bits, high_bits;
  uint32_t width, count;
  uint16_t symbols[32];
  uint32_t symbol_count;
} QuerySymbolFilter;

// SQQuery - A tree query, compiled from a string of S-expressions. The query
// itself is immutable. The mutable state used in the process of executing the
// query is stored in a `SQQueryCursor`.
struct SQQuery {
  SymbolTable captures;
  SymbolTable predicate_values;
  Array(CaptureQuantifiers) capture_quantifiers;
  bool has_repeated_captures;
  bool needs_fields, needs_supertypes;
  Array(QueryStep) steps;
  Array(PatternEntry) pattern_map;
  Array(Slice) pattern_map_slices;
  Array(QueryPresenceRequirement) presence_requirements;
  Array(TSQueryPredicateStep) predicate_steps;
  Array(QueryPattern) patterns;
  Array(StepOffset) step_offsets;
  Array(TSFieldId) negated_fields;
  Array(char) string_buffer;
  Array(TSSymbol) repeat_symbols_with_rootless_patterns;
  const TSLanguage *language;
  uint16_t wildcard_root_pattern_count;
  Array(uint64_t) scan_symbols;
  Array(uint16_t) scan_targets;
  QuerySymbolFilter scan_filter;
  QueryExecutionPlan execution_plan;
};

// SQQueryCursor - A stateful struct used to execute a query on a tree.
typedef TSRange SQRange;

struct SQQueryCursor {
  SQQueryExecutionError error;
  const SQQuery *query;
  QueryTreeCursor cursor;
  QueryStateList states;
  QueryStateList pending_states; // empty between enter events
  Array(CaptureComparisonEntry) capture_comparison_index;
  Array(uint32_t) capture_comparison_heads;
  Array(CaptureComparisonBlock) capture_comparison_blocks;
  Array(QueryPresenceCache) presence_cache;
  QueryStateList finished_states;

  // Tracks how much of finished_states is in heap order. Elements at indices
  // < this value satisfy the min-heap property; elements >= this value are
  // newly pushed and need to be sifted into place. Only used by `next_capture`.
  uint32_t finished_states_heap_size;
  struct {
    uint32_t state_index, byte_offset, pattern_index;
    bool valid, found, definite;
  } first_capture;
  CaptureListPool capture_list_pool;
  uint32_t depth;
  uint32_t states_max_depth; // conservative upper bound, refreshed during compaction
  uint32_t max_start_depth;
  SQRange included_range;
  SQRange containing_range;
  uint32_t next_state_id;
  uint32_t next_finished_state_id;
  const TSQueryCursorOptions *query_options;
  TSQueryCursorState query_state;
  unsigned operation_count;

  // No on_visible_node flag: mainline needs one because its cursor can
  // step onto a real, steppable HIDDEN node (a composite wrapper it still
  // has to recurse through); QueryTreeCursor's goto_first_child/
  // goto_next_sibling only ever land on visible nodes at all (see
  // sq_query_cursor__should_descend's own note above), so every
  // node this cursor visits is visible unconditionally.
  bool ascending;
  bool halted;
  bool did_exceed_match_limit;
  uint64_t dirty_patterns; // pattern indices modulo 64; collisions only repeat work
  bool states_need_sort;
  bool root_has_error;
  bool symbol_scan;
  uint32_t scan_root_end;
  SQNode execution_root;
  uint32_t execution_position, execution_last_node;
  Array(QueryExecutionState) execution_states;
  uint32_t execution_free_state;
  bool execution_active;
  bool execution_needs_fallback;
  uint32_t scan_samples, scan_sparse_samples, scan_cooldown;
  QueryExecutionStats execution_stats;
};

static const TSQueryError PARENT_DONE = -1;
static const uint16_t PATTERN_DONE_MARKER = UINT16_MAX;
static const uint16_t NONE = UINT16_MAX;
static const uint32_t CAPTURE_LIST_NONE = UINT32_MAX;
static const TSSymbol WILDCARD_SYMBOL = 0;
static const unsigned OP_COUNT_PER_QUERY_CALLBACK_CHECK = 100;

void sq_query__prepare_symbol_scan(SQQuery *query);
static void sq_query__prepare_execution(SQQuery *query);
static void sq_query__prepare_steps(SQQuery *query);
static void sq_query__prepare_presence(SQQuery *query);
static bool sq_query_cursor__presence_matches(SQQueryCursor *self, const PatternEntry *pattern,
                                              SQNode root);
static void sq_query__prepare_symbol_filter(SQQuery *query);
static uint32_t query_execution_find_symbols(SQQueryCursor *cursor, const SQTree *tree,
                                             const QuerySymbolFilter *filter, uint32_t start,
                                             uint32_t end);
static void sq_query_cursor__execution_start(SQQueryCursor *cursor, SQNode root);
static void sq_query_cursor__execution_fallback(SQQueryCursor *cursor);
static void query_execution_release_state(SQQueryCursor *cursor, const QueryState *state);

// Stream

// Advance to the next unicode code point in the stream.
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

// Reset the stream to the given input position, represented as a pointer
// into the input string.
static void stream_reset(Stream *self, const char *input) {
  self->input = input;
  self->next_size = 0;
  stream_advance(self);
}

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

static bool stream_is_ident_start(Stream *self) {
  return iswalnum(self->next) || self->next == '_' || self->next == '-';
}

static void stream_scan_identifier(Stream *stream) {
  do {
    stream_advance(stream);
  } while (iswalnum(stream->next) || stream->next == '_' || stream->next == '-' ||
           stream->next == '.');
}

static uint32_t stream_offset(Stream *self) {
  return (uint32_t)(self->input - self->start);
}

// CaptureListPool

static CaptureListPool capture_list_pool_new(void) {
  return (CaptureListPool){
      .list = array_new(),
      .empty_list = {0},
      .max_capture_list_count = UINT32_MAX,
      .free_capture_list_head = CAPTURE_LIST_NONE,
      .free_storage_head = CAPTURE_LIST_NONE,
  };
}

static void capture_list_pool_reset(CaptureListPool *self) {
  for (uint32_t i = 0; i < self->list.size; i++) {
    CaptureList *list = array_get(&self->list, i);
    list->size = UINT32_MAX;
    list->next_free = i + 1 < self->list.size ? i + 1 : CAPTURE_LIST_NONE;
    list->storage_id = CAPTURE_LIST_NONE;
  }

  self->free_capture_list_head = self->list.size ? 0 : CAPTURE_LIST_NONE;
  for (uint32_t index = 0; index < self->storage.size; index++) {
    CaptureListStorage *storage = &self->storage.contents[index];
    storage->references = 0;
    storage->next_free = index + 1 < self->storage.size ? index + 1 : CAPTURE_LIST_NONE;
  }

  self->free_storage_head = self->storage.size ? 0 : CAPTURE_LIST_NONE;
  self->next_prefix_id = 0;
}

static void capture_list_pool_delete(CaptureListPool *self) {
  for (uint32_t index = 0; index < self->storage.size; index++) {
    ts_free(self->storage.contents[index].contents);
  }

  array_delete(&self->storage);
  array_delete(&self->list);
}

static void capture_list_pool_clear(CaptureListPool *self, CaptureList *list) {
  if (list->storage_id != CAPTURE_LIST_NONE) {
    CaptureListStorage *storage = &self->storage.contents[list->storage_id];
    ts_assert(storage->references);
    if (!--storage->references) {
      // Keep returned captures alive until a subsequent advancement reuses storage.
      storage->next_free = self->free_storage_head;
      self->free_storage_head = list->storage_id;
    }
  }

  *list = (CaptureList){.storage_id = CAPTURE_LIST_NONE};
}

static uint32_t capture_list_pool_make_mutable(CaptureListPool *self, CaptureList *list,
                                               uint32_t count) {
  uint32_t copied = 0;
  if (list->storage_id == CAPTURE_LIST_NONE ||
      self->storage.contents[list->storage_id].references > 1) {
    uint32_t storage_id = self->free_storage_head;
    if (storage_id == CAPTURE_LIST_NONE) {
      storage_id = self->storage.size;
      array_push(&self->storage, ((CaptureListStorage){0}));
    } else {
      self->free_storage_head = self->storage.contents[storage_id].next_free;
    }

    CaptureListStorage *storage = &self->storage.contents[storage_id];
    array_reserve(storage, list->size + count);
    if (list->size) {
      memcpy(storage->contents, list->contents, list->size * sizeof(SQQueryCapture));
      copied = list->size;
    }

    if (list->storage_id != CAPTURE_LIST_NONE) {
      self->storage.contents[list->storage_id].references--;
    }

    storage->references = 1;
    list->storage_id = storage_id;
  }

  CaptureListStorage *storage = &self->storage.contents[list->storage_id];
  if (storage->capacity < list->size + count) {
    array_reserve(storage, (list->size + count) * 2);
  }

  list->contents = storage->contents;
  list->capacity = storage->capacity;
  return copied;
}

static inline void capture_list_hash_capture(CaptureList *self, uint32_t node,
                                             uint32_t capture_id) {
  // Every capture belongs to this cursor's tree view; cache fields do not
  // participate in node identity.
  uint64_t identity = (uint64_t)node * UINT64_C(0x9e3779b185ebca87) + capture_id;
  self->capture_hash = self->capture_hash * UINT64_C(0xc2b2ae3d27d4eb4f) + identity;
  uint32_t bit = identity >> 57;
  self->capture_set[bit / 64] |= UINT64_C(1) << (bit % 64);
}

static void capture_list_pool_share(CaptureListPool *self, CaptureList *target,
                                    CaptureList *source) {
  ts_assert(!target->size && target->storage_id == CAPTURE_LIST_NONE);
  if (!source->size) {
    return;
  }

  // Unbranched histories rarely need comparison. Initialize fingerprints on
  // their first branch, and preserve them through subsequent appends.
  if (!source->prefix_id) {
    source->capture_hash = source->capture_set[0] = source->capture_set[1] = 0;
    for (uint32_t index = 0; index < source->size; index++) {
      SQQueryCapture capture = source->contents[index];
      capture_list_hash_capture(source, capture.node.slot, capture.index);
    }
  }

  // Appends preserve this prefix even after the buffers detach. A later
  // branch may establish a longer prefix; losing older provenance is safe.
  if (!source->prefix_id || source->prefix_size != source->size) {
    if (!++self->next_prefix_id) {
      for (uint32_t index = 0; index < self->list.size; index++) {
        self->list.contents[index].prefix_id = 0;
      }

      self->next_prefix_id = 1;
    }

    source->prefix_id = self->next_prefix_id;
    source->prefix_size = source->size;
  }

  *target = *source;
  self->storage.contents[source->storage_id].references++;
}

static const CaptureList *capture_list_pool_get(const CaptureListPool *self, uint32_t id) {
  if (id >= self->list.size) {
    return &self->empty_list;
  }

  return array_get(&self->list, id);
}

static CaptureList *capture_list_pool_get_mut(CaptureListPool *self, uint32_t id) {
  ts_assert(id < self->list.size);
  CaptureList *list = array_get(&self->list, id);

  // Appending or replacing captures invalidates the last bound. UINT32_MAX
  // may also be a valid end byte; that rare value is simply recomputed.
  list->last_end_byte = UINT32_MAX;
  return list;
}

static uint32_t capture_list_pool_last_end_byte(CaptureListPool *self, uint32_t id) {
  CaptureList *list = array_get(&self->list, id);
  if (list->last_end_byte == UINT32_MAX) {
    list->last_end_byte = sq_node_end_byte(array_back(list)->node);
  }

  return list->last_end_byte;
}

static bool capture_list_pool_is_empty(const CaptureListPool *self) {
  // The capture list pool is empty if all allocated lists are in use, and we
  // have reached the maximum allowed number of allocated lists.
  return self->free_capture_list_head == CAPTURE_LIST_NONE &&
         self->list.size >= self->max_capture_list_count;
}

static uint32_t capture_list_pool_acquire(CaptureListPool *self) {
  if (self->free_capture_list_head != CAPTURE_LIST_NONE) {
    uint32_t index = self->free_capture_list_head;
    CaptureList *list = array_get(&self->list, index);
    self->free_capture_list_head = list->next_free;
    *list = (CaptureList){.storage_id = CAPTURE_LIST_NONE};
    return index;
  }

  // Otherwise allocate and initialize a new capture list, as long as that
  // doesn't put us over the requested maximum.
  uint32_t i = self->list.size;
  if (i >= self->max_capture_list_count) {
    return CAPTURE_LIST_NONE;
  }

  CaptureList list = {.storage_id = CAPTURE_LIST_NONE};
  array_push(&self->list, list);
  return i;
}

static void capture_list_pool_release(CaptureListPool *self, uint32_t id) {
  if (id >= self->list.size) {
    return;
  }

  CaptureList *list = array_get(&self->list, id);
  ts_assert(list->size != UINT32_MAX);
  capture_list_pool_clear(self, list);
  list->size = UINT32_MAX;
  list->next_free = self->free_capture_list_head;
  self->free_capture_list_head = id;
}

// FinishedStateHeap
//
// A min-heap of finished query states, ordered by (byte offset of next
// unconsumed capture, pattern_index, insertion order). This allows
// sq_query_cursor_next_capture to find the earliest capture in O(1) instead
// of scanning all finished states. The heap is maintained lazily -
// sq_query_cursor__advance uses plain array_push, and next_capture sifts
// new elements into place via a tracked heap_size boundary.

static inline uint32_t sq_query__node_start_byte(SQNode node) {
  return sq_node_start_byte(node);
}

static void finished_state_swap(QueryStateList *states, uint32_t a, uint32_t b) {
  QueryState tmp = *array_get(states, a);
  *array_get(states, a) = *array_get(states, b);
  *array_get(states, b) = tmp;
}

static void finished_state_update_key(QueryState *state, const CaptureListPool *pool) {
  const CaptureList *captures = capture_list_pool_get(pool, state->capture_list_id);
  state->captures_exhausted = state->consumed_capture_count >= captures->size;
  if (!state->captures_exhausted) {
    state->next_capture_byte =
        state->consumed_capture_count
            ? sq_query__node_start_byte(captures->contents[state->consumed_capture_count].node)
            : captures->first_start_byte;
  }
}

// Compare two finished states by (byte offset of next unconsumed capture,
// pattern_index, insertion order).
static inline bool finished_state_precedes(const QueryState *a, const QueryState *b) {
  if (a->captures_exhausted) {
    return false;
  }

  if (b->captures_exhausted) {
    return true;
  }

  if (a->next_capture_byte != b->next_capture_byte) {
    return a->next_capture_byte < b->next_capture_byte;
  }

  if (a->pattern_index != b->pattern_index) {
    return a->pattern_index < b->pattern_index;
  }

  return a->heap_insert_order < b->heap_insert_order;
}

static void finished_state_sift_down(QueryStateList *states, uint32_t index) {
  uint32_t size = states->size;
  QueryState state = *array_get(states, index);
  while (true) {
    uint32_t left = 2 * index + 1;
    uint32_t right = 2 * index + 2;
    if (left >= size) {
      break;
    }

    uint32_t smallest = left;
    if (right < size &&
        finished_state_precedes(array_get(states, right), array_get(states, smallest))) {
      smallest = right;
    }

    if (!finished_state_precedes(array_get(states, smallest), &state)) {
      break;
    }

    *array_get(states, index) = *array_get(states, smallest);
    index = smallest;
  }

  *array_get(states, index) = state;
}

static void finished_state_sift_up(QueryStateList *states, uint32_t index) {
  while (index > 0) {
    uint32_t parent = (index - 1) / 2;
    if (finished_state_precedes(array_get(states, index), array_get(states, parent))) {
      finished_state_swap(states, index, parent);
      index = parent;
    } else {
      break;
    }
  }
}

static inline void finished_state_pop(QueryStateList *states) {
  if (states->size > 1) {
    *array_front(states) = *array_back(states);
  }

  states->size--;
  if (states->size > 0) {
    finished_state_sift_down(states, 0);
  }
}

// Remove an element at an arbitrary index and restore heap order.
static void finished_state_erase(QueryStateList *states, uint32_t index) {
  if (index == states->size - 1) {
    states->size--;
    return;
  }

  *array_get(states, index) = *array_back(states);
  states->size--;

  // The replacement element may need to go up or down.
  if (index > 0 &&
      finished_state_precedes(array_get(states, index), array_get(states, (index - 1) / 2))) {
    finished_state_sift_up(states, index);
  } else {
    finished_state_sift_down(states, index);
  }
}

static void sq_query_cursor__push_finished_state(SQQueryCursor *self, QueryState *state) {
  state->heap_insert_order = self->next_finished_state_id++;
  array_push(&self->finished_states, *state);
}

static void sq_query_cursor__heapify_finished_states(SQQueryCursor *self) {
  while (self->finished_states_heap_size < self->finished_states.size) {
    finished_state_update_key(&self->finished_states.contents[self->finished_states_heap_size],
                              &self->capture_list_pool);
    finished_state_sift_up(&self->finished_states, self->finished_states_heap_size);
    self->finished_states_heap_size++;
  }
}

// Quantifiers

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

  return TSQuantifierZero; // to make compiler happy, but all cases should be covered above!
}

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

  return TSQuantifierZero; // to make compiler happy, but all cases should be covered above!
}

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

  return TSQuantifierZero; // to make compiler happy, but all cases should be covered above!
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

// Return capture quantifier for the given capture id
static TSQuantifier capture_quantifier_for_id(const CaptureQuantifiers *self, uint16_t id) {
  return (self->size <= id) ? TSQuantifierZero : (TSQuantifier)*array_get(self, id);
}

// Add the given quantifier to the current value for id
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

// Join the given quantifier with the current values
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

// SymbolTable

static SymbolTable symbol_table_new(void) {
  return (SymbolTable){
      .characters = array_new(),
      .slices = array_new(),
  };
}

static void symbol_table_delete(SymbolTable *self) {
  array_delete(&self->characters);
  array_delete(&self->slices);
}

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

static const char *symbol_table_name_for_id(const SymbolTable *self, uint16_t id,
                                            uint32_t *length) {
  Slice slice = *(array_get(&self->slices, id));
  *length = slice.length;
  return array_get(&self->characters, slice.offset);
}

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

// QueryStep

static QueryStep query_step__new(TSSymbol symbol, uint16_t depth, bool is_immediate) {
  QueryStep step = {
      .symbol = symbol,
      .depth = depth,
      .alternative_index = NONE,
      .is_immediate = is_immediate,
  };
  for (unsigned i = 0; i < MAX_STEP_CAPTURE_COUNT; i++) {
    step.capture_ids[i] = NONE;
  }

  return step;
}

static void query_step__add_capture(QueryStep *self, uint16_t capture_id) {
  for (unsigned i = 0; i < MAX_STEP_CAPTURE_COUNT; i++) {
    if (self->capture_ids[i] == NONE) {
      self->capture_ids[i] = capture_id;
      break;
    }
  }
}

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

// StatePredecessorMap

static inline StatePredecessorMap state_predecessor_map_new(const TSLanguage *language) {
  return (StatePredecessorMap){
      .contents = ts_calloc((size_t)language->state_count * (MAX_STATE_PREDECESSOR_COUNT + 1),
                            sizeof(TSStateId)),
  };
}

static inline void state_predecessor_map_delete(StatePredecessorMap *self) {
  ts_free(self->contents);
}

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

static inline const TSStateId *state_predecessor_map_get(const StatePredecessorMap *self,
                                                         TSStateId state, unsigned *count) {
  size_t index = (size_t)state * (MAX_STATE_PREDECESSOR_COUNT + 1);
  *count = self->contents[index];
  return &self->contents[index + 1];
}

// AnalysisState

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

static inline AnalysisStateEntry *analysis_state__top(AnalysisState *self) {
  if (self->depth == 0) {
    return &self->stack[0];
  }

  return &self->stack[self->depth - 1];
}

static inline bool analysis_state__has_supertype(AnalysisState *self, TSSymbol symbol) {
  for (unsigned i = 0; i < self->depth; i++) {
    if (self->stack[i].parent_symbol == symbol) {
      return true;
    }
  }

  return false;
}

// AnalysisStateSet

// Obtains an `AnalysisState` instance, either by consuming one from this set's object pool, or by
// cloning one from scratch.
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

// Inserts a clone of the passed-in item at the appropriate position to maintain ordering in this
// set. The set does not contain duplicates, so if the item is already present, it will not be
// inserted, and no clone will be made.
//
// The caller retains ownership of the passed-in memory. However, the clone that is created by this
// function will be managed by the state set.
static inline void analysis_state_set__insert_sorted(AnalysisStateSet *self, AnalysisStateSet *pool,
                                                     AnalysisState *borrowed_item) {
  unsigned index, exists;
  array_search_sorted_with(self, analysis_state__compare, &borrowed_item, &index, &exists);
  if (!exists) {
    AnalysisState *new_item = analysis_state_pool__clone_or_reuse(pool, borrowed_item);
    array_insert(self, index, new_item);
  }
}

// Inserts a clone of the passed-in item at the end position of this list.
//
// IMPORTANT: The caller MUST ENSURE that this item is larger (by the comparison function
// `analysis_state__compare`) than largest item already in this set. If items are inserted in the
// wrong order, the set will not function properly for future use.
//
// The caller retains ownership of the passed-in memory. However, the clone that is created by this
// function will be managed by the state set.
static inline void analysis_state_set__push(AnalysisStateSet *self, AnalysisStateSet *pool,
                                            AnalysisState *borrowed_item) {
  AnalysisState *new_item = analysis_state_pool__clone_or_reuse(pool, borrowed_item);
  array_push(self, new_item);
}

// Removes all items from this set, returning it to an empty state.
static inline void analysis_state_set__clear(AnalysisStateSet *self, AnalysisStateSet *pool) {
  array_push_all(pool, self);
  array_clear(self);
}

// Releases all memory that is managed with this state set, including any items currently present.
// After calling this function, the set is no longer suitable for use.
static inline void analysis_state_set__delete(AnalysisStateSet *self) {
  for (unsigned i = 0; i < self->size; i++) {
    ts_free(self->contents[i]);
  }

  array_delete(self);
}

// QueryAnalyzer

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

static inline void query_analysis__delete(QueryAnalysis *self) {
  analysis_state_set__delete(&self->states);
  analysis_state_set__delete(&self->next_states);
  analysis_state_set__delete(&self->deeper_states);
  analysis_state_set__delete(&self->state_pool);
  array_delete(&self->final_step_indices);
  array_delete(&self->finished_parent_symbols);
}

// AnalysisSubgraphNode

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

// Query

// The `pattern_map` contains a mapping from TSSymbol values to indices in the
// `steps` array. For a given syntax node, the `pattern_map` makes it possible
// to quickly find the starting steps of all of the patterns whose root matches
// that node. Each entry has two fields: a `pattern_index`, which identifies one
// of the patterns in the query, and a `step_index`, which indicates the start
// offset of that pattern's steps within the `steps` array.
//
// The entries are sorted by the patterns' root symbols, and lookups use a
// binary search. This ensures that the cost of this initial lookup step
// scales logarithmically with the number of patterns in the query.
//
// This returns `true` if the symbol is present and `false` otherwise.
// If the symbol is not present `*result` is set to the index where the
// symbol should be inserted.
static inline bool sq_query__pattern_map_search(const SQQuery *self, TSSymbol needle,
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
static inline void sq_query__pattern_map_insert(SQQuery *self, TSSymbol symbol,
                                                PatternEntry new_entry) {
  uint32_t index;
  sq_query__pattern_map_search(self, symbol, &index);

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

static void sq_query__index_pattern_map(SQQuery *self) {
  uint32_t symbol_count = self->language->symbol_count + self->language->alias_count;
  array_clear(&self->pattern_map_slices);
  array_grow_by(&self->pattern_map_slices, symbol_count + 1);
  memset(self->pattern_map_slices.contents, 0, self->pattern_map_slices.size * sizeof(Slice));
  for (uint32_t index = self->wildcard_root_pattern_count; index < self->pattern_map.size;
       index++) {
    TSSymbol symbol = self->steps.contents[self->pattern_map.contents[index].step_index].symbol;
    Slice *slice =
        &self->pattern_map_slices.contents[symbol == ts_builtin_sym_error ? symbol_count : symbol];
    if (!slice->length) {
      slice->offset = index;
    }

    slice->length++;
  }
}

// Walk the subgraph for this non-terminal, tracking all of the possible
// sequences of progress within the pattern.
static void sq_query__perform_analysis(SQQuery *self, const AnalysisSubgraphArray *subgraphs,
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
              if (step->is_named && !self->language->symbol_metadata[visible_symbol].named) {
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
            if (next_step->is_pass_through) {
              next_state.step_index++;
              next_step++;
              continue;
            }

            // If the pattern is finished or hypothetical parent node is complete, then
            // record that matching can terminate at this step of the pattern. Otherwise,
            // add this state to the list of states to process on the next iteration.
            if (!next_step->is_dead_end) {
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
static void sq_query__dump_steps(const SQQuery *self, const char *label) {
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

    if (s->is_immediate) {
      printf(" IMM");
    }

    if (s->is_pass_through) {
      printf(" PASS");
    }

    if (s->is_dead_end) {
      printf(" DEAD");
    }

    if (s->is_last_child) {
      printf(" LAST");
    }

    if (s->is_named) {
      printf(" NAMED");
    }

    if (s->is_missing) {
      printf(" MISSING");
    }

    if (s->is_inside_alternation) {
      printf(" INALT");
    }

    if (s->contains_captures) {
      printf(" HASCAP");
    }

    if (s->parent_pattern_guaranteed) {
      printf(" PPG");
    }

    if (s->root_pattern_guaranteed) {
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

static bool sq_query__analyze_patterns(SQQuery *self, unsigned *error_offset) {
  Array(uint16_t) non_rooted_pattern_start_steps = array_new();
  for (unsigned i = 0; i < self->pattern_map.size; i++) {
    PatternEntry *pattern = array_get(&self->pattern_map, i);
    if (!pattern->is_rooted) {
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
      step->parent_pattern_guaranteed = true;
      step->root_pattern_guaranteed = true;
      continue;
    }

    bool has_children = false;
    bool is_wildcard = step->symbol == WILDCARD_SYMBOL;
    step->contains_captures = step->capture_ids[0] != NONE;
    for (unsigned j = i + 1; j < self->steps.size; j++) {
      QueryStep *next_step = array_get(&self->steps, j);
      if (next_step->depth == PATTERN_DONE_MARKER || next_step->depth <= step->depth) {
        break;
      }

      if (next_step->capture_ids[0] != NONE) {
        step->contains_captures = true;
      }

      if (!is_wildcard) {
        next_step->root_pattern_guaranteed = true;
        next_step->parent_pattern_guaranteed = true;
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
    sq_query__perform_analysis(self, &subgraphs, &analysis);

    // If this pattern could not be fully analyzed, then every step should
    // be considered fallible.
    if (analysis.did_abort) {
      for (unsigned j = parent_step_index + 1; j < self->steps.size; j++) {
        QueryStep *step = array_get(&self->steps, j);
        if (step->depth <= parent_depth || step->depth == PATTERN_DONE_MARKER) {
          break;
        }

        if (!step->is_dead_end) {
          step->parent_pattern_guaranteed = false;
          step->root_pattern_guaranteed = false;
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
      if (step->depth != PATTERN_DONE_MARKER && step->depth > parent_depth && !step->is_dead_end) {
        step->parent_pattern_guaranteed = false;
        step->root_pattern_guaranteed = false;
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
      TSQueryPredicateStep *step = array_get(&self->predicate_steps, j);
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
          step->root_pattern_guaranteed = false;
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
        if (step->root_pattern_guaranteed) {
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
        if (!prev_step->is_dead_end && prev_step->depth != PATTERN_DONE_MARKER &&
            prev_step->root_pattern_guaranteed) {
          prev_step->root_pattern_guaranteed = false;
          done = false;
        }
      }
    }
  }

#ifdef DEBUG_ANALYZE_QUERY
  sq_query__dump_steps(self, "analysis");
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

    sq_query__perform_analysis(self, &subgraphs, &analysis);

    if (analysis.finished_parent_symbols.size > 0) {
      array_get(&self->patterns, pattern_entry->pattern_index)->is_non_local = true;
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

  // Cleanup
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

static void sq_query__add_negated_fields(SQQuery *self, uint16_t step_index, TSFieldId *field_ids,
                                         uint16_t field_count) {
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

static TSQueryError sq_query__parse_string_literal(SQQuery *self, Stream *stream) {
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

// Parse a single predicate associated with a pattern, adding it to the
// query's internal `predicate_steps` array. Predicates are arbitrary
// S-expressions associated with a pattern which are meant to be handled at
// a higher level of abstraction, such as the Rust/JavaScript bindings. They
// can contain '@'-prefixed capture names, double-quoted strings, and bare
// symbols, which also represent strings.
static TSQueryError sq_query__parse_predicate(SQQuery *self, Stream *stream) {
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
  array_push(&self->predicate_steps, ((TSQueryPredicateStep){
                                         .type = TSQueryPredicateStepTypeString,
                                         .value_id = id,
                                     }));
  stream_skip_whitespace(stream);

  for (;;) {
    if (stream->next == ')') {
      stream_advance(stream);
      stream_skip_whitespace(stream);
      array_push(&self->predicate_steps, ((TSQueryPredicateStep){
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

      array_push(&self->predicate_steps, ((TSQueryPredicateStep){
                                             .type = TSQueryPredicateStepTypeCapture,
                                             .value_id = capture_id,
                                         }));
    }

    // Parse a string literal
    else if (stream->next == '"') {
      TSQueryError e = sq_query__parse_string_literal(self, stream);
      if (e) {
        return e;
      }

      uint16_t query_id = symbol_table_insert_name(
          &self->predicate_values, self->string_buffer.contents, self->string_buffer.size);
      array_push(&self->predicate_steps, ((TSQueryPredicateStep){
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
      array_push(&self->predicate_steps, ((TSQueryPredicateStep){
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

// Read one S-expression pattern from the stream, and incorporate it into
// the query's internal state machine representation. For nested patterns,
// this function calls itself recursively.
//
// The caller is responsible for passing in a dedicated CaptureQuantifiers.
// These should not be shared between different calls to sq_query__parse_pattern!
static TSQueryError sq_query__parse_pattern(SQQuery *self, Stream *stream, uint32_t depth,
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
      TSQueryError e = sq_query__parse_pattern(self, stream, depth, is_immediate, true,
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
      end_step->is_dead_end = true;
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

        TSQueryError e = sq_query__parse_pattern(self, stream, depth, child_is_immediate,
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
      return sq_query__parse_predicate(self, stream);
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
            TSQueryError e = sq_query__parse_string_literal(self, stream);
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
        step->is_missing = true;
      }

      if (symbol == WILDCARD_SYMBOL) {
        step->is_named = true;
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
          TSQueryError e = sq_query__parse_string_literal(self, stream);
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

          // Keep the field ids sorted.
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
        TSQueryError e = sq_query__parse_pattern(self, stream, depth + 1, child_is_immediate,
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
              last_child_step->is_last_child = true;
              if (last_child_step->alternative_index != NONE &&
                  last_child_step->alternative_index < self->steps.size) {
                QueryStep *alternative_step =
                    array_get(&self->steps, last_child_step->alternative_index);
                alternative_step->is_last_child = true;
                while (alternative_step->alternative_index != NONE &&
                       alternative_step->alternative_index < self->steps.size) {
                  alternative_step = array_get(&self->steps, alternative_step->alternative_index);
                  alternative_step->is_last_child = true;
                }
              }
            }

            if (negated_field_count) {
              sq_query__add_negated_fields(self, starting_step_index, negated_field_ids,
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
    TSQueryError e = sq_query__parse_string_literal(self, stream);
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
    TSQueryError e = sq_query__parse_pattern(self, stream, depth, is_immediate,
                                             is_inside_alternation, &field_capture_quantifiers);
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

  // Parse suffixes modifiers for this pattern
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

  QueryStep repeat_step;
  QueryStep *step;
  switch (quantifier) {
  case TSQuantifierOneOrMore:
    repeat_step = query_step__new(WILDCARD_SYMBOL, depth, false);
    repeat_step.is_inside_alternation = is_inside_alternation;
    repeat_step.alternative_index = starting_step_index;
    repeat_step.is_pass_through = true;
    array_push(&self->steps, repeat_step);
    break;
  case TSQuantifierZeroOrMore:
    repeat_step = query_step__new(WILDCARD_SYMBOL, depth, false);
    repeat_step.is_inside_alternation = is_inside_alternation;
    repeat_step.alternative_index = starting_step_index;
    repeat_step.is_pass_through = true;
    array_push(&self->steps, repeat_step);

    // Stop when `step->alternative_index` is `NONE` or it points to
    // `repeat_step` or beyond. Note that having just been pushed,
    // `repeat_step` occupies slot `self->steps.size - 1`.
    step = array_get(&self->steps, starting_step_index);
    while (step->alternative_index != NONE && step->alternative_index < self->steps.size - 1) {
      step = array_get(&self->steps, step->alternative_index);
    }

    step->alternative_index = self->steps.size;
    step->alternative_is_skip = true;
    break;
  case TSQuantifierZeroOrOne:
    step = array_get(&self->steps, starting_step_index);
    while (step->alternative_index != NONE && step->alternative_index < self->steps.size) {
      step = array_get(&self->steps, step->alternative_index);
    }

    step->alternative_index = self->steps.size;
    step->alternative_is_skip = true;
    break;
  default:
    break;
  }

  capture_quantifiers_mul(capture_quantifiers, quantifier);

  return 0;
}

SQQuery *sq_query_new(const TSLanguage *language, const char *source, uint32_t source_len,
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
                   .is_non_local = false,
               }));
    CaptureQuantifiers capture_quantifiers = capture_quantifiers_new();
    *error_type = sq_query__parse_pattern(self, &stream, 0, false, false, &capture_quantifiers);
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
      sq_query_delete(self);
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
            !second_step->is_immediate) {
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
        if (child_step->is_dead_end) {
          break;
        }

        if (child_step->depth == start_depth) {
          is_rooted = false;
          break;
        }
      }

      sq_query__pattern_map_insert(self, step->symbol,
                                   (PatternEntry){.step_index = start_step_index,
                                                  .pattern_index = pattern_index,
                                                  .is_rooted = is_rooted});
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

    // Fix up quantifier loop-backs within alternations. When a branch of an
    // alternation has a + or * quantifier, the quantifier's pass_through step
    // loops back to the branch's first step. However, the alternation linking
    // assigns that same step's `alternative_index` to point to the _next_ branch.
    // This causes the quantifier loop to incorrectly explore other alternation branches,
    // when a quantified branch matches, loops back, and then fails to match. To correct
    // this, we create "clean" copies of the branches' first steps without the link to the
    // next branch. After a quantified branch matches, it loops back to the cleaned copy.
    {
      uint32_t pat_start = pattern->steps.offset;
      uint32_t pat_end = pat_start + pattern->steps.length - 1; // exclude DONE

      for (uint32_t i = pat_start; i < pat_end; i++) {
        QueryStep *s = array_get(&self->steps, i);

        // Ensure this step is a pass_through with a _backward_ alternative (a quantifier loop-back)
        if (!s->is_pass_through || !s->is_inside_alternation || s->alternative_index == NONE ||
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
        redirect.is_dead_end = true;
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
  sq_query__dump_steps(self, "post-parse");
#endif

  if (!sq_query__analyze_patterns(self, error_offset)) {
    *error_type = TSQueryErrorStructure;
    sq_query_delete(self);
    return NULL;
  }

#ifdef DEBUG_DUMP_STEPS
  sq_query__dump_steps(self, "post-analysis");
#endif

  array_delete(&self->string_buffer);
  sq_query__index_pattern_map(self);
  sq_query__prepare_steps(self);
  sq_query__prepare_presence(self);
  sq_query__prepare_symbol_scan(self);
  return self;
}

void sq_query_delete(SQQuery *self) {
  if (self) {
    array_delete(&self->scan_symbols);
    array_delete(&self->scan_targets);
    array_delete(&self->execution_plan.steps);
    array_delete(&self->execution_plan.roots);
    array_delete(&self->steps);
    array_delete(&self->pattern_map);
    array_delete(&self->pattern_map_slices);
    array_delete(&self->presence_requirements);
    array_delete(&self->predicate_steps);
    array_delete(&self->patterns);
    array_delete(&self->step_offsets);
    array_delete(&self->string_buffer);
    array_delete(&self->negated_fields);
    array_delete(&self->repeat_symbols_with_rootless_patterns);
    ts_language_delete(self->language);
    symbol_table_delete(&self->captures);
    symbol_table_delete(&self->predicate_values);
    for (uint32_t index = 0; index < self->capture_quantifiers.size; index++) {
      CaptureQuantifiers *capture_quantifiers = array_get(&self->capture_quantifiers, index);
      capture_quantifiers_delete(capture_quantifiers);
    }

    array_delete(&self->capture_quantifiers);
    ts_free(self);
  }
}

SQQuery *sq_query_copy(const SQQuery *self) {
  SQQuery *copy = ts_malloc(sizeof(SQQuery));
  *copy = (SQQuery){
      .captures = symbol_table_new(),
      .predicate_values = symbol_table_new(),
      .language = ts_language_copy(self->language),
      .wildcard_root_pattern_count = self->wildcard_root_pattern_count,
      .has_repeated_captures = self->has_repeated_captures,
      .needs_fields = self->needs_fields,
      .needs_supertypes = self->needs_supertypes,
      .scan_filter = self->scan_filter,
  };

  if (self->scan_symbols.size) {
    array_assign(&copy->scan_symbols, &self->scan_symbols);
  }

  if (self->scan_targets.size) {
    array_assign(&copy->scan_targets, &self->scan_targets);
  }

  if (self->steps.size) {
    array_assign(&copy->steps, &self->steps);
  }

  if (self->pattern_map.size) {
    array_assign(&copy->pattern_map, &self->pattern_map);
  }

  if (self->pattern_map_slices.size) {
    array_assign(&copy->pattern_map_slices, &self->pattern_map_slices);
  }

  if (self->presence_requirements.size) {
    array_assign(&copy->presence_requirements, &self->presence_requirements);
  }

  if (self->predicate_steps.size) {
    array_assign(&copy->predicate_steps, &self->predicate_steps);
  }

  if (self->patterns.size) {
    array_assign(&copy->patterns, &self->patterns);
  }

  if (self->step_offsets.size) {
    array_assign(&copy->step_offsets, &self->step_offsets);
  }

  if (self->negated_fields.size) {
    array_assign(&copy->negated_fields, &self->negated_fields);
  }

  if (self->string_buffer.size) {
    array_assign(&copy->string_buffer, &self->string_buffer);
  }

  if (self->repeat_symbols_with_rootless_patterns.size) {
    array_assign(&copy->repeat_symbols_with_rootless_patterns,
                 &self->repeat_symbols_with_rootless_patterns);
  }

  if (self->captures.characters.size) {
    array_assign(&copy->captures.characters, &self->captures.characters);
  }

  if (self->captures.slices.size) {
    array_assign(&copy->captures.slices, &self->captures.slices);
  }

  if (self->predicate_values.characters.size) {
    array_assign(&copy->predicate_values.characters, &self->predicate_values.characters);
  }

  if (self->predicate_values.slices.size) {
    array_assign(&copy->predicate_values.slices, &self->predicate_values.slices);
  }

  if (self->capture_quantifiers.size) {
    array_assign(&copy->capture_quantifiers, &self->capture_quantifiers);
  }

  for (uint32_t i = 0; i < copy->capture_quantifiers.size; i++) {
    CaptureQuantifiers *dst = array_get(&copy->capture_quantifiers, i);
    const CaptureQuantifiers *src = array_get(&self->capture_quantifiers, i);
    *dst = capture_quantifiers_new();
    if (src->size) {
      array_assign(dst, src);
    }
  }

  if (self->execution_plan.supported) {
    sq_query__prepare_execution(copy);
  }

  return copy;
}

uint32_t sq_query_pattern_count(const SQQuery *self) {
  return self->patterns.size;
}

uint32_t sq_query_capture_count(const SQQuery *self) {
  return self->captures.slices.size;
}

uint32_t sq_query_string_count(const SQQuery *self) {
  return self->predicate_values.slices.size;
}

const char *sq_query_capture_name_for_id(const SQQuery *self, uint32_t index, uint32_t *length) {
  return symbol_table_name_for_id(&self->captures, index, length);
}

TSQuantifier sq_query_capture_quantifier_for_id(const SQQuery *self, uint32_t pattern_index,
                                                uint32_t capture_index) {
  CaptureQuantifiers *capture_quantifiers = array_get(&self->capture_quantifiers, pattern_index);
  return capture_quantifier_for_id(capture_quantifiers, capture_index);
}

const char *sq_query_string_value_for_id(const SQQuery *self, uint32_t index, uint32_t *length) {
  return symbol_table_name_for_id(&self->predicate_values, index, length);
}

const TSQueryPredicateStep *
sq_query_predicates_for_pattern(const SQQuery *self, uint32_t pattern_index, uint32_t *step_count) {
  Slice slice = array_get(&self->patterns, pattern_index)->predicate_steps;
  *step_count = slice.length;
  if (slice.length == 0) {
    return NULL;
  }

  return array_get(&self->predicate_steps, slice.offset);
}

uint32_t sq_query_start_byte_for_pattern(const SQQuery *self, uint32_t pattern_index) {
  return array_get(&self->patterns, pattern_index)->start_byte;
}

uint32_t sq_query_end_byte_for_pattern(const SQQuery *self, uint32_t pattern_index) {
  return array_get(&self->patterns, pattern_index)->end_byte;
}

bool sq_query_is_pattern_rooted(const SQQuery *self, uint32_t pattern_index) {
  for (unsigned i = 0; i < self->pattern_map.size; i++) {
    PatternEntry *entry = array_get(&self->pattern_map, i);
    if (entry->pattern_index == pattern_index) {
      if (!entry->is_rooted) {
        return false;
      }
    }
  }

  return true;
}

bool sq_query_is_pattern_non_local(const SQQuery *self, uint32_t pattern_index) {
  if (pattern_index < self->patterns.size) {
    return array_get(&self->patterns, pattern_index)->is_non_local;
  } else {
    return false;
  }
}

bool sq_query_is_pattern_guaranteed_at_step(const SQQuery *self, uint32_t byte_offset) {
  uint32_t step_index = UINT32_MAX;
  for (unsigned i = 0; i < self->step_offsets.size; i++) {
    StepOffset *step_offset = array_get(&self->step_offsets, i);
    if (step_offset->byte_offset > byte_offset) {
      break;
    }

    step_index = step_offset->step_index;
  }

  if (step_index < self->steps.size) {
    return array_get(&self->steps, step_index)->root_pattern_guaranteed;
  } else {
    return false;
  }
}

bool sq_query__step_is_fallible(const SQQuery *self, uint16_t step_index) {
  unsigned i = 1;
  QueryStep *step = array_get(&self->steps, step_index);
  QueryStep *next_step;
  do {
    ts_assert((uint32_t)step_index + i < self->steps.size);
    next_step = array_get(&self->steps, step_index + i);
    i++;
  } while (next_step->is_pass_through);
  return (next_step->depth != PATTERN_DONE_MARKER &&
          (next_step->depth > step->depth ||
           (next_step->depth == step->depth && next_step->is_immediate)) &&
          (!next_step->parent_pattern_guaranteed || step->symbol == WILDCARD_SYMBOL));
}

void sq_query_disable_capture(SQQuery *self, const char *name, uint32_t length) {
  // Remove capture information for any pattern step that previously
  // captured with the given name.
  int id = symbol_table_id_for_name(&self->captures, name, length);
  if (id != -1) {
    for (unsigned i = 0; i < self->steps.size; i++) {
      QueryStep *step = array_get(&self->steps, i);
      query_step__remove_capture(step, id);
    }
  }
}

void sq_query_disable_pattern(SQQuery *self, uint32_t pattern_index) {
  if (self->execution_plan.supported && pattern_index < 64) {
    uint64_t mask = ~((uint64_t)1 << pattern_index);
    for (uint32_t index = 0; index < self->execution_plan.roots.size; index++) {
      self->execution_plan.roots.contents[index] &= mask;
    }
  }

  // Remove the given pattern from the pattern map. Its steps will still
  // be in the `steps` array, but they will never be read.
  for (unsigned i = 0; i < self->pattern_map.size; i++) {
    PatternEntry *pattern = array_get(&self->pattern_map, i);
    if (pattern->pattern_index == pattern_index) {
      if (i < self->wildcard_root_pattern_count) {
        self->wildcard_root_pattern_count--;
      }

      array_erase(&self->pattern_map, i);
      i--;
    }
  }

  sq_query__index_pattern_map(self);
}

// QueryCursor

SQQueryCursor *sq_query_cursor_new(void) {
  SQQueryCursor *self = ts_malloc(sizeof(SQQueryCursor));
  *self = (SQQueryCursor){
      .did_exceed_match_limit = false,
      .ascending = false,
      .halted = false,
      .states = array_new(),
      .finished_states = array_new(),
      .capture_list_pool = capture_list_pool_new(),
      .included_range =
          {
              .start_point = {0, 0},
              .end_point = POINT_MAX,
              .start_byte = 0,
              .end_byte = UINT32_MAX,
          },
      .containing_range =
          {
              .start_point = {0, 0},
              .end_point = POINT_MAX,
              .start_byte = 0,
              .end_byte = UINT32_MAX,
          },
      .max_start_depth = UINT32_MAX,
      .symbol_scan = true,
      .operation_count = 0,
  };
  array_reserve(&self->states, 8);
  array_reserve(&self->finished_states, 8);
  return self;
}

void sq_query_cursor__enable_symbol_scan(SQQueryCursor *self) {
  self->symbol_scan = true;
}

void sq_query_cursor_delete(SQQueryCursor *self) {
  array_delete(&self->states);
  array_delete(&self->pending_states);
  array_delete(&self->capture_comparison_index);
  array_delete(&self->capture_comparison_heads);
  array_delete(&self->capture_comparison_blocks);
  array_delete(&self->presence_cache);
  array_delete(&self->finished_states);
  array_delete(&self->execution_states);
  query_tree_cursor_delete(&self->cursor);
  capture_list_pool_delete(&self->capture_list_pool);
  ts_free(self);
}

bool sq_query_cursor_did_exceed_match_limit(const SQQueryCursor *self) {
  return self->did_exceed_match_limit;
}

uint32_t sq_query_cursor_match_limit(const SQQueryCursor *self) {
  return self->capture_list_pool.max_capture_list_count;
}

void sq_query_cursor_set_match_limit(SQQueryCursor *self, uint32_t limit) {
  self->capture_list_pool.max_capture_list_count = limit;
}

#ifdef DEBUG_EXECUTE_QUERY
#define LOG(...) fprintf(stderr, __VA_ARGS__)
#else
#define LOG(...)
#endif

void sq_query__prepare_symbol_scan(SQQuery *query) {
  sq_query__prepare_execution(query);
  array_clear(&query->scan_symbols);
  array_clear(&query->scan_targets);
  query->scan_filter.count = 0;
  for (uint32_t pattern = 0; pattern < query->pattern_map.size; pattern++) {
    // Wildcards include the empty branch of root ?/* patterns. They can start
    // at every node, so skipping symbols would omit empty matches.
    if (query->steps.contents[query->pattern_map.contents[pattern].step_index].symbol ==
        WILDCARD_SYMBOL) {
      return;
    }
  }

  uint32_t count = query->language->symbol_count + query->language->alias_count;
  array_grow_by(&query->scan_symbols, (count + 2 + 63) / 64);
  memset(query->scan_symbols.contents, 0, query->scan_symbols.size * sizeof(uint64_t));
  for (uint32_t raw = 0; raw < count + 2; raw++) {
    TSSymbol symbol = query_decode_symbol(raw, count);
    if (symbol < count) {
      symbol = query->language->public_symbol_map[symbol];
    }

    unsigned pattern;
    if (sq_query__pattern_map_search(query, symbol, &pattern)) {
      query->scan_symbols.contents[raw / 64] |= (uint64_t)1 << (raw % 64);
      array_push(&query->scan_targets, raw);
    }
  }

  sq_query__prepare_symbol_filter(query);
}

void sq_query_cursor_exec(SQQueryCursor *self, const SQQuery *query, SQNode node) {
  if (query) {
    LOG("query steps:\n");
    for (unsigned i = 0; i < query->steps.size; i++) {
      QueryStep *step = array_get(&query->steps, i);
      LOG("  %u: {", i);
      if (step->depth == PATTERN_DONE_MARKER) {
        LOG("DONE");
      } else if (step->is_dead_end) {
        LOG("dead_end");
      } else if (step->is_pass_through) {
        LOG("pass_through");
      } else if (step->symbol != WILDCARD_SYMBOL) {
        LOG("symbol: %s", query->language->symbol_names[step->symbol]);
      } else {
        LOG("symbol: *");
      }

      if (step->field) {
        LOG(", field: %s", query->language->field_names[step->field]);
      }

      if (step->alternative_index != NONE) {
        LOG(", alternative: %u", step->alternative_index);
      }

      LOG("},\n");
    }
  }

  array_clear(&self->states);
  array_clear(&self->finished_states);
  self->finished_states_heap_size = 0;
  self->first_capture.valid = false;
  query_tree_cursor_reset(&self->cursor, node);
  capture_list_pool_reset(&self->capture_list_pool);
  self->next_state_id = 0;
  self->next_finished_state_id = 0;
  self->depth = 0;
  self->states_max_depth = 0;
  self->ascending = false;
  self->halted = !query || !node.tree || node.tree->language != query->language;
  self->error = self->halted ? SQ_QUERY_INVALID_EXECUTION : SQ_QUERY_OK;
  self->query = query;
  self->root_has_error = sq_node_has_error(node);
  if (self->symbol_scan && node.tree) {
    SQNode root = query_identity_node(node);
    self->scan_root_end = sq_node_end_slot(root);
  }

  self->did_exceed_match_limit = false;
  self->dirty_patterns = 0;
  self->states_need_sort = false;
  self->operation_count = 0;
  self->query_options = NULL;
  self->query_state = (TSQueryCursorState){0};
  self->scan_samples = self->scan_sparse_samples = self->scan_cooldown = 0;
  self->execution_stats = (QueryExecutionStats){0};
  array_clear(&self->presence_cache);
  if (self->symbol_scan && query) {
    array_grow_by(&self->presence_cache, query->presence_requirements.size);
  }

  self->execution_active = self->execution_needs_fallback = false;
  array_clear(&self->execution_states);
  self->execution_free_state = UINT32_MAX;
  if (self->symbol_scan) {
    sq_query_cursor__execution_start(self, node);
  }
}

void sq_query_cursor_exec_with_options(SQQueryCursor *self, const SQQuery *query, SQNode node,
                                       const TSQueryCursorOptions *query_options) {
  sq_query_cursor_exec(self, query, node);
  if (query_options) {
    self->query_options = query_options;
    self->query_state = (TSQueryCursorState){.payload = query_options->payload};
  }
}

bool sq_query_cursor_set_byte_range(SQQueryCursor *self, uint32_t start_byte, uint32_t end_byte) {
  if (end_byte == 0) {
    end_byte = UINT32_MAX;
  }

  if (start_byte > end_byte) {
    return false;
  }

  self->execution_needs_fallback = self->execution_active;
  self->included_range.start_byte = start_byte;
  self->included_range.end_byte = end_byte;
  self->first_capture.valid = false;
  return true;
}

bool sq_query_cursor_set_point_range(SQQueryCursor *self, TSPoint start_point, TSPoint end_point) {
  if (end_point.row == 0 && end_point.column == 0) {
    end_point = POINT_MAX;
  }

  if (point_gt(start_point, end_point)) {
    return false;
  }

  self->execution_needs_fallback = self->execution_active;
  self->included_range.start_point = start_point;
  self->included_range.end_point = end_point;
  self->first_capture.valid = false;
  return true;
}

bool sq_query_cursor_set_containing_byte_range(SQQueryCursor *self, uint32_t start_byte,
                                               uint32_t end_byte) {
  if (end_byte == 0) {
    end_byte = UINT32_MAX;
  }

  if (start_byte > end_byte) {
    return false;
  }

  self->execution_needs_fallback = self->execution_active;
  self->containing_range.start_byte = start_byte;
  self->containing_range.end_byte = end_byte;
  return true;
}

bool sq_query_cursor_set_containing_point_range(SQQueryCursor *self, TSPoint start_point,
                                                TSPoint end_point) {
  if (end_point.row == 0 && end_point.column == 0) {
    end_point = POINT_MAX;
  }

  if (point_gt(start_point, end_point)) {
    return false;
  }

  self->execution_needs_fallback = self->execution_active;
  self->containing_range.start_point = start_point;
  self->containing_range.end_point = end_point;
  return true;
}

static inline bool sq_query__range_is_unrestricted(const SQRange *range);

// Keep range rejection shared by both capture paths.
static inline bool sq_query__node_precedes_range(SQNode node, const SQRange *range) {
  if (sq_node_end_byte(node) <= range->start_byte) return true;
  return point_lte(sq_node_end_point(node), range->start_point);
}

static inline bool sq_query__node_follows_range(SQNode node, const SQRange *range) {
  if (sq_node_start_byte(node) >= range->end_byte) return true;
  return point_gte(sq_node_start_point(node), range->end_point);
}

static inline bool sq_query__capture_is_inside_unrestricted_range(SQNode node,
                                                                  const SQRange *range) {
  // Slab nodes carry no cached positions; retain the exact range checks below.
  (void)node;
  (void)range;
  return false;
}

// Search through all of the in-progress states, and find the captured
// node that occurs earliest in the document.
static bool sq_query_cursor__first_in_progress_capture(SQQueryCursor *self, uint32_t *state_index,
                                                       uint32_t *byte_offset,
                                                       uint32_t *pattern_index, bool *is_definite) {
  bool result = false;
  *state_index = UINT32_MAX;
  *byte_offset = UINT32_MAX;
  *pattern_index = UINT32_MAX;
  for (unsigned i = 0; i < self->states.size; i++) {
    QueryState *state = array_get(&self->states, i);
    if (state->dead) {
      continue;
    }

    const CaptureList *captures =
        capture_list_pool_get(&self->capture_list_pool, state->capture_list_id);
    if (state->consumed_capture_count >= captures->size) {
      continue;
    }

    SQNode node = array_get(captures, state->consumed_capture_count)->node;
    if (!sq_query__capture_is_inside_unrestricted_range(node, &self->included_range) &&
        sq_query__node_precedes_range(node, &self->included_range)) {
      state->consumed_capture_count++;
      i--;
      continue;
    }

    uint32_t node_start_byte = sq_query__node_start_byte(node);
    if (!result || node_start_byte < *byte_offset ||
        (node_start_byte == *byte_offset && state->pattern_index < *pattern_index)) {
      QueryStep *step = array_get(&self->query->steps, state->step_index);
      if (is_definite) {
        // We're being a bit conservative here by asserting that the following step
        // is not immediate, because this capture might end up being discarded if the
        // following symbol in the tree isn't the required symbol for this step.
        *is_definite = step->root_pattern_guaranteed && !step->is_immediate;
      } else if (step->root_pattern_guaranteed) {
        continue;
      }

      result = true;
      *state_index = i;
      *byte_offset = node_start_byte;
      *pattern_index = state->pattern_index;
    }
  }

  return result;
}

static inline bool sq_query__node_eq(SQNode left, SQNode right) {
  return sq_node_eq(left, right);
}

// Determine which node is first in a depth-first traversal
static inline int sq_query_cursor__compare_nodes(const SQNode *left, const SQNode *right) {
  if (!sq_query__node_eq(*left, *right)) {
    uint32_t left_start = sq_query__node_start_byte(*left);
    uint32_t right_start = sq_query__node_start_byte(*right);
    if (left_start < right_start) {
      return -1;
    }

    if (left_start > right_start) {
      return 1;
    }

    uint32_t left_node_count = sq_node_end_byte(*left);
    uint32_t right_node_count = sq_node_end_byte(*right);
    if (left_node_count > right_node_count) {
      return -1;
    }

    if (left_node_count < right_node_count) {
      return 1;
    }
  }

  return 0;
}

typedef struct {
  bool left_contains_right, right_contains_left;
} CaptureContainment;

// Determine whether either capture list contains the other.
CaptureContainment sq_query_cursor__compare_captures(SQQueryCursor *self,
                                                     const CaptureList *left_captures,
                                                     const CaptureList *right_captures) {
  (void)self;
  CaptureContainment result;
  result.left_contains_right = left_captures->size >= right_captures->size;
  result.right_contains_left = right_captures->size >= left_captures->size;
  QUERY_EXEC_COUNT(self, capture_comparisons, 1);
  if (left_captures->contents == right_captures->contents) {
    QUERY_EXEC_COUNT(self, capture_prefix_skips,
                     left_captures->size < right_captures->size ? left_captures->size
                                                                : right_captures->size);
    return result;
  }

  if (left_captures->prefix_id && right_captures->prefix_id) {
    // These fingerprints only reject containment. Collisions still undergo
    // exact comparison, including node identity and capture order.
    if (left_captures->size == right_captures->size) {
      if (left_captures->capture_hash != right_captures->capture_hash) {
        result.left_contains_right = result.right_contains_left = false;
      }
    } else {
      for (uint32_t word = 0; word < 2; word++) {
        result.left_contains_right &=
            (left_captures->capture_set[word] & right_captures->capture_set[word]) ==
            right_captures->capture_set[word];
        result.right_contains_left &=
            (left_captures->capture_set[word] & right_captures->capture_set[word]) ==
            left_captures->capture_set[word];
      }
    }

    if (!result.left_contains_right && !result.right_contains_left) {
      QUERY_EXEC_COUNT(self, capture_filter_rejections, 1);
      return result;
    }
  }

  // Shared provenance proves prefix equality even after buffers detach.
  // Otherwise, byte equality is sufficient; differing caches or padding
  // still use the semantic comparison below.
  unsigned shared_count = 0;
  if (left_captures->prefix_id && left_captures->prefix_id == right_captures->prefix_id) {
    shared_count = left_captures->prefix_size < right_captures->prefix_size
                       ? left_captures->prefix_size
                       : right_captures->prefix_size;
  }

  QUERY_EXEC_COUNT(self, capture_prefix_skips, shared_count);
  while (left_captures->size - shared_count >= 8 && right_captures->size - shared_count >= 8 &&
         !memcmp(left_captures->contents + shared_count, right_captures->contents + shared_count,
                 8 * sizeof(SQQueryCapture))) {
    shared_count += 8;
  }

  unsigned i = shared_count, j = shared_count;
  for (;;) {
    if (i < left_captures->size) {
      if (j < right_captures->size) {
        SQQueryCapture *left = array_get(left_captures, i);
        SQQueryCapture *right = array_get(right_captures, j);
        if (sq_query__node_eq(left->node, right->node) && left->index == right->index) {
          i++;
          j++;
        } else {
          // Equal-length lists can contain one another only if every capture
          // agrees. No node-position comparison can recover this mismatch.
          if (left_captures->size == right_captures->size) {
            result.left_contains_right = false;
            result.right_contains_left = false;
            return result;
          }

          switch (sq_query_cursor__compare_nodes(&left->node, &right->node)) {
          case -1:
            result.right_contains_left = false;
            i++;
            break;
          case 1:
            result.left_contains_right = false;
            j++;
            break;
          default:
            result.right_contains_left = false;
            result.left_contains_right = false;
            i++;
            j++;
            break;
          }

          if (!result.left_contains_right && !result.right_contains_left) {
            return result;
          }
        }
      } else {
        result.right_contains_left = false;
        break;
      }
    } else {
      if (j < right_captures->size) {
        result.left_contains_right = false;
      }

      break;
    }
  }

  return result;
}

// Order two in-progress states for the longest-match dedup pass. Within a
// (start_depth, pattern_index) group, states with no captures sort first, as they are a
// subset of every other state (so the dedup pass must always compare them). The rest
// sort by the start byte of their first capture.
static bool sq_query_cursor__state_precedes(const SQQueryCursor *self, const QueryState *a,
                                            const QueryState *b) {
  if (a->start_depth != b->start_depth) {
    return a->start_depth < b->start_depth;
  }

  if (a->pattern_index != b->pattern_index) {
    return a->pattern_index < b->pattern_index;
  }

  const CaptureList *a_caps = capture_list_pool_get(&self->capture_list_pool, a->capture_list_id);
  const CaptureList *b_caps = capture_list_pool_get(&self->capture_list_pool, b->capture_list_id);
  if ((a_caps->size == 0) != (b_caps->size == 0)) {
    return a_caps->size == 0;
  }

  if (a_caps->size == 0) {
    return false;
  }

  return a_caps->first_start_byte < b_caps->first_start_byte;
}

// Stable-sort the in-progress states with the order dictated by `sq_query_cursor__state_precedes`.
// This runs once per node, right before the dedup pass.
static void sq_query_cursor__sort_states_by_capture(SQQueryCursor *self) {
  QueryStateList *states = &self->states;
  for (uint32_t i = 1; i < states->size; i++) {
    // Fast+common path: this state is already ordered after its predecessor, so it does not need
    // to move.
    if (!sq_query_cursor__state_precedes(self, array_get(states, i), array_get(states, i - 1))) {
      continue;
    }

    QueryState key = *array_get(states, i);
    uint32_t j = i;
    do {
      *array_get(states, j) = *array_get(states, j - 1);
      j--;
    } while (j > 0 && sq_query_cursor__state_precedes(self, &key, array_get(states, j - 1)));
    *array_get(states, j) = key;
  }
}

// Bitmap setup needs many fingerprinted histories at one start to pay off.
// Smaller groups and distinct starts retain the cheaper pairwise checks.
static bool sq_query_cursor__needs_capture_comparison_blocks(const SQQueryCursor *self) {
  if (self->states.size < 256) {
    return false;
  }

  uint32_t run = 0, depth = UINT32_MAX, pattern = UINT32_MAX, start = UINT32_MAX;
  for (uint32_t index = 0; index < self->states.size; index++) {
    const QueryState *state = &self->states.contents[index];
    const CaptureList *captures =
        capture_list_pool_get(&self->capture_list_pool, state->capture_list_id);
    if (!captures->prefix_id) {
      run = 0;
      continue;
    }

    if (depth != state->start_depth || pattern != state->pattern_index ||
        start != captures->first_start_byte) {
      run = 0;
      depth = state->start_depth;
      pattern = state->pattern_index;
      start = captures->first_start_byte;
    }

    if (++run == 256) {
      return true;
    }
  }

  return false;
}

// Equal-length histories with different fingerprints cannot contain one another.
// Each entry links to its next hash bucket member and ends a run of equal sizes.
// Cached bounds avoid capture-pool reads for candidates rejected by the bitmap.
// Hash collisions only add candidates; exact comparison still decides containment.
// Indices stay fixed until the dedup pass compacts its tombstones.
static void sq_query_cursor__index_capture_lists(SQQueryCursor *self) {
  array_clear(&self->capture_comparison_index);
  array_clear(&self->capture_comparison_blocks);
  if (self->states.size < 64) {
    return;
  }

  QUERY_EXEC_COUNT(self, capture_index_entries, self->states.size);
  array_reserve(&self->capture_comparison_index, self->states.size);
  self->capture_comparison_index.size = self->states.size;
  if (sq_query_cursor__needs_capture_comparison_blocks(self)) {
    array_grow_by(&self->capture_comparison_blocks, (self->states.size + 63) / 64);
    QUERY_EXEC_COUNT(self, capture_index_blocks, self->capture_comparison_blocks.size);
    for (uint32_t index = 0; index < self->capture_comparison_blocks.size; index++) {
      self->capture_comparison_blocks.contents[index].common[0] = UINT64_MAX;
      self->capture_comparison_blocks.contents[index].common[1] = UINT64_MAX;
    }
  }

  uint32_t bucket_count = 256;
  while (bucket_count < self->states.size && bucket_count < 65536) {
    bucket_count *= 2;
  }

  array_reserve(&self->capture_comparison_heads, bucket_count);
  self->capture_comparison_heads.size = bucket_count;
  uint32_t *heads = self->capture_comparison_heads.contents;
  for (uint32_t bucket = 0; bucket < bucket_count; bucket++) {
    heads[bucket] = self->states.size;
  }

  for (uint32_t index = self->states.size; index-- > 0;) {
    const QueryState *state = &self->states.contents[index];
    const CaptureList *captures =
        capture_list_pool_get(&self->capture_list_pool, state->capture_list_id);
    CaptureComparisonEntry *entry = &self->capture_comparison_index.contents[index];
    *entry = (CaptureComparisonEntry){self->states.size, index + 1, captures->size,
                                      captures->first_start_byte};
    if (!captures->prefix_id) {
      continue;
    }

    if (self->capture_comparison_blocks.size) {
      CaptureComparisonBlock *block = &self->capture_comparison_blocks.contents[index / 64];
      uint64_t slot = UINT64_C(1) << (index % 64);
      uint64_t previous = block->valid;
      block->valid |= slot;
      for (uint32_t word = 0; word < 2; word++) {
        uint64_t differing = captures->capture_set[word] & ~block->common[word];

        // Common bits need no bitmap until one history omits them. At that
        // point every previous fingerprinted slot must be restored at once.
        if (previous) {
          for (uint64_t bits = block->common[word] & ~captures->capture_set[word]; bits;
               bits &= bits - 1) {
            block->bits[word * 64 + query_ctz(bits)] = previous;
          }
        }

        block->common[word] &= captures->capture_set[word];
        block->combined[word] |= captures->capture_set[word];
        for (uint64_t bits = differing; bits; bits &= bits - 1) {
          block->bits[word * 64 + query_ctz(bits)] |= slot;
        }
      }
    }

    uint32_t bucket =
        (captures->capture_hash ^ (captures->capture_hash >> 32)) & (bucket_count - 1);
    entry->next = heads[bucket];
    heads[bucket] = index;
    if (index + 1 < self->states.size) {
      const QueryState *next = &self->states.contents[index + 1];
      const CaptureList *next_captures =
          capture_list_pool_get(&self->capture_list_pool, next->capture_list_id);
      if (state->pattern_index == next->pattern_index && state->start_depth == next->start_depth &&
          next_captures->prefix_id && captures->size == next_captures->size) {
        entry->end = self->capture_comparison_index.contents[index + 1].end;
      }
    }
  }
}

// Apply the capture-set containment filter to 64 candidates at once. Common
// bits need one check; only varying bits need their individual slot masks.
// Unknown fingerprints remain candidates, as do hash collisions.
static uint64_t capture_comparison_block_candidates(CaptureComparisonBlock *block,
                                                    const uint64_t *capture_set) {
  // Fingerprinted histories are nonempty, so zero is an unused cache key.
  if (block->cached_set[0] == capture_set[0] && block->cached_set[1] == capture_set[1]) {
    return block->cached_candidates;
  }

  block->cached_set[0] = capture_set[0];
  block->cached_set[1] = capture_set[1];
  uint64_t subsets = block->valid, supersets = block->valid;
  for (uint32_t word = 0; word < 2 && (subsets | supersets); word++) {
    if (block->common[word] & ~capture_set[word]) {
      subsets = 0;
    }

    if (capture_set[word] & ~block->combined[word]) {
      supersets = 0;
    }

    uint64_t varying = block->combined[word] ^ block->common[word];
    uint64_t required = varying & capture_set[word];
    uint64_t forbidden = varying & ~capture_set[word];
    while (required && supersets) {
      uint32_t bit = query_ctz(required);
      required &= required - 1;
      supersets &= block->bits[word * 64 + bit];
    }

    while (forbidden && subsets) {
      uint32_t bit = query_ctz(forbidden);
      forbidden &= forbidden - 1;
      subsets &= ~block->bits[word * 64 + bit];
    }
  }

  return block->cached_candidates = subsets | supersets | ~block->valid;
}

// A capture used at most once in a pattern identifies incompatible histories:
// if every nonempty list starts with it, different starts cannot contain one
// another. Empty lists still compare against the entire group.
static bool sq_query_cursor__group_has_unique_start(const SQQueryCursor *self, uint32_t index) {
  if (self->states.size - index < 5) {
    return false;
  }

  const QueryState *first = &self->states.contents[index];
  const QueryState *later = &self->states.contents[index + 4];
  if (first->start_depth != later->start_depth || first->pattern_index != later->pattern_index) {
    return false;
  }

  uint32_t capture_id = UINT32_MAX;
  for (; index < self->states.size; index++) {
    const QueryState *state = &self->states.contents[index];
    if (state->start_depth != first->start_depth || state->pattern_index != first->pattern_index) {
      break;
    }

    const CaptureList *captures =
        capture_list_pool_get(&self->capture_list_pool, state->capture_list_id);
    if (!captures->size || state->dead) {
      continue;
    }

    if (capture_id == UINT32_MAX) {
      capture_id = captures->contents[0].index;
      TSQuantifier quantifier =
          sq_query_capture_quantifier_for_id(self->query, state->pattern_index, capture_id);
      if (quantifier != TSQuantifierOne && quantifier != TSQuantifierZeroOrOne) {
        return false;
      }
    } else if (captures->contents[0].index != capture_id) {
      return false;
    }
  }

  return capture_id != UINT32_MAX;
}

static uint32_t sq_query_cursor__add_state(SQQueryCursor *self, const PatternEntry *pattern) {
  QueryStep *step = array_get(&self->query->steps, pattern->step_index);
  uint32_t start_depth = self->depth - step->depth;

  // Keep the states array in ascending order of start_depth and pattern_index,
  // so that it can be processed more efficiently elsewhere. Usually, there is
  // no work to do here because of two facts:
  // * States with lower start_depth are naturally added first due to the
  //   order in which nodes are visited.
  // * Earlier patterns are naturally added first because of the ordering of the
  //   pattern_map data structure that's used to initiate matches.
  //
  // This loop is only needed in cases where two conditions hold:
  // * A pattern consists of more than one sibling node, so that its states
  //   remain in progress after exiting the node that started the match.
  // * The first node in the pattern matches against multiple nodes at the
  //   same depth.
  //
  // An example of this is the pattern '((comment)* (function))'. If multiple
  // `comment` nodes appear in a row, then we may initiate a new state for this
  // pattern while another state for the same pattern is already in progress.
  // If there are multiple patterns like this in a query, then this loop will
  // need to execute in order to keep the states ordered by pattern_index.
  uint32_t index = self->states.size;
  while (index > 0) {
    QueryState *prev_state = array_get(&self->states, index - 1);
    if (prev_state->start_depth < start_depth) {
      break;
    }

    if (prev_state->start_depth == start_depth) {
      // Avoid inserting an unnecessary duplicate state, which would be
      // immediately pruned by the longest-match criteria.
      if (prev_state->pattern_index == pattern->pattern_index &&
          prev_state->step_index == pattern->step_index) {
        return index - 1;
      }

      if (prev_state->pattern_index <= pattern->pattern_index) {
        break;
      }
    }

    index--;
  }

  LOG("  start state. pattern:%u, step:%u\n", pattern->pattern_index, pattern->step_index);
  QUERY_EXEC_COUNT(self, candidates, 1);
  QUERY_EXEC_COUNT(self, root_state_shifts, self->states.size - index);
  self->dirty_patterns |= UINT64_C(1) << (pattern->pattern_index % 64);
  self->states_need_sort = true;
  array_insert(&self->states, index,
               ((QueryState){
                   .id = UINT32_MAX,
                   .capture_list_id = CAPTURE_LIST_NONE,
                   .heap_insert_order = UINT32_MAX,
                   .step_index = pattern->step_index,
                   .pattern_index = pattern->pattern_index,
                   .start_depth = start_depth,
                   .consumed_capture_count = 0,
                   .seeking_immediate_match = true,
                   .has_in_progress_alternatives = false,
                   .needs_parent = step->depth == 1,
                   .dead = false,
                   .skipped_quantifier = false,
               }));
  return index;
}

// Acquire a capture list for this state. If there are no capture lists left in the
// pool, this will steal the capture list from another existing state, and mark that
// other state as 'dead'.
static CaptureList *sq_query_cursor__prepare_to_capture(SQQueryCursor *self, QueryState *state,
                                                        unsigned state_index_to_preserve) {
  if (state->capture_list_id == CAPTURE_LIST_NONE) {
    state->capture_list_id = capture_list_pool_acquire(&self->capture_list_pool);

    // If there are no capture lists left in the pool, then terminate whichever
    // state has captured the earliest node in the document, and steal its
    // capture list.
    if (state->capture_list_id == CAPTURE_LIST_NONE) {
      self->did_exceed_match_limit = true;
      uint32_t state_index, byte_offset, pattern_index;
      if (sq_query_cursor__first_in_progress_capture(self, &state_index, &byte_offset,
                                                     &pattern_index, NULL) &&
          state_index != state_index_to_preserve) {
        LOG("  abandon state. index:%u, pattern:%u, offset:%u.\n", state_index, pattern_index,
            byte_offset);
        QueryState *other_state = array_get(&self->states, state_index);
        state->capture_list_id = other_state->capture_list_id;
        other_state->capture_list_id = CAPTURE_LIST_NONE;
        other_state->dead = true;
        self->dirty_patterns |= UINT64_C(1) << (other_state->pattern_index % 64);
        self->states_need_sort = true;
        CaptureList *list =
            capture_list_pool_get_mut(&self->capture_list_pool, state->capture_list_id);
        capture_list_pool_clear(&self->capture_list_pool, list);
        list->last_end_byte = UINT32_MAX;
        return list;
      } else {
        LOG("  ran out of capture lists");
        return NULL;
      }
    }
  }

  return capture_list_pool_get_mut(&self->capture_list_pool, state->capture_list_id);
}

static void sq_query_cursor__capture(SQQueryCursor *self, QueryState *state, QueryStep *step,
                                     SQNode node) {
  if (state->dead) {
    return;
  }

  CaptureList *capture_list = sq_query_cursor__prepare_to_capture(self, state, UINT32_MAX);
  if (!capture_list) {
    state->dead = true;
    return;
  }

  SQNode captured_node = query_identity_node(node);
  uint32_t copied = capture_list_pool_make_mutable(&self->capture_list_pool, capture_list,
                                                   MAX_STEP_CAPTURE_COUNT);
  QUERY_EXEC_COUNT(self, capture_copies, copied);
  (void)copied;
  if (!capture_list->size) {
    capture_list->first_start_byte = sq_query__node_start_byte(captured_node);
    self->states_need_sort = true;
  }

  for (unsigned j = 0; j < MAX_STEP_CAPTURE_COUNT; j++) {
    uint16_t capture_id = step->capture_ids[j];
    if (step->capture_ids[j] == NONE) {
      break;
    }

    QUERY_EXEC_COUNT(self, materialized_captures, 1);
    array_push(capture_list, ((SQQueryCapture){captured_node, capture_id}));
    if (capture_list->prefix_id) {
      capture_list_hash_capture(capture_list, node.slot, capture_id);
    }

    LOG("  capture node. type:%s, pattern:%u, capture_id:%u, capture_count:%u\n",
        sq_node_type(node), state->pattern_index, capture_id, capture_list->size);
  }
}

// Stage the remaining input before an insertion or removal would shift a large
// suffix. New branches then move only the current state's other branches. Pending
// states are not eviction candidates, so finite limits may retain a different
// subset than an unstaged execution.
static void sq_query_cursor__stage_remaining_states(SQQueryCursor *self, uint32_t index) {
  uint32_t remaining = self->states.size - index - 1;
  if (remaining >= 32 && !self->pending_states.size) {
    array_extend(&self->pending_states, remaining, self->states.contents + index + 1);
    self->states.size = index + 1;
    QUERY_EXEC_COUNT(self, staged_states, remaining);
  }
}

// Duplicate the given state and insert the newly-created state immediately after
// the given state in the `states` array. Ensures that the given state reference is
// still valid, even if the states array is reallocated.
static QueryState *sq_query_cursor__copy_state(SQQueryCursor *self, QueryState **state_ref) {
  const QueryState *state = *state_ref;
  uint32_t state_index = (uint32_t)(state - self->states.contents);
  QUERY_EXEC_COUNT(self, branches, 1);
  QueryState copy = *state;
  copy.capture_list_id = CAPTURE_LIST_NONE;

  // If the state has captures, copy its capture list.
  if (state->capture_list_id != CAPTURE_LIST_NONE) {
    CaptureList *new_captures = sq_query_cursor__prepare_to_capture(self, &copy, state_index);
    if (!new_captures) {
      return NULL;
    }

    CaptureList *old_captures = &self->capture_list_pool.list.contents[state->capture_list_id];
    QUERY_EXEC_COUNT(self, capture_shares, old_captures->size);
    capture_list_pool_share(&self->capture_list_pool, new_captures, old_captures);
  }

  array_insert(&self->states, state_index + 1, copy);
  *state_ref = array_get(&self->states, state_index);
  return array_get(&self->states, state_index + 1);
}

// Fields and supertypes are already resolved through hidden wrappers in the
// packed columns. Sibling anchors use visible siblings, which are flattened
// under the same parent.
static void sq_query_cursor__current_status(const QueryTreeCursor *cursor, const SQQuery *query,
                                            TSSymbol *symbol, bool *is_named, TSFieldId *field,
                                            TSSymbol *supertypes, unsigned *supertype_count) {
  SQNode node = query_tree_cursor_node(cursor);
  TSSymbol raw = sq_decode_symbol(node.tree, sq_node_symbol_id(node));
  *symbol = ts_language_public_symbol(node.tree->language, raw);
  *is_named = ts_language_symbol_metadata(node.tree->language, raw).named;
  *field = query->needs_fields && sq_cursor_depth(cursor->cursor) ? sq_node_field_id(node) : 0;
  (void)supertypes;

  // Exact supertype membership is tested against the slab below, without an
  // ancestor-list truncation. This flag is only a conservative root-start gate.
  *supertype_count = query->needs_supertypes ? 1 : 0;
}

typedef struct {
  bool checked, is_error;
} QueryParentStatus;

static bool sq_query_cursor__parent_is_error(const QueryTreeCursor *self,
                                             QueryParentStatus *status) {
  if (!status->checked) {
    SQNode parent = query_tree_cursor_parent(self);
    status->is_error = !sq_node_is_null(parent) && sq_node_symbol(parent) == ts_builtin_sym_error;
    status->checked = true;
  }

  return status->is_error;
}

typedef struct {
  bool checked, has_later, has_later_named;
} QuerySiblingStatus;

static bool sq_query_cursor__has_later_sibling(const QueryTreeCursor *cursor,
                                               QuerySiblingStatus *status, bool named) {
  if (!status->checked) {
    status->checked = true;
    if (sq_cursor_depth(cursor->cursor)) {
      for (SQNode node = sq_node_next_sibling_including_empty(query_tree_cursor_node(cursor));
           node.tree; node = sq_node_next_sibling_including_empty(node)) {
        status->has_later = true;
        if (sq_node_is_named(node)) {
          status->has_later_named = true;
          break;
        }
      }
    }
  }

  return named ? status->has_later_named : status->has_later;
}

// Resolved fields include hidden-wrapper inheritance, so the visible parent's
// grammar field map cannot answer this (e.g. C's `int a, b, c;`). Only scan
// when an active step needs the answer: a unique field can require walking
// every remaining sibling.
static bool sq_query_cursor__has_later_field(const QueryTreeCursor *cursor, TSFieldId field) {
  if (!sq_cursor_depth(cursor->cursor)) {
    return false;
  }

  for (SQNode node = sq_node_next_sibling_including_empty(query_tree_cursor_node(cursor));
       node.tree; node = sq_node_next_sibling_including_empty(node)) {
    if (sq_node_field_id(node) == field) {
      return true;
    }
  }

  return false;
}

static inline bool sq_query_cursor__should_descend(SQQueryCursor *self,
                                                   bool node_intersects_range) {

  if (node_intersects_range && self->depth < self->max_start_depth) {
    return true;
  }

  // If there are in-progress matches whose remaining steps occur
  // deeper in the tree, then descend.
  for (unsigned i = 0; i < self->states.size; i++) {
    QueryState *state = array_get(&self->states, i);
    QueryStep *next_step = array_get(&self->query->steps, state->step_index);
    if (next_step->depth != PATTERN_DONE_MARKER &&
        state->start_depth + next_step->depth > self->depth) {
      return true;
    }
  }

  // Slab cursors visit only visible nodes. Range combinations that require
  // hidden traversal barriers are rejected before execution.
  return false;
}

bool sq_query__range_intersects(const SQRange *a, const SQRange *b) {
  bool is_empty = a->start_byte == a->end_byte;
  bool bytes_intersect =
      (a->end_byte > b->start_byte || (is_empty && a->end_byte == b->start_byte)) &&
      a->start_byte < b->end_byte;
  return bytes_intersect &&
         (point_gt(a->end_point, b->start_point) ||
          (is_empty && point_eq(a->end_point, b->start_point))) &&
         point_lt(a->start_point, b->end_point);
}

bool sq_query__range_within(const SQRange *a, const SQRange *b) {
  bool bytes_within = a->start_byte >= b->start_byte && a->end_byte <= b->end_byte;
  return bytes_within && point_gte(a->start_point, b->start_point) &&
         point_lte(a->end_point, b->end_point);
}

// Avoid constructing per-node ranges on the unrestricted hot path.
static inline bool sq_query__range_is_unrestricted(const SQRange *range) {
  bool all_bytes = range->start_byte == 0 && range->end_byte == UINT32_MAX;
  return all_bytes && range->start_point.row == 0 && range->start_point.column == 0 &&
         point_eq(range->end_point, POINT_MAX);
}

// Scans can cross many groups without NFA events. Poll at bounded intervals
// there too, so enabling a timeout does not silently disable column filtering.
static bool sq_query_cursor__scan_cancelled(SQQueryCursor *self, SQNode node) {
  if (!self->query_options || !self->query_options->progress_callback) {
    return false;
  }

  if (++self->operation_count < OP_COUNT_PER_QUERY_CALLBACK_CHECK) {
    return false;
  }

  self->operation_count = 0;
  self->query_state.current_byte_offset = sq_node_start_byte(node);
  if (!self->query_options->progress_callback(&self->query_state)) {
    return false;
  }

  self->halted = true;
  return true;
}

// With no partial states, only pattern-start symbols can affect execution.
// Scan columns first, then restore the ordinary cursor path at the next hit.
// Active states always see every enter/leave event, including skipped siblings.
static bool sq_query_cursor__scan_seek(SQQueryCursor *self) {
  SQNode current = query_tree_cursor_node(&self->cursor);
  const SQQuery *query = self->query;
  uint32_t target = sq_node_position(current), end = self->scan_root_end;
  if (query->scan_filter.count) {
    target = query_execution_find_symbols(self, current.tree, &query->scan_filter, target, end);
  } else {
    while (target < end) {
      SQNode node = sq_position_node(current.tree, target);
      if (sq_query_cursor__scan_cancelled(self, node)) {
        return false;
      }

      uint32_t symbol = sq_node_symbol_id(node);
      if (query->scan_symbols.contents[symbol / 64] & (UINT64_C(1) << (symbol % 64))) {
        break;
      }

      target = sq_next_position(current.tree, target + 1);
    }
  }

  if (self->halted) {
    return false;
  }

  QUERY_EXEC_COUNT(self, records_skipped, target - sq_node_position(current));
  if (target == end) {
    self->halted = true;
    return false;
  }

  if (target - sq_node_position(current) >= 2) {
    self->scan_sparse_samples++;
  }

  if (++self->scan_samples == 32) {
    if (!self->scan_sparse_samples) {
      self->scan_cooldown = 256;
    }

    self->scan_samples = self->scan_sparse_samples = 0;
  }

  // Restore the ancestor path without processing events: this is legal only
  // with no active states. Active states always receive every enter/exit event.
  while (sq_node_position(current) != target) {
    QUERY_EXEC_COUNT(self, seek_restoration_steps, 1);
    if (target < sq_node_end_slot(current) && query_tree_cursor_goto_first_child(&self->cursor)) {
      self->depth++;
    } else {
      while (!query_tree_cursor_goto_next_sibling(&self->cursor)) {
        bool ascended = query_tree_cursor_goto_parent(&self->cursor);
        ts_assert(ascended);
        self->depth--;
      }
    }

    current = query_tree_cursor_node(&self->cursor);
  }

  return true;
}

#include "query_plan.c"

// Walk the tree, processing patterns until at least one pattern finishes,
// If one or more patterns finish, return `true` and store their states in the
// `finished_states` array. Multiple patterns can finish on the same node. If
// there are no more matches, return `false`.
static inline bool sq_query_cursor__advance(SQQueryCursor *self, bool stop_on_definite_step) {
  self->first_capture.valid = false;
  if (self->execution_active) {
    if (self->execution_needs_fallback) {
      sq_query_cursor__execution_fallback(self);
    } else {
      return sq_query_cursor__execution_advance(self, stop_on_definite_step);
    }
  }

  bool did_match = false;
  for (;;) {
    if (self->halted) {
      while (self->states.size > 0) {
        QueryState state = array_pop(&self->states);
        capture_list_pool_release(&self->capture_list_pool, state.capture_list_id);
      }
    }

    if (++self->operation_count == OP_COUNT_PER_QUERY_CALLBACK_CHECK) {
      self->operation_count = 0;
    }

    if (self->query_options && self->query_options->progress_callback) {
      self->query_state.current_byte_offset =
          sq_node_start_byte(query_tree_cursor_node(&self->cursor));
    }

    if (did_match || self->halted ||
        (self->operation_count == 0 &&
         ((self->query_options && self->query_options->progress_callback &&
           self->query_options->progress_callback(&self->query_state))))) {
      return did_match;
    }

    if (!self->ascending && self->scan_cooldown) {
      self->scan_cooldown--;
    }

    if (!self->ascending && !self->scan_cooldown && self->states.size == 0 && self->symbol_scan &&
        self->query->scan_symbols.size && self->max_start_depth == UINT32_MAX &&
        sq_query__range_is_unrestricted(&self->included_range) &&
        sq_query__range_is_unrestricted(&self->containing_range) &&
        !sq_query_cursor__scan_seek(self)) {
      return did_match;
    }

    // Exit the current node.
    if (self->ascending) {
      LOG("leave node. depth:%u, type:%s\n", self->depth,
          sq_node_type(query_tree_cursor_node(&self->cursor)));

      // After leaving a node, remove any states that cannot make further progress.
      uint32_t deleted_count = 0;
      for (unsigned i = 0, n = self->depth <= self->states_max_depth ? self->states.size : 0; i < n;
           i++) {
        QueryState *state = array_get(&self->states, i);
        QueryStep *step = array_get(&self->query->steps, state->step_index);

        // If a state completed its pattern inside of this node, but was deferred from finishing
        // in order to search for longer matches, mark it as finished.
        if (step->depth == PATTERN_DONE_MARKER &&
            (state->start_depth > self->depth || self->depth == 0)) {
          LOG("  finish pattern %u\n", state->pattern_index);
          sq_query_cursor__push_finished_state(self, state);
          did_match = true;
          self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
          deleted_count++;
        }

        // If a state needed to match something within this node, then remove that state
        // as it has failed to match.
        else if (step->depth != PATTERN_DONE_MARKER &&
                 (uint32_t)state->start_depth + (uint32_t)step->depth > self->depth) {
          LOG("  failed to match. pattern:%u, step:%u\n", state->pattern_index, state->step_index);
          capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
          self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
          deleted_count++;
        }

        else if (deleted_count > 0) {
          *array_get(&self->states, i - deleted_count) = *state;
        }
      }

      self->states.size -= deleted_count;

      // Leave this node by stepping to its next sibling or to its parent.
      // QueryTreeCursor's goto_next_sibling is a plain two-way result
      // (stepped to a real node, or didn't) — no Hidden case to handle,
      // per should_descend's own note above.
      if (query_tree_cursor_goto_next_sibling(&self->cursor)) {
        self->ascending = false;
      } else if (query_tree_cursor_goto_parent(&self->cursor)) {
        self->depth--;
      } else {
        LOG("halt at root\n");
        self->halted = true;
      }
    }

    // Enter a new node.
    else {
      SQNode node = query_tree_cursor_node(&self->cursor);

      // The common case by far: no set_byte_range/set_point_range or
      // set_containing_*_range call ever narrowed either range from its
      // exec()-time default. Every node trivially satisfies both
      // checks against an unrestricted range, so skip building either
      // node's SQRange (and the point derivation that costs) entirely.
      bool ranges_unrestricted = sq_query__range_is_unrestricted(&self->included_range) &&
                                 sq_query__range_is_unrestricted(&self->containing_range);

      bool parent_intersects_range;
      bool node_intersects_range;
      bool node_intersects_containing_range;
      bool node_within_containing_range;
      if (ranges_unrestricted) {
        parent_intersects_range = true;
        node_intersects_range = true;
        node_intersects_containing_range = true;
        node_within_containing_range = true;
      } else {
        SQNode parent_node = query_tree_cursor_parent(&self->cursor);
        parent_intersects_range =
            sq_node_is_null(parent_node) || sq_query__range_intersects(
                                                &(SQRange){
                                                    .start_point = sq_node_start_point(parent_node),
                                                    .end_point = sq_node_end_point(parent_node),
                                                    .start_byte = sq_node_start_byte(parent_node),
                                                    .end_byte = sq_node_end_byte(parent_node),
                                                },
                                                &self->included_range);
        SQRange node_range = (SQRange){
            .start_point = sq_node_start_point(node),
            .end_point = sq_node_end_point(node),
            .start_byte = sq_node_start_byte(node),
            .end_byte = sq_node_end_byte(node),
        };
        node_intersects_range = parent_intersects_range &&
                                sq_query__range_intersects(&node_range, &self->included_range);
        node_intersects_containing_range =
            sq_query__range_intersects(&node_range, &self->containing_range);
        node_within_containing_range = sq_query__range_within(&node_range, &self->containing_range);
      }

      if (node_within_containing_range) {
        TSSymbol symbol;
        bool is_named;
        QuerySiblingStatus sibling_status = {0};
        bool can_have_later_siblings_with_this_field = false;
        bool checked_later_field = false;
        TSFieldId field_id = 0;
        TSSymbol supertypes[8] = {0};
        unsigned supertype_count = 8;
        sq_query_cursor__current_status(&self->cursor, self->query, &symbol, &is_named, &field_id,
                                        supertypes, &supertype_count);
        LOG("enter node. depth:%u, type:%s, field:%s, row:%u state_count:%u, "
            "finished_state_count:%u\n",
            self->depth, sq_node_type(node),
            ts_language_field_name_for_id(self->query->language, field_id),
            sq_node_start_point(node).row,
            self->states.size, self->finished_states.size);

        bool node_is_error = symbol == ts_builtin_sym_error;

        // Cursor parents stay within this execution root.
        QueryParentStatus parent_status = {.checked = !self->root_has_error};

        // States waiting above this depth cannot consume this enter event.
        // New roots may insert earlier, so include their first insertion.
        uint32_t first_updated_state = self->depth > self->states_max_depth ? self->states.size : 0;

        uint32_t symbol_index =
            symbol == ts_builtin_sym_error ? self->query->pattern_map_slices.size - 1 : symbol;
        Slice patterns = symbol_index < self->query->pattern_map_slices.size
                             ? self->query->pattern_map_slices.contents[symbol_index]
                             : (Slice){0};

        // Mainline applies the first entry's start depth to the whole symbol group.
        uint32_t start_depth =
            patterns.length
                ? self->depth -
                      self->query->steps
                          .contents[self->query->pattern_map.contents[patterns.offset].step_index]
                          .depth
                : 0;
        uint32_t wildcard_index = 0,
                 wildcard_count = node_is_error ? 0 : self->query->wildcard_root_pattern_count;
        uint32_t pattern_index = patterns.offset, pattern_end = patterns.offset + patterns.length;

        // Both slices are in pattern order. Merging starts avoids shifting new
        // wildcard states again when an earlier concrete pattern also matches.
        while (wildcard_index < wildcard_count || pattern_index < pattern_end) {
          bool wildcard = wildcard_index < wildcard_count &&
                          (pattern_index == pattern_end ||
                           self->query->pattern_map.contents[wildcard_index].pattern_index <=
                               self->query->pattern_map.contents[pattern_index].pattern_index);
          const PatternEntry *pattern =
              &self->query->pattern_map.contents[wildcard ? wildcard_index++ : pattern_index++];
          const QueryStep *step = &self->query->steps.contents[pattern->step_index];
          uint32_t candidate_depth = wildcard ? self->depth - step->depth : start_depth;
          if ((pattern->is_rooted
                   ? node_intersects_range
                   : (parent_intersects_range &&
                      !sq_query_cursor__parent_is_error(&self->cursor, &parent_status))) &&
              (!step->field || field_id == step->field) &&
              (!wildcard || !step->supertype_symbol || supertype_count > 0) &&
              (candidate_depth <= self->max_start_depth)) {
            if (pattern->presence_requirement &&
                !sq_query_cursor__presence_matches(self, pattern, node)) {
              continue;
            }

            uint32_t inserted = sq_query_cursor__add_state(self, pattern);
            if (inserted < first_updated_state) {
              first_updated_state = inserted;
            }
          }
        }

        // Update all of the in-progress states with current node.
        QUERY_EXEC_COUNT(self, inactive_steps_skipped, first_updated_state);
        for (unsigned j = first_updated_state, copy_count = 0, pending_index = 0;
             j < self->states.size || pending_index < self->pending_states.size;
             j += 1 + copy_count) {
          if (j == self->states.size) {
            array_push(&self->states, self->pending_states.contents[pending_index++]);
          }

          QUERY_EXEC_COUNT(self, active_steps, 1);
          QueryState *state = array_get(&self->states, j);
          QueryStep *step = array_get(&self->query->steps, state->step_index);
          copy_count = 0;

          // Check that the node matches all of the criteria for the next
          // step of the pattern.
          if ((uint32_t)state->start_depth + (uint32_t)step->depth != self->depth) {
            QUERY_EXEC_COUNT(self, depth_rejections, 1);
            continue;
          }

          // A local root step can also follow a captured repetition. Such states
          // still need the ordinary path's branching for later sibling matches.
          if (self->symbol_scan && step->is_local && state->seeking_immediate_match &&
              (step->symbol ? step->symbol == symbol
                            : !node_is_error && (!step->is_named || is_named))) {
            self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
            QUERY_EXEC_COUNT(self, local_steps, 1);
            QUERY_EXEC_COUNT(self, matched_steps, 1);
            if (step->capture_ids[0] != NONE) {
              sq_query_cursor__capture(self, state, step, node);
            }

            const QueryPattern *pattern = &self->query->patterns.contents[state->pattern_index];
            state->step_index = pattern->steps.offset + pattern->steps.length - 1;
            state->seeking_immediate_match = false;
            state->skipped_quantifier = false;
            if (stop_on_definite_step &&
                self->query->steps.contents[state->step_index].root_pattern_guaranteed) {
              did_match = true;
            }

            continue;
          }

          // Determine if this node matches this step of the pattern, and also
          // if this node can have later siblings that match this step of the
          // pattern.
          bool node_does_match = false;
          if (step->symbol == WILDCARD_SYMBOL) {
            if (step->is_missing) {
              node_does_match = sq_node_is_missing(node);
            } else {
              node_does_match = !node_is_error && (is_named || !step->is_named);
            }
          } else {
            node_does_match =
                symbol == step->symbol && (!step->is_missing || sq_node_is_missing(node));
          }

          QUERY_EXEC_COUNT(self, symbol_rejections, !node_does_match);
          bool later_sibling_can_match =
              !((step->is_immediate && is_named && !state->skipped_quantifier) ||
                state->seeking_immediate_match) &&
              sq_query_cursor__has_later_sibling(&self->cursor, &sibling_status, false);
          if (step->is_last_child &&
              sq_query_cursor__has_later_sibling(&self->cursor, &sibling_status, true)) {
            node_does_match = false;
          }

          if (step->supertype_symbol) {
            if (!sq_node_has_supertype(node, step->supertype_symbol)) {
              node_does_match = false;
            }
          }

          if (step->field) {
            if (step->field == field_id && later_sibling_can_match) {
              if (!checked_later_field) {
                can_have_later_siblings_with_this_field =
                    sq_query_cursor__has_later_field(&self->cursor, field_id);
                checked_later_field = true;
              }

              if (!can_have_later_siblings_with_this_field) {
                later_sibling_can_match = false;
              }
            } else if (step->field != field_id) {
              node_does_match = false;
            }
          }

          if (step->negated_field_list_id) {
            TSFieldId *negated_field_ids =
                array_get(&self->query->negated_fields, step->negated_field_list_id);
            for (;;) {
              TSFieldId negated_field_id = *negated_field_ids;
              if (negated_field_id) {
                negated_field_ids++;
                if (!sq_node_is_null(sq_node_child_by_field_id(node, negated_field_id))) {
                  node_does_match = false;
                  break;
                }
              } else {
                break;
              }
            }
          }

          // Remove states immediately if it is ever clear that they cannot match.
          if (!node_does_match) {
            if (!later_sibling_can_match) {
              LOG("  discard state. pattern:%u, step:%u\n", state->pattern_index,
                  state->step_index);
              capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
              self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
              sq_query_cursor__stage_remaining_states(self, j);
              array_erase(&self->states, j);
              j--;
            }

            continue;
          }

          QUERY_EXEC_COUNT(self, matched_steps, 1);
          self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
          sq_query_cursor__stage_remaining_states(self, j);

          // Some patterns can match their root node in multiple ways, capturing different
          // children. If this pattern step could match later children within the same
          // parent, then this query state cannot simply be updated in place. It must be
          // split into two states: one that matches this node, and one which skips over
          // this node, to preserve the possibility of matching later siblings.
          if (later_sibling_can_match &&
              (step->contains_captures ||
               sq_query__step_is_fallible(self->query, state->step_index))) {
            if (sq_query_cursor__copy_state(self, &state)) {
              LOG("  split state for capture. pattern:%u, step:%u\n", state->pattern_index,
                  state->step_index);
              copy_count++;
            }
          }

          // If this pattern started with a wildcard, such that the pattern map
          // actually points to the *second* step of the pattern, then check
          // that the node has a parent, and capture the parent node if necessary.
          if (state->needs_parent) {
            SQNode parent = query_tree_cursor_parent(&self->cursor);
            if (sq_node_is_null(parent)) {
              LOG("  missing parent node\n");
              state->dead = true;
            } else {
              state->needs_parent = false;
              QueryStep *skipped_wildcard_step = step;
              do {
                skipped_wildcard_step--;
              } while (skipped_wildcard_step->is_dead_end ||
                       skipped_wildcard_step->is_pass_through || skipped_wildcard_step->depth > 0);
              if (skipped_wildcard_step->capture_ids[0] != NONE) {
                LOG("  capture wildcard parent\n");
                sq_query_cursor__capture(self, state, skipped_wildcard_step, parent);
              }
            }
          }

          // If the current node is captured in this pattern, add it to the capture list.
          if (step->capture_ids[0] != NONE) {
            sq_query_cursor__capture(self, state, step, node);
          }

          if (state->dead) {
            self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
            array_erase(&self->states, j);
            j--;
            continue;
          }

          // Advance this state to the next step of its pattern.
          state->step_index++;
          LOG("  advance state. pattern:%u, step:%u\n", state->pattern_index, state->step_index);

          QueryStep *next_step = array_get(&self->query->steps, state->step_index);

          // For a given step, if the current symbol is the wildcard symbol, `_`, and it is **not**
          // named, meaning it should capture anonymous nodes, **and** the next step is immediate,
          // we reuse the `seeking_immediate_match` flag to indicate that we are looking for an
          // immediate match due to an unnamed wildcard symbol.
          //
          // The reason for this is that typically, anchors will not consider anonymous nodes,
          // but we're special casing the wildcard symbol to allow for any immediate matches,
          // regardless of whether they are named or not.
          if (step->symbol == WILDCARD_SYMBOL && !step->is_named && next_step->is_immediate) {
            state->seeking_immediate_match = true;
          } else {
            state->seeking_immediate_match = false;
          }

          // The zero-skip's vacuous-anchor exemption only covers the immediate
          // step it lands on. Once the state advances, a later anchor is normal.
          state->skipped_quantifier = false;

          if (stop_on_definite_step && next_step->root_pattern_guaranteed) {
            did_match = true;
          }

          // If this state's next step has an alternative step, then copy the state in order
          // to pursue both alternatives. The alternative step itself may have an alternative,
          // so this is an interactive process.
          unsigned end_index = j + 1;
          for (unsigned k = j; k < end_index; k++) {
            QueryState *child_state = array_get(&self->states, k);
            QueryStep *child_step = array_get(&self->query->steps, child_state->step_index);
            if (child_step->alternative_index != NONE) {
              // A "dead-end" step exists only to add a non-sequential jump into the step sequence,
              // via its alternative index. When a state reaches a dead-end step, it jumps straight
              // to the step's alternative.
              if (child_step->is_dead_end) {
                child_state->step_index = child_step->alternative_index;
                k--;
                continue;
              }

              // A "pass-through" step exists only to add a branch into the step sequence,
              // via its alternative_index. When a state reaches a pass-through step, it splits
              // in order to process the alternative step, and then it advances to the next step.
              if (child_step->is_pass_through) {
                child_state->step_index++;
                k--;
              }

              // A `?`/`*` zero-skip past a step that carries a trailing last-child
              // anchor transfers that requirement to the last matched node. The
              // skip is only valid if that node really is the last named child.
              if (child_step->alternative_is_skip && child_step->is_last_child &&
                  sq_query_cursor__has_later_sibling(&self->cursor, &sibling_status, true)) {
                continue;
              }

              QueryState *copy = sq_query_cursor__copy_state(self, &child_state);
              if (copy) {
                LOG("  split state for branch. pattern:%u, from_step:%u, to_step:%u, "
                    "pass_through:%d, capture_count:%u\n",
                    copy->pattern_index, copy->step_index, next_step->alternative_index,
                    next_step->is_pass_through,
                    capture_list_pool_get(&self->capture_list_pool, copy->capture_list_id)->size);
                end_index++;
                copy_count++;
                copy->step_index = child_step->alternative_index;
                if (child_step->is_pass_through) {
                  copy->seeking_immediate_match = true;
                }

                // Taking a `?`/`*` zero-skip means the quantified subpattern matched
                // nothing. How an adjacent anchor behaves then depends on where it sat:
                if (child_step->alternative_is_skip) {
                  if (!child_step->is_immediate) {
                    QueryStep *skip_target =
                        array_get(&self->query->steps, child_step->alternative_index);

                    // No leading anchor on the skipped step, so an immediately-following
                    // anchor on the skip target is vacuous (`Q* . B` with zero `Q` lets
                    // `B` match anywhere).
                    copy->skipped_quantifier = skip_target->depth == child_step->depth;
                  } else if (array_get(&self->query->steps, child_state->step_index - 1)->depth <
                             child_step->depth) {
                    // The skipped step was the parent's first child pattern and carried a
                    // leading *boundary* anchor (`(P . Q* Y)`). Transfer the first-child
                    // requirement to the skip target so it survives the empty run: `Y`
                    // must still be the parent's first named child.
                    copy->seeking_immediate_match = true;
                  }

                  // Otherwise the skipped step carried a leading *between* anchor
                  // (`A . Q* ...`): with zero `Q` that adjacency vanishes, while the skip
                  // target's own anchor, if any, still applies (`A . Q* . B` stays adjacent).
                }
              }
            }
          }
        }

        array_clear(&self->pending_states);

        // Containment is independent between patterns. Preserve unchanged
        // patterns' alternative flags; removals can release a deferred match.
        if (self->dirty_patterns) {
          uint64_t dirty_patterns = self->dirty_patterns;
          self->dirty_patterns = 0;
          QUERY_EXEC_COUNT(self, dedup_passes, 1);
          for (uint32_t index = 0; index < self->states.size; index++) {
            QueryState *state = &self->states.contents[index];
            if (dirty_patterns & (UINT64_C(1) << (state->pattern_index % 64))) {
              state->has_in_progress_alternatives = false;
            }
          }

          // Later captures, adjacent copies, and removals preserve this order.
          // New states, first captures, evictions, and restored depths invalidate it.
          if (self->states_need_sort) {
            sq_query_cursor__sort_states_by_capture(self);
            self->states_need_sort = false;
          }

          sq_query_cursor__index_capture_lists(self);

          uint32_t group_depth = UINT32_MAX, group_pattern = UINT32_MAX;
          bool group_has_unique_start = false;
          for (unsigned j = 0; j < self->states.size; j++) {
            QueryState *state = array_get(&self->states, j);
            if (state->removed ||
                !(dirty_patterns & (UINT64_C(1) << (state->pattern_index % 64)))) {
              continue;
            }

            if (state->dead) {
              self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
              state->removed = true;
              continue;
            }

            if (state->start_depth != group_depth || state->pattern_index != group_pattern) {
              group_depth = state->start_depth;
              group_pattern = state->pattern_index;
              group_has_unique_start = sq_query_cursor__group_has_unique_start(self, j);
            }

            // Enforce the longest-match criteria. When a query pattern contains optional or
            // repeated nodes, this is necessary to avoid multiple redundant states, where
            // one state has a strict subset of another state's captures.
            bool did_remove = false;
            uint32_t next_in_bucket = self->capture_comparison_index.size
                                          ? self->capture_comparison_index.contents[j].next
                                          : self->states.size;
            const CaptureList *state_captures =
                capture_list_pool_get(&self->capture_list_pool, state->capture_list_id);
            uint32_t comparison_block = UINT32_MAX;
            uint64_t comparison_candidates = 0;
            for (unsigned k = j + 1; k < self->states.size; k++) {
              QueryState *other_state = array_get(&self->states, k);
              if (other_state->removed) {
                continue;
              }

              // Query states are kept in ascending order of start_depth and pattern_index, and
              // (via the above call to `sq_query_cursor__sort_states_by_capture`) in ascending
              // order of first-capture position within each such group.
              //
              // Since the longest-match criteria is only used for deduping matches of the same
              // pattern and root node, we only need to perform pairwise comparisons within a
              // small slice of the states array.
              if (other_state->start_depth != state->start_depth ||
                  other_state->pattern_index != state->pattern_index) {
                break;
              }

              // States in a group acquire their first capture in tree-traversal order, so the
              // group is ordered by first-capture position. Once `other_state`'s captures begin
              // at or after where `state`'s captures end, `other_state` (and every state after
              // it in the group) is disjoint from `state`: neither can be a capture-subset of
              // the other, so there is nothing to drop and no longest-match alternative to
              // record. Stop scanning `state` against the rest of the group.
              const CaptureComparisonEntry *other_entry =
                  self->capture_comparison_index.size ? &self->capture_comparison_index.contents[k]
                                                      : NULL;
              const CaptureList *other_captures =
                  other_entry ? NULL
                              : capture_list_pool_get(&self->capture_list_pool,
                                                      other_state->capture_list_id);
              uint32_t other_count =
                  other_entry ? other_entry->capture_count : other_captures->size;
              uint32_t other_start =
                  other_entry ? other_entry->first_start_byte : other_captures->first_start_byte;
              if (state_captures->size > 0 && other_count > 0 &&
                  ((group_has_unique_start && other_start > state_captures->first_start_byte) ||
                   other_start >= capture_list_pool_last_end_byte(&self->capture_list_pool,
                                                                  state->capture_list_id))) {
                break;
              }

              if (self->capture_comparison_blocks.size && state_captures->prefix_id &&
                  (self->query->has_repeated_captures || state_captures->size != other_count)) {
                if (comparison_block != k / 64) {
                  comparison_block = k / 64;
                  comparison_candidates = capture_comparison_block_candidates(
                      &self->capture_comparison_blocks.contents[comparison_block],
                      state_captures->capture_set);
                }

                uint64_t remaining = comparison_candidates & (UINT64_MAX << (k % 64));
                uint32_t next = comparison_block * 64 + (remaining ? query_ctz(remaining) : 64);
                if (next > self->states.size) {
                  next = self->states.size;
                }

                if (next > k) {
                  QUERY_EXEC_COUNT(self, capture_set_slots_skipped, next - k);
                  k = next - 1;
                  continue;
                }
              }

              if (!other_captures) {
                other_captures =
                    capture_list_pool_get(&self->capture_list_pool, other_state->capture_list_id);
              }

              if (self->capture_comparison_index.size && state_captures->prefix_id &&
                  other_captures->prefix_id && state_captures->size == other_captures->size &&
                  state_captures->capture_hash != other_captures->capture_hash) {
                while (next_in_bucket <= k) {
                  next_in_bucket = self->capture_comparison_index.contents[next_in_bucket].next;
                }

                uint32_t next = self->capture_comparison_index.contents[k].end;
                if (next_in_bucket < next) {
                  next = next_in_bucket;
                }

                QUERY_EXEC_COUNT(self, indexed_slots_skipped, next - k);
                k = next - 1;
                continue;
              }

              CaptureContainment containment =
                  sq_query_cursor__compare_captures(self, state_captures, other_captures);
              if (containment.left_contains_right) {
                if (state->step_index == other_state->step_index &&
                    (other_state->seeking_immediate_match || !state->seeking_immediate_match)) {
                  LOG("  drop shorter state. pattern: %u, step_index: %u\n", state->pattern_index,
                      state->step_index);
                  capture_list_pool_release(&self->capture_list_pool, other_state->capture_list_id);
                  self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
                  other_state->removed = true;
                  continue;
                }

                other_state->has_in_progress_alternatives = true;
              }

              if (containment.right_contains_left) {
                if (state->step_index == other_state->step_index &&
                    (state->seeking_immediate_match || !other_state->seeking_immediate_match)) {
                  LOG("  drop shorter state. pattern: %u, step_index: %u\n", state->pattern_index,
                      state->step_index);
                  capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
                  self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
                  state->removed = true;
                  did_remove = true;
                  break;
                }

                state->has_in_progress_alternatives = true;
              }
            }

            // If the state is at the end of its pattern, remove it from the list
            // of in-progress states and add it to the list of finished states.
            if (!did_remove) {
              LOG("  keep state. pattern: %u, start_depth: %u, step_index: %u, capture_count: %u\n",
                  state->pattern_index, state->start_depth, state->step_index,
                  capture_list_pool_get(&self->capture_list_pool, state->capture_list_id)->size);
              QueryStep *next_step = array_get(&self->query->steps, state->step_index);
              if (next_step->depth == PATTERN_DONE_MARKER) {
                if (state->has_in_progress_alternatives) {
                  LOG("  defer finishing pattern %u\n", state->pattern_index);
                } else {
                  LOG("  finish pattern %u\n", state->pattern_index);
                  sq_query_cursor__push_finished_state(self, state);
                  self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
                  state->removed = true;
                  did_match = true;
                }
              }
            }
          }

          // Defer shifts until the pass ends; only surviving states participate
          // in comparisons, and their relative order is unchanged.
          uint32_t retained = 0;
          self->states_max_depth = 0;
          for (uint32_t index = 0; index < self->states.size; index++) {
            if (!self->states.contents[index].removed) {
              const QueryState *state = &self->states.contents[index];
              uint32_t step_depth = self->query->steps.contents[state->step_index].depth;
              uint32_t depth =
                  state->start_depth + (step_depth == PATTERN_DONE_MARKER ? 0 : step_depth);
              if (depth > self->states_max_depth) {
                self->states_max_depth = depth;
              }

              if (retained != index) {
                self->states.contents[retained] = self->states.contents[index];
              }

              retained++;
            }
          }

          self->states.size = retained;
        } else {
          QUERY_EXEC_COUNT(self, dedup_skips, 1);
        }
      }

      if (node_intersects_containing_range &&
          sq_query_cursor__should_descend(self, node_intersects_range)) {
        if (query_tree_cursor_goto_first_child(&self->cursor)) {
          self->depth++;
          continue;
        }
      }

      self->ascending = true;
    }
  }
}

static bool query_execution_supported(SQQueryCursor *cursor) {
  if (cursor->error != SQ_QUERY_OK) {
    return false;
  }

  if (!sq_query__range_is_unrestricted(&cursor->included_range) ||
      !sq_query__range_is_unrestricted(&cursor->containing_range)) {
    for (uint32_t index = 0; index < cursor->query->steps.size; index++) {
      if (cursor->query->steps.contents[index].alternative_index != NONE) {
        cursor->error = SQ_QUERY_UNSUPPORTED_RANGE;
        cursor->halted = true;
        return false;
      }
    }

    for (uint32_t index = 0; index < cursor->query->pattern_map.size; index++) {
      if (!cursor->query->pattern_map.contents[index].is_rooted) {
        cursor->error = SQ_QUERY_UNSUPPORTED_RANGE;
        cursor->halted = true;
        return false;
      }
    }
  }

  return true;
}

SQQueryExecutionError sq_query_cursor_error(const SQQueryCursor *cursor) {
  return cursor->error;
}

bool sq_query_cursor_next_match(SQQueryCursor *self, SQQueryMatch *match) {
  if (!query_execution_supported(self)) {
    return false;
  }

  if (self->finished_states.size == 0) {
    if (!sq_query_cursor__advance(self, false)) {
      return false;
    }
  }

  if (self->finished_states_heap_size > 0) {
    sq_query_cursor__heapify_finished_states(self);
  }

  uint32_t state_index = 0;
  if (self->finished_states_heap_size > 0) {
    for (uint32_t i = 1; i < self->finished_states.size; i++) {
      QueryState *state = array_get(&self->finished_states, i);
      QueryState *earliest_state = array_get(&self->finished_states, state_index);
      if (state->heap_insert_order < earliest_state->heap_insert_order) {
        state_index = i;
      }
    }
  }

  QueryState *state = array_get(&self->finished_states, state_index);
  if (state->id == UINT32_MAX) {
    state->id = self->next_state_id++;
  }

  match->id = state->id;
  match->pattern_index = state->pattern_index;
  const CaptureList *captures =
      capture_list_pool_get(&self->capture_list_pool, state->capture_list_id);
  match->captures = captures->contents;
  match->capture_count = captures->size;
  QUERY_EXEC_COUNT(self, snapshot_captures, captures->size);
  capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
  if (self->finished_states_heap_size > 0) {
    finished_state_erase(&self->finished_states, state_index);
    self->finished_states_heap_size = self->finished_states.size;
  } else {
    array_erase(&self->finished_states, state_index);
  }

  return true;
}

void sq_query_cursor_remove_match(SQQueryCursor *self, uint32_t match_id) {
  if (self->finished_states_heap_size > 0) {
    sq_query_cursor__heapify_finished_states(self);
  }

  for (unsigned i = 0; i < self->finished_states.size; i++) {
    const QueryState *state = array_get(&self->finished_states, i);
    if (state->id == match_id) {
      capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
      if (self->finished_states_heap_size > 0) {
        finished_state_erase(&self->finished_states, i);
        self->finished_states_heap_size = self->finished_states.size;
      } else {
        array_erase(&self->finished_states, i);
      }

      return;
    }
  }

  // Remove unfinished query states as well to prevent future
  // captures for a match being removed.
  for (unsigned i = 0; i < self->states.size; i++) {
    const QueryState *state = array_get(&self->states, i);
    if (state->id == match_id) {
      capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
      if (self->execution_active) {
        query_execution_release_state(self, state);
      }

      self->first_capture.valid = false;
      self->dirty_patterns |= UINT64_C(1) << (state->pattern_index % 64);
      array_erase(&self->states, i);
      return;
    }
  }
}

bool sq_query_cursor_next_capture(SQQueryCursor *self, SQQueryMatch *match,
                                  uint32_t *capture_index) {
  if (!query_execution_supported(self)) {
    return false;
  }

  // The goal here is to return captures in order, even though they may not
  // be discovered in order, because patterns can overlap. Search for matches
  // until there is a finished capture that is before any unfinished capture.
  for (;;) {
    // Sift any newly pushed finished states into the heap.
    sq_query_cursor__heapify_finished_states(self);

    // First, find the earliest capture in an unfinished match.
    // Returning finished captures leaves in-progress states unchanged.
    if (!self->first_capture.valid) {
      self->first_capture.definite = false;
      self->first_capture.found = sq_query_cursor__first_in_progress_capture(
          self, &self->first_capture.state_index, &self->first_capture.byte_offset,
          &self->first_capture.pattern_index, &self->first_capture.definite);
      self->first_capture.valid = true;
    }

    uint32_t first_unfinished_capture_byte = self->first_capture.byte_offset;
    uint32_t first_unfinished_pattern_index = self->first_capture.pattern_index;
    uint32_t first_unfinished_state_index = self->first_capture.state_index;
    bool first_unfinished_state_is_definite = self->first_capture.definite;
    bool found_unfinished_state = self->first_capture.found;

    // Then find the earliest capture in a finished match. The finished_states
    // array is maintained as a min-heap, so the earliest is always at index 0.
    // Clean up fully-consumed and out-of-range states from the heap root first.
    QueryState *first_finished_state = NULL;
    uint32_t first_finished_capture_byte = first_unfinished_capture_byte;
    uint32_t first_finished_pattern_index = first_unfinished_pattern_index;
    while (self->finished_states.size > 0) {
      QueryState *state = array_get(&self->finished_states, 0);
      const CaptureList *captures =
          capture_list_pool_get(&self->capture_list_pool, state->capture_list_id);

      // Remove states whose captures are all consumed.
      if (state->consumed_capture_count >= captures->size) {
        capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
        finished_state_pop(&self->finished_states);
        self->finished_states_heap_size = self->finished_states.size;
        continue;
      }

      SQNode node = array_get(captures, state->consumed_capture_count)->node;

      bool inside_unrestricted_range =
          sq_query__capture_is_inside_unrestricted_range(node, &self->included_range);
      bool node_precedes_range =
          !inside_unrestricted_range && sq_query__node_precedes_range(node, &self->included_range);
      bool node_follows_range =
          !inside_unrestricted_range && sq_query__node_follows_range(node, &self->included_range);
      bool node_outside_of_range = node_precedes_range || node_follows_range;

      // Skip captures that are outside of the cursor's range.
      if (node_outside_of_range) {
        state->consumed_capture_count++;
        finished_state_update_key(state, &self->capture_list_pool);
        finished_state_sift_down(&self->finished_states, 0);
        continue;
      }

      uint32_t node_start_byte = sq_query__node_start_byte(node);
      if (node_start_byte < first_finished_capture_byte ||
          (node_start_byte == first_finished_capture_byte &&
           state->pattern_index < first_finished_pattern_index)) {
        first_finished_state = state;
        first_finished_capture_byte = node_start_byte;
        first_finished_pattern_index = state->pattern_index;
      }

      break;
    }

    // If there is finished capture that is clearly before any unfinished
    // capture, then return its match, and its capture index. Internally
    // record the fact that the capture has been 'consumed'.
    QueryState *state;
    if (first_finished_state) {
      state = first_finished_state;
    } else if (first_unfinished_state_is_definite) {
      state = array_get(&self->states, first_unfinished_state_index);
    } else {
      state = NULL;
    }

    if (state) {
      if (state->id == UINT32_MAX) {
        state->id = self->next_state_id++;
      }

      match->id = state->id;
      match->pattern_index = state->pattern_index;
      const CaptureList *captures =
          capture_list_pool_get(&self->capture_list_pool, state->capture_list_id);
      match->captures = captures->contents;
      match->capture_count = captures->size;
      QUERY_EXEC_COUNT(self, snapshot_captures, captures->size);
      *capture_index = state->consumed_capture_count;
      state->consumed_capture_count++;

      // If this state is in the finished_states heap, its sort key has changed
      // (next capture is now later in the document). Restore heap order.
      if (state == first_finished_state) {
        finished_state_update_key(state, &self->capture_list_pool);
        finished_state_sift_down(&self->finished_states, 0);
      } else {
        self->first_capture.valid = false;
      }

      return true;
    }

    if (capture_list_pool_is_empty(&self->capture_list_pool) && found_unfinished_state) {
      LOG("  abandon state. index:%u, pattern:%u, offset:%u.\n", first_unfinished_state_index,
          first_unfinished_pattern_index, first_unfinished_capture_byte);
      QueryState *state = array_get(&self->states, first_unfinished_state_index);
      capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
      if (self->execution_active) {
        query_execution_release_state(self, state);
      }

      self->dirty_patterns |= UINT64_C(1) << (first_unfinished_pattern_index % 64);
      array_erase(&self->states, first_unfinished_state_index);
    }

    // If there are no finished matches that are ready to be returned, then
    // continue finding more matches.
    if (!sq_query_cursor__advance(self, true) && self->finished_states.size == 0) {
      return false;
    }
  }
}

void sq_query_cursor_set_max_start_depth(SQQueryCursor *self, uint32_t max_start_depth) {
  self->execution_needs_fallback = self->execution_active;
  self->max_start_depth = max_start_depth;
}

#undef LOG

void sq_query_cursor_set_optimized(SQQueryCursor *cursor, bool enabled) {
  cursor->execution_needs_fallback = cursor->execution_active;
  cursor->symbol_scan = enabled;
}
