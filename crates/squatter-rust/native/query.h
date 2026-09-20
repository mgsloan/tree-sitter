#ifndef SQUAT_NATIVE_QUERY_H_
#define SQUAT_NATIVE_QUERY_H_

#include "internal.h"
#include "array.h"

typedef struct SQQuery SQQuery;

// element range within an owning array; string slices use byte offsets
typedef struct {
  uint32_t offset, length;
} Slice;

// match instruction shared with Rust
// Depth UINT16_MAX ends a pattern; capture IDs and alternative_index use it for absence.
typedef struct {
  uint16_t symbol, supertype_symbol, field, capture_ids[3];
  uint16_t depth, alternative_index, negated_field_list_id, flags;
} QueryStep;

// pattern entry point, sorted by the starting step's symbol and then pattern ID
typedef struct {
  uint16_t step_index, pattern_index, presence_requirement, flags;
} PatternEntry;

// compiled pattern ranges and the corresponding span in the query source
typedef struct {
  Slice steps, predicate_steps;

  uint32_t start_byte, end_byte;
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

SQQuery *sq_native_query_new(const TSLanguage *, const char *, uint32_t, uint32_t *,
                             TSQueryError *);
void sq_native_query_delete(SQQuery *);

#endif
