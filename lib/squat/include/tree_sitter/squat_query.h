#ifndef TREE_SITTER_SQUAT_QUERY_H_
#define TREE_SITTER_SQUAT_QUERY_H_
#include "squat.h"
#ifdef __cplusplus
extern "C" {
#endif

typedef struct SQQuery SQQuery;
typedef struct SQQueryCursor SQQueryCursor;
typedef enum {
  SQ_QUERY_OK,
  SQ_QUERY_UNSUPPORTED_RANGE,
  SQ_QUERY_INVALID_EXECUTION,
} SQQueryExecutionError;

typedef struct {
  SQNode node;
  uint32_t index;
} SQQueryCapture;

typedef struct {
  uint32_t id;
  uint16_t pattern_index, capture_count;
  const SQQueryCapture *captures;
} SQQueryMatch;

// Mainline matching and predicate metadata, with the range restriction below. C does not evaluate host text predicates. Captures borrow
// the cursor until its next advancement; the query and tree must remain alive.
SQQuery *sq_query_new(const TSLanguage *language, const char *source, uint32_t source_len,
                      uint32_t *error_offset, TSQueryError *error_type);
void sq_query_delete(SQQuery *self);
SQQuery *sq_query_copy(const SQQuery *self);
uint32_t sq_query_pattern_count(const SQQuery *self);
uint32_t sq_query_capture_count(const SQQuery *self);
uint32_t sq_query_string_count(const SQQuery *self);
uint32_t sq_query_start_byte_for_pattern(const SQQuery *self, uint32_t pattern_index);
uint32_t sq_query_end_byte_for_pattern(const SQQuery *self, uint32_t pattern_index);
const TSQueryPredicateStep *
sq_query_predicates_for_pattern(const SQQuery *self, uint32_t pattern_index, uint32_t *step_count);
bool sq_query_is_pattern_rooted(const SQQuery *self, uint32_t pattern_index);
bool sq_query_is_pattern_non_local(const SQQuery *self, uint32_t pattern_index);
bool sq_query_is_pattern_guaranteed_at_step(const SQQuery *self, uint32_t byte_offset);
const char *sq_query_capture_name_for_id(const SQQuery *self, uint32_t index, uint32_t *length);
TSQuantifier sq_query_capture_quantifier_for_id(const SQQuery *self, uint32_t pattern_index,
                                                uint32_t capture_index);
const char *sq_query_string_value_for_id(const SQQuery *self, uint32_t index, uint32_t *length);
void sq_query_disable_capture(SQQuery *self, const char *name, uint32_t length);
void sq_query_disable_pattern(SQQuery *self, uint32_t pattern_index);
SQQueryCursor *sq_query_cursor_new(void);
void sq_query_cursor_delete(SQQueryCursor *self);
void sq_query_cursor_exec(SQQueryCursor *self, const SQQuery *query, SQNode node);
void sq_query_cursor_exec_with_options(SQQueryCursor *self, const SQQuery *query, SQNode node,
                                       const TSQueryCursorOptions *query_options);
bool sq_query_cursor_did_exceed_match_limit(const SQQueryCursor *self);
uint32_t sq_query_cursor_match_limit(const SQQueryCursor *self);
void sq_query_cursor_set_match_limit(SQQueryCursor *self, uint32_t limit);
bool sq_query_cursor_set_byte_range(SQQueryCursor *self, uint32_t start_byte, uint32_t end_byte);
bool sq_query_cursor_set_point_range(SQQueryCursor *self, TSPoint start_point, TSPoint end_point);

bool sq_query_cursor_set_containing_byte_range(SQQueryCursor *self, uint32_t start_byte,
                                               uint32_t end_byte);
bool sq_query_cursor_set_containing_point_range(SQQueryCursor *self, TSPoint start_point,
                                                TSPoint end_point);

bool sq_query_cursor_next_match(SQQueryCursor *self, SQQueryMatch *match);
void sq_query_cursor_remove_match(SQQueryCursor *self, uint32_t match_id);
// Advance to a capture event. The match is a provisional snapshot: it may gain
// captures or lose longest-match filtering, and captures may repeat across
// states. Event order is unspecified. Use next_match for completed, longest matches.
bool sq_query_cursor_next_capture(SQQueryCursor *self, SQQueryMatch *match,
                                  uint32_t *capture_index);
void sq_query_cursor_set_max_start_depth(SQQueryCursor *self, uint32_t max_start_depth);

// Disable optimizations for differential testing or diagnosis. Set before exec.
void sq_query_cursor_set_optimized(SQQueryCursor *, bool);

// A bounded range with a rootless or branching pattern depends on omitted hidden-node
// traversal barriers. Such execution is explicitly rejected, not approximated.
SQQueryExecutionError sq_query_cursor_error(const SQQueryCursor *);
#ifdef __cplusplus
}
#endif
#endif
