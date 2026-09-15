#define _POSIX_C_SOURCE 200809L
#include <tree_sitter/squat_query.h>
#include "../query_internal.h"
#include "field_lookup.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *query_source;
static unsigned mode, optimized, event;
static const char *input_source;
static const char *presence_query =
    "(object (pair key: (string) @key value: (string) @value)) @object";
static const char *presence_source = "[{\"x\":1},{\"x\":2},{\"x\":\"yes\"}]";
static unsigned expected_field_query_mismatches;
#define CHECK(value)                                                                               \
  do {                                                                                             \
    if (!(value)) {                                                                                \
      fprintf(stderr, "query %s mode %u optimized %u event %u: %s at %d\n", query_source, mode,    \
              optimized, event, #value, __LINE__);                                                 \
      abort();                                                                                     \
    }                                                                                              \
  } while (0)

typedef struct {
  TSNode *nodes;
  SQNode *packed;
  uint32_t count;
} Identities;

static Identities identities(TSTree *tree, SQTree *packed) {
  uint32_t count = ts_node_descendant_count(ts_tree_root_node(tree));
  Identities result = {malloc(count * sizeof(TSNode)), malloc(count * sizeof(SQNode)), count};
  CHECK(result.nodes && result.packed);
  TSTreeCursor cursor = ts_tree_cursor_new(ts_tree_root_node(tree));
  SQNode node = sq_tree_root_node(packed);
  uint32_t index = 0;
  for (;;) {
    CHECK(index < count && node.tree);
    result.nodes[index] = ts_tree_cursor_current_node(&cursor);
    result.packed[index++] = node;
    node = sq_node_next_preorder(node);
    if (ts_tree_cursor_goto_first_child(&cursor)) {
      continue;
    }

    for (;;) {
      if (ts_tree_cursor_goto_next_sibling(&cursor)) {
        break;
      }

      if (!ts_tree_cursor_goto_parent(&cursor)) {
        goto done;
      }
    }
  }

done:
  CHECK(index == count && !node.tree);
  ts_tree_cursor_delete(&cursor);
  return result;
}

static void compare_node(const Identities *ids, TSNode node, SQNode packed) {
  if (ts_node_is_null(node)) {
    CHECK(sq_node_is_null(packed));
    return;
  }

  for (uint32_t index = 0; index < ids->count; index++) {
    if (ts_node_eq(node, ids->nodes[index])) {
      CHECK(sq_node_eq(packed, ids->packed[index]));
      return;
    }
  }

  CHECK(false);
}

static void check_packed_node(const Identities *ids, SQNode node) {
  for (uint32_t index = 0; index < ids->count; index++) {
    if (sq_node_eq(node, ids->packed[index])) {
      return;
    }
  }

  CHECK(false);
}

// This allowance applies only to the generated single-capture wildcard query
// below. A more complex query difference must still fail: a field discrepancy
// somewhere in a captured subtree is not sufficient evidence to excuse it.
static TSFieldId simple_negated_field(const TSLanguage *language, const char *source) {
  if (strncmp(source, "(_ !", 4)) {
    return 0;
  }

  const char *end = strchr(source + 4, ')');
  if (!end || strcmp(end, ") @parent")) {
    return 0;
  }

  return ts_language_field_id_for_name(language, source + 4, (uint32_t)(end - source - 4));
}

static bool expected_negated_field_difference(const Identities *ids, uint32_t ordinal,
                                              TSFieldId field) {
  TSNode parent = ids->nodes[ordinal];
  TSNode lookup = ts_node_child_by_field_id(parent, field);
  TSNode visible = visible_child_by_field(parent, field);
  if (ts_node_is_null(lookup) == ts_node_is_null(visible)) {
    return false;
  }

  compare_node(ids, visible, sq_node_child_by_field_id(ids->packed[ordinal], field));
  return true;
}

static bool expected_mainline_field_match(const Identities *ids, const TSQueryMatch *match,
                                          TSFieldId field) {
  if (!field) {
    return false;
  }

  CHECK(match->pattern_index == 0 && match->capture_count == 1 && match->captures[0].index == 0);
  for (uint32_t index = 0; index < ids->count; index++) {
    if (ts_node_eq(match->captures[0].node, ids->nodes[index])) {
      bool expected = expected_negated_field_difference(ids, index, field);
      if (expected) {
        CHECK(ts_node_is_null(ts_node_child_by_field_id(ids->nodes[index], field)));
      }

      return expected;
    }
  }

  CHECK(false);
  return false;
}

static bool expected_packed_field_match(const Identities *ids, const SQQueryMatch *match,
                                        TSFieldId field) {
  if (!field) {
    return false;
  }

  CHECK(match->pattern_index == 0 && match->capture_count == 1 && match->captures[0].index == 0);
  for (uint32_t index = 0; index < ids->count; index++) {
    if (sq_node_eq(match->captures[0].node, ids->packed[index])) {
      bool expected = expected_negated_field_difference(ids, index, field);
      if (expected) {
        CHECK(sq_node_is_null(sq_node_child_by_field_id(ids->packed[index], field)));
      }

      return expected;
    }
  }

  CHECK(false);
  return false;
}

static bool cancel(TSQueryCursorState *state) {
  (void)state;
  return true;
}

static bool capture_in_range(TSNode node) {
  uint32_t start = mode == 2 ? 1 : 0;
  uint32_t end = mode == 2 ? 12 : UINT32_MAX;
  if (ts_node_end_byte(node) <= start || ts_node_start_byte(node) >= end) {
    return false;
  }
  if (mode == 5) {
    TSPoint start_point = ts_node_start_point(node), end_point = ts_node_end_point(node);
    return (end_point.row || end_point.column > 1) && start_point.row < 1;
  }
  return true;
}

static void run_query(const TSLanguage *language, TSTree *tree, SQTree *packed,
                      const Identities *ids, const char *source) {
  query_source = source;
  TSFieldId negated_field = simple_negated_field(language, source);
  uint32_t offset_a = 0, offset_b = 0;
  TSQueryError error_a = 0, error_b = 0;
  TSQuery *mainline = ts_query_new(language, source, (uint32_t)strlen(source), &offset_a, &error_a);
  SQQuery *query = sq_query_new(language, source, (uint32_t)strlen(source), &offset_b, &error_b);
  CHECK((mainline != NULL) == (query != NULL));
  if (!mainline) {
    CHECK(error_a == error_b && offset_a == offset_b);
    return;
  }

  CHECK(ts_query_pattern_count(mainline) == sq_query_pattern_count(query));
  CHECK(ts_query_capture_count(mainline) == sq_query_capture_count(query));
  SQQuery *copy = sq_query_copy(query);
  CHECK(copy);
  sq_query_delete(query);
  query = copy;
  for (optimized = 0; optimized < 2; optimized++) {
    for (mode = 0; mode < 9; mode++) {
      size_t capture_slots = (size_t)sq_query_capture_count(query) * ids->count;
      bool *seen = calloc(sq_query_pattern_count(query) * capture_slots + 1, sizeof(bool));
      CHECK(seen);
      TSQueryCursor *a = ts_query_cursor_new();
      SQQueryCursor *b = sq_query_cursor_new();
      sq_query_cursor_set_optimized(b, optimized);
      if (mode == 2) {
        ts_query_cursor_set_byte_range(a, 1, 12);
        sq_query_cursor_set_byte_range(b, 1, 12);
      } else if (mode == 3) {
        ts_query_cursor_set_max_start_depth(a, 1);
        sq_query_cursor_set_max_start_depth(b, 1);
      } else if (mode == 4) {
        ts_query_cursor_set_match_limit(a, 2);
        sq_query_cursor_set_match_limit(b, 2);
      } else if (mode == 5) {
        ts_query_cursor_set_point_range(a, (TSPoint){0, 1}, (TSPoint){1, 0});
        sq_query_cursor_set_point_range(b, (TSPoint){0, 1}, (TSPoint){1, 0});
      }

      TSQueryCursorOptions options = {.progress_callback = cancel};
      ts_query_cursor_exec_with_options(a, mainline, ts_tree_root_node(tree),
                                        mode == 6 ? &options : NULL);
      sq_query_cursor_exec_with_options(b, query, sq_tree_root_node(packed),
                                        mode == 6 ? &options : NULL);
      if (mode == 8) {
        ts_query_cursor_set_match_limit(a, 2);
        sq_query_cursor_set_match_limit(b, 2);
      }

      for (event = 0; event < 100000; event++) {
        TSQueryMatch expected;
        SQQueryMatch actual;
        uint32_t capture_a = 0, capture_b = 0;
        bool found_a, found_b;
        for (;;) {
          found_a = mode == 0 ? ts_query_cursor_next_match(a, &expected)
                              : ts_query_cursor_next_capture(a, &expected, &capture_a);
          if (!found_a || !expected_mainline_field_match(ids, &expected, negated_field)) {
            break;
          }

          CHECK(capture_a == 0);
          expected_field_query_mismatches++;
        }

        for (;;) {
          found_b = mode == 0 ? sq_query_cursor_next_match(b, &actual)
                              : sq_query_cursor_next_capture(b, &actual, &capture_b);
          if (!found_b || !expected_packed_field_match(ids, &actual, negated_field)) {
            break;
          }

          CHECK(capture_b == 0);
          expected_field_query_mismatches++;
        }

        if ((mode == 4 || mode == 8) && optimized &&
            !ts_node_has_error(ts_tree_root_node(tree)) && !strcmp(source, "(_) @node")) {
          CHECK(sq_query_cursor__execution_stats(b).planned);
        }

        if ((mode == 2 || mode == 5) && sq_query_cursor_error(b) == SQ_QUERY_UNSUPPORTED_RANGE) {
          CHECK(!found_b && event == 0);
          break;
        }

        // Callback cadence counts representation-specific traversal events.
        // Both executions must stop, but need not stop at the same capture.
        if (mode == 6) {
          if (!found_a && !found_b) {
            break;
          } else {
            continue;
          }
        }

        // A finite match limit bounds storage, not which valid matches survive.
        // Different execution strategies may therefore return different subsets.
        if (mode == 4 || mode == 8) {
          if (found_b) {
            CHECK(actual.pattern_index < sq_query_pattern_count(query));
            CHECK(capture_b < actual.capture_count);
            for (uint32_t index = 0; index < actual.capture_count; index++) {
              CHECK(actual.captures[index].index < sq_query_capture_count(query));
              check_packed_node(ids, actual.captures[index].node);
            }
          }

          if (!found_a && !found_b) {
            break;
          }

          continue;
        }

        if (mode != 0) {
          if (found_b) {
            CHECK(actual.pattern_index < sq_query_pattern_count(query));
            CHECK(capture_b < actual.capture_count);
            for (uint32_t index = 0; index < actual.capture_count; index++) {
              CHECK(actual.captures[index].index < sq_query_capture_count(query));
              check_packed_node(ids, actual.captures[index].node);
            }
            SQQueryCapture capture = actual.captures[capture_b];
            for (uint32_t index = 0; index < ids->count; index++) {
              if (sq_node_eq(capture.node, ids->packed[index])) {
                CHECK(capture_in_range(ids->nodes[index]));
                seen[actual.pattern_index * capture_slots + capture.index * ids->count + index] = true;
                break;
              }
            }
            if (mode == 7 && event % 2 == 0) {
              sq_query_cursor_remove_match(b, actual.id);
            }
          }
          if (found_a && mode == 7 && event % 2 == 0) {
            ts_query_cursor_remove_match(a, expected.id);
          }
          if (!found_a && !found_b) {
            break;
          }
          continue;
        }

        CHECK(found_a == found_b);
        if (!found_a) {
          break;
        }

        if (expected.pattern_index != actual.pattern_index ||
            expected.capture_count != actual.capture_count) {
          fprintf(stderr, "expected pattern %u captures %u, actual pattern %u captures %u\n",
                  expected.pattern_index, expected.capture_count, actual.pattern_index,
                  actual.capture_count);
        }

        CHECK(expected.pattern_index == actual.pattern_index);
        CHECK(expected.capture_count == actual.capture_count);
        for (uint32_t index = 0; index < expected.capture_count; index++) {
          CHECK(expected.captures[index].index == actual.captures[index].index);
          compare_node(ids, expected.captures[index].node, actual.captures[index].node);
        }
      }

      CHECK(event < 100000);
#ifdef TS_QUERY_EXEC_STATS
      if (mode == 0 && source == presence_query && input_source == presence_source) {
        QueryExecutionStats stats = sq_query_cursor__execution_stats(b);
        if (optimized) {
          CHECK(stats.presence_rejections > 0);
        }
        printf("capture presence filter optimized %u: %llu active steps, %llu captures, "
               "%llu rejected roots\n",
               optimized, (unsigned long long)stats.active_steps,
               (unsigned long long)stats.materialized_captures,
               (unsigned long long)stats.presence_rejections);
      }
#endif
      if ((mode == 1 || mode == 2 || mode == 3 || mode == 5) &&
          sq_query_cursor_error(b) == SQ_QUERY_OK) {
        // Provisional states may add events, but completed captures must survive.
        ts_query_cursor_exec(a, mainline, ts_tree_root_node(tree));
        TSQueryMatch match;
        while (ts_query_cursor_next_match(a, &match)) {
          if (expected_mainline_field_match(ids, &match, negated_field)) {
            continue;
          }
          for (uint32_t capture = 0; capture < match.capture_count; capture++) {
            if (!capture_in_range(match.captures[capture].node)) {
              continue;
            }
            for (uint32_t index = 0; index < ids->count; index++) {
              if (ts_node_eq(match.captures[capture].node, ids->nodes[index])) {
                CHECK(seen[match.pattern_index * capture_slots +
                           match.captures[capture].index * ids->count + index]);
                break;
              }
            }
          }
        }
      }
      if (mode != 4 && mode != 8) {
        CHECK(ts_query_cursor_did_exceed_match_limit(a) ==
              sq_query_cursor_did_exceed_match_limit(b));
      }
      ts_query_cursor_delete(a);
      sq_query_cursor_delete(b);
      free(seen);
    }
  }

  ts_query_delete(mainline);
  sq_query_delete(query);
}

static void exercise(const TSLanguage *language, const char *source, uint32_t length) {
  input_source = source;
  TSParser *parser = ts_parser_new();
  CHECK(ts_parser_set_language(parser, language));
  TSTree *tree = ts_parser_parse_string(parser, NULL, source, length);
  SQError error;
  SQPackOptions options = sq_pack_options_default();
  options.initial_group_capacity = 1;
  SQTree *packed = sq_tree_pack(tree, options, &error);
  CHECK(packed);
  Identities ids = identities(tree, packed);
  const char *queries[] = {
      "(_) @node",
      "(_) @one (_) @two",
      "[(ERROR) (_)] @node",
      "(MISSING) @missing",
      "(_ (_) @child) @parent",
      "(_ . (_) @first)",
      "(_ (_) @last .)",
      "(_ (_) @first . (_) @second)",
      "(_ . (_) @first . (_) @second)",
      "(_ (_)+ @children) @parent",
      "(_ (_)* @children) @parent",
      "(_ (_)? @child . (_) @last)",
      presence_query,
      "[(_) (_)] @alternative",
      "((_) @text (#eq? @text \"x\"))",
      "((_) @text (#match? @text \"^[a-z]+$\"))",
      "(not_a_real_symbol) @invalid",
      "(_",
      "(_) @",
  };
  for (unsigned index = 0; index < sizeof(queries) / sizeof(queries[0]); index++) {
    run_query(language, tree, packed, &ids, queries[index]);
  }

  for (uint32_t index = 0; index < ids.count && index < 24; index++) {
    if (!ts_node_is_named(ids.nodes[index])) {
      continue;
    }

    char query[512];
    snprintf(query, sizeof(query), "(%s) @specific", ts_node_type(ids.nodes[index]));
    run_query(language, tree, packed, &ids, query);
    snprintf(query, sizeof(query), "(%s . (_) @first) @parent", ts_node_type(ids.nodes[index]));
    run_query(language, tree, packed, &ids, query);
  }

  // Multiple raw roots exercise merged SWAR comparisons; supertypes exercise
  // the inherited mask even when no visible node has the supertype's symbol.
  char alternatives[2048] = "[";
  size_t used = 1;
  unsigned roots = 0;
  for (uint32_t symbol = 0; symbol < ts_language_symbol_count(language); symbol++) {
    TSSymbolType type = ts_language_symbol_type(language, (TSSymbol)symbol);
    const char *name = ts_language_symbol_name(language, (TSSymbol)symbol);
    char query[512];
    if (type == TSSymbolTypeSupertype) {
      snprintf(query, sizeof(query), "(%s) @supertype", name);
      run_query(language, tree, packed, &ids, query);
    } else if (type == TSSymbolTypeRegular && roots < 12) {
      int written = snprintf(alternatives + used, sizeof(alternatives) - used, "(%s) ", name);
      CHECK(written > 0 && (size_t)written < sizeof(alternatives) - used);
      used += (size_t)written;
      roots++;
    }
  }

  if (roots) {
    snprintf(alternatives + used, sizeof(alternatives) - used, "] @roots");
    run_query(language, tree, packed, &ids, alternatives);
  }

  for (uint32_t field = 1; field <= ts_language_field_count(language); field++) {
    char query[512];
    const char *name = ts_language_field_name_for_id(language, (TSFieldId)field);
    snprintf(query, sizeof(query), "(_ %s: (_) @child) @parent", name);
    run_query(language, tree, packed, &ids, query);
    snprintf(query, sizeof(query), "(_ !%s) @parent", name);
    run_query(language, tree, packed, &ids, query);
  }

  free(ids.nodes);
  free(ids.packed);
  sq_tree_delete(packed);
  ts_tree_delete(tree);
  ts_parser_delete(parser);
}

int main(int argc, char **argv) {
  CHECK(argc >= 3);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) {
    fprintf(stderr, "%s\n", dlerror());
    return 2;
  }

  const TSLanguage *(*language_fn)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  CHECK(language_fn);
  const TSLanguage *language = language_fn();
  const char *samples[] = {"",
                           "x",
                           "{\"x\": [1, 2, 3], \"y\": true}",
                           presence_source,
                           "{\"x\": [1,",
                           "function f(x) { return x + 1; }",
                           "type X = typeof obj.member;",
                           "// comment\n x = 1\n"};
  for (unsigned i = 0; i < sizeof(samples) / sizeof(samples[0]); i++) {
    exercise(language, samples[i], (uint32_t)strlen(samples[i]));
  }

  for (int i = 3; i < argc; i++) {
    FILE *file = fopen(argv[i], "rb");
    CHECK(file);
    CHECK(!fseek(file, 0, SEEK_END));
    long length = ftell(file);
    CHECK(length >= 0 && length < 65536);
    rewind(file);
    char *source = malloc((size_t)length + 1);
    CHECK(source);
    CHECK(fread(source, 1, (size_t)length, file) == (size_t)length);
    fclose(file);
    exercise(language, source, (uint32_t)length);
    free(source);
  }

  printf("ok: query matches, capture coverage, ranges, limits, removal, and optimization "
         "modes: %s\n",
         argv[2]);
  printf("expected negated-field query mismatches: %u\n", expected_field_query_mismatches);
  dlclose(library);
}
