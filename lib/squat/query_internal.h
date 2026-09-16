#ifndef TREE_SQUATTER_QUERY_EXEC_H_
#define TREE_SQUATTER_QUERY_EXEC_H_

#include "../src/array.h"
#include "tree_sitter/api.h"

typedef enum {
  QueryExecutionRoot,
  QueryExecutionFirstNamedChild,
  QueryExecutionNextNamedSibling,
} QueryExecutionRelation;

typedef struct {
  QueryExecutionRelation relation;
  TSSymbol symbol;
  TSFieldId field;
  bool last_named_child;
} QueryExecutionStep;

typedef struct {
  Array(QueryExecutionStep) steps;
  Array(uint64_t) roots;
  uint16_t start_steps[64], end_steps[64];
  uint64_t local_patterns;
  bool supported;
} QueryExecutionPlan;

typedef struct {
  uint32_t root, next, end;
} QueryExecutionState;

typedef struct {
  TSSymbol symbol;
  TSFieldId field;
} QueryPresenceRequirement;

typedef struct {
  uint32_t start, next;
  bool found;
  uint8_t samples, rejections, cooldown;
} QueryPresenceCache;

typedef struct {
  uint64_t candidates;
  uint64_t root_state_shifts;
  uint64_t records_skipped;
  uint64_t active_steps;
  uint64_t depth_rejections;
  uint64_t symbol_rejections;
  uint64_t matched_steps;
  uint64_t branches;
  uint64_t capture_copies;
  uint64_t capture_shares;
  uint64_t capture_prefix_skips;
  uint64_t materialized_captures;
  uint64_t snapshot_captures;
  uint64_t seek_restoration_steps;
  uint64_t execution_steps;
  uint64_t local_steps;
  uint64_t dedup_passes;
  uint64_t dedup_skips;
  uint64_t staged_states;
  uint64_t capture_index_entries;
  uint64_t capture_index_blocks;
  uint64_t indexed_slots_skipped;
  uint64_t capture_set_slots_skipped;
  uint64_t inactive_steps_skipped;
  uint64_t capture_comparisons;
  uint64_t capture_filter_rejections;
  uint64_t presence_checks;
  uint64_t presence_rejections;
  uint64_t presence_words;
  uint64_t presence_unknown;
  uint64_t presence_cached;
  uint64_t presence_bypassed;
  bool planned;
} QueryExecutionStats;

// Counters are opt-in; plan selection remains observable in ordinary builds.
QueryExecutionStats sq_query_cursor__execution_stats(const SQQueryCursor *cursor);

#ifdef TS_QUERY_EXEC_STATS
#define QUERY_EXEC_COUNT(cursor, name, count) ((cursor)->execution_stats.name += (count))
#else
#define QUERY_EXEC_COUNT(cursor, name, count) ((void)0)
#endif

#endif
