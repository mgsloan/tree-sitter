#ifndef SQUAT_NATIVE_QUERY_H_
#define SQUAT_NATIVE_QUERY_H_

#include "internal.h"
#include "array.h"

typedef struct SQQuery SQQuery;

/*
 * QueryStep - A step in the process of matching a query. Each node within
 * a query S-expression corresponds to one of these steps. An entire pattern
 * is represented as a sequence of these steps. The basic properties of a
 * node are represented by these fields:
 * - `symbol` - The grammar symbol to match. A zero value represents the
 *    wildcard symbol, '_'.
 * - `field` - The field name to match. A zero value means that a field name
 *    was not specified.
 * - `capture_ids` - An array of integers representing the names of captures
 *    associated with this node in the pattern, terminated by a `NONE` value.
 * - `depth` - The depth where this node occurs in the pattern. The root node
 *    of the pattern has depth zero.
 * - `negated_field_list_id` - An id representing a set of fields that must
 *    not be present on a node matching this step.
 *
 * Steps have some additional fields in order to handle the `.` (or "anchor") operator,
 * which forbids additional child nodes:
 * - `is_immediate` - Indicates that the node matching this step cannot be preceded
 *    by other sibling nodes that weren't specified in the pattern.
 * - `is_last_child` - Indicates that the node matching this step cannot have any
 *    subsequent named siblings.
 *
 * For simple patterns, steps are matched in sequential order. But in order to
 * handle alternative/repeated/optional sub-patterns, query steps are not always
 * structured as a linear sequence; they sometimes need to split and merge. This
 * is done using the following fields:
 * - `alternative_index` - The index of a different query step that serves as
 *    an alternative to this step. A `NONE` value represents no alternative.
 *    When a query state reaches a step with an alternative index, the state
 *    is duplicated, with one copy remaining at the original step, and one copy
 *    moving to the alternative step. The alternative may have its own alternative
 *    step, so this splitting is an iterative process.
 * - `is_dead_end` - Indicates that this state cannot be passed directly, and
 *    exists only in order to redirect to an alternative index, with no splitting.
 * - `is_pass_through` - Indicates that state has no matching logic of its own,
 *    and exists only to split a state. One copy of the state advances immediately
 *    to the next step, and one moves to the alternative step.
 * - `alternative_is_skip` - Indicates that this step's `alternative_index` is the
 *    forward skip introduced by a `?` or `*` quantifier (the branch taken when the
 *    quantifier matches zero occurrences). For a state that follows it, an
 *    immediately-following anchor is vacuous.
 * - `is_inside_alternation` - Indicates that state is inside an alternation.
 *    Currently only written to quantifier steps, read by logic that maintains
 *    correctness for quantifiers inside alternations.
 *
 * Steps also store some derived state that summarizes how they relate to other
 * steps within the same pattern. This is used to optimize the matching process:
 * - `contains_captures` - Indicates that this step or one of its child steps
 *    has a non-empty `capture_ids` list.
 * - `parent_pattern_guaranteed` - Indicates that if this step is reached, then
 *    it and all of its subsequent sibling steps within the same parent pattern
 *    are guaranteed to match.
 * - `root_pattern_guaranteed` - Similar to `parent_pattern_guaranteed`, but
 *    for the entire top-level pattern. When iterating through a query's
 *    captures using `ts_query_cursor_next_capture`, this field is used to
 *    detect that a capture can safely be returned from a match that has not
 *    even completed yet.
 */
typedef struct {
  TSSymbol symbol;
  TSSymbol supertype_symbol;
  TSFieldId field;
  uint16_t capture_ids[3];
  uint16_t depth;
  uint16_t alternative_index;
  uint16_t negated_field_list_id;
  uint16_t flags;
} QueryStep;

/*
 * Slice - A slice of an external array. Within a query, capture names,
 * literal string values, and predicate step information are stored in three
 * contiguous arrays. Individual captures, string values, and predicates are
 * represented as slices of these three arrays.
 */
typedef struct {
  uint32_t offset;
  uint32_t length;
} Slice;

/*
 * PatternEntry - Information about the starting point for matching a particular
 * pattern. These entries are stored in a 'pattern map' - a sorted array that
 * makes it possible to efficiently lookup patterns based on the symbol for their
 * first step. The entry consists of the following fields:
 * - `pattern_index` - the index of the pattern within the query
 * - `step_index` - the index of the pattern's first step in the shared `steps` array
 * - `is_rooted` - whether or not the pattern has a single root node. This property
 *   affects decisions about whether or not to start the pattern for nodes outside
 *   of a QueryCursor's range restriction.
 */
typedef struct {
  uint16_t step_index;
  uint16_t pattern_index;
  uint16_t presence_requirement;
  uint16_t flags;
} PatternEntry;

typedef struct {
  Slice steps;
  Slice predicate_steps;
  uint32_t start_byte;
  uint32_t end_byte;
  uint16_t flags;
} QueryPattern;

// predicate token: type selects a capture ID, string ID, or end marker
typedef struct {
  uint32_t type, value_id;
} QueryPredicateStep;

// untyped borrowed array; length counts elements of the field's agreed FFI type
typedef struct {
  const void *data;
  uint32_t length;
} NativeView;

// string bytes plus an array of Slice entries into those bytes
typedef struct {
  NativeView bytes, entries;
} NativeStringTable;

// borrowed program view
// Rust retains SQQuery for the lifetime of these arrays. Mutations require exclusive
// access; refresh the view after changes to array lengths.
typedef struct {
  const TSLanguage *language;
  uint32_t symbol_count;

  NativeView public_symbols, steps, pattern_entries, patterns, predicate_steps;
  NativeStringTable capture_names, predicate_values;
  NativeView capture_quantifiers, negated_fields, rootless_repeat_symbols;
  uint32_t wildcard_root_pattern_count;
} SQQueryView;

// Explicit flag words avoid compiler-dependent C bitfield layout at the Rust boundary.
// build.rs also reads the SQ_STEP definitions to generate Rust constants.
#define SQ_STEP_IS_NAMED (UINT16_C(1) << 0)
#define SQ_STEP_IS_IMMEDIATE (UINT16_C(1) << 1)
#define SQ_STEP_IS_LAST_CHILD (UINT16_C(1) << 2)
#define SQ_STEP_IS_PASS_THROUGH (UINT16_C(1) << 3)
#define SQ_STEP_IS_DEAD_END (UINT16_C(1) << 4)
#define SQ_STEP_IS_INSIDE_ALTERNATION (UINT16_C(1) << 5)
#define SQ_STEP_CONTAINS_CAPTURES (UINT16_C(1) << 6)
#define SQ_STEP_ROOT_PATTERN_GUARANTEED (UINT16_C(1) << 7)
#define SQ_STEP_PARENT_PATTERN_GUARANTEED (UINT16_C(1) << 8)
#define SQ_STEP_IS_MISSING (UINT16_C(1) << 9)
#define SQ_STEP_ALTERNATIVE_IS_SKIP (UINT16_C(1) << 10)
#define SQ_STEP_IS_LOCAL (UINT16_C(1) << 11)

#define SQ_PATTERN_IS_ROOTED UINT16_C(1)
#define SQ_PATTERN_IS_NON_LOCAL UINT16_C(1)

_Static_assert(sizeof(QueryStep) == 20, "query step layout");
_Static_assert(offsetof(QueryStep, flags) == 18, "query step flags");
_Static_assert(sizeof(PatternEntry) == 8, "pattern entry layout");

SQQuery *sq_native_query_new(
  const TSLanguage *,
  const char *,
  uint32_t,
  uint32_t *,
  TSQueryError *
);
void sq_native_query_delete(SQQuery *);

#endif
