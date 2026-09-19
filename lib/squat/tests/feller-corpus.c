#define _POSIX_C_SOURCE 200809L
#include <tree_sitter/squat.h>
#include <dlfcn.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void quoted(const char *text) {
  putchar('"');
  for (const unsigned char *cursor = (const unsigned char *)text; *cursor; cursor++) {
    if (*cursor == '"' || *cursor == '\\') printf("\\%c", *cursor);
    else if (*cursor < 32) printf("\\u%04x", *cursor);
    else putchar(*cursor);
  }
  putchar('"');
}

static void result(uint32_t index, const char *status, const char *detail,
                   uint32_t expected_size, uint32_t actual_size) {
  printf("{\"index\":%u,\"status\":", index);
  quoted(status);
  printf(",\"detail\":");
  quoted(detail ? detail : "");
  printf(",\"expected_size\":%u,\"actual_size\":%u}\n", expected_size, actual_size);
  fflush(stdout);
}

static const char *semantic_difference(const SQTree *expected, const SQTree *actual,
                                        const TSLanguage *language, uint32_t *ordinal) {
  SQCursor *left = sq_cursor_new(sq_tree_root_node(expected));
  SQCursor *right = sq_cursor_new(sq_tree_root_node(actual));
  const char *difference = NULL;
  uint32_t supertype_count;
  const TSSymbol *supertypes = ts_language_supertypes(language, &supertype_count);
  if (!left || !right) { difference = "cursor allocation"; goto done; }
  for (*ordinal = 0;; (*ordinal)++) {
    SQCursorAttributes first, second;
    sq_cursor_attributes(left, &first);
    sq_cursor_attributes(right, &second);
#ifdef TRACE_NODES
    fprintf(stderr, "%u expected %s [%u,%u) (%u,%u)-(%u,%u); actual %s [%u,%u) (%u,%u)-(%u,%u)\n",
            *ordinal, first.type, first.start_byte, first.end_byte,
            first.start_point.row, first.start_point.column, first.end_point.row, first.end_point.column,
            second.type, second.start_byte, second.end_byte,
            second.start_point.row, second.start_point.column, second.end_point.row, second.end_point.column);
#endif
#define SAME(member) do { if (first.member != second.member) { difference = #member; goto done; } } while (0)
    SAME(symbol); SAME(grammar_symbol); SAME(field_id);
    SAME(start_byte); SAME(end_byte);
    SAME(start_point.row); SAME(start_point.column);
    SAME(end_point.row); SAME(end_point.column);
    SAME(is_named); SAME(is_extra); SAME(is_missing); SAME(is_error); SAME(has_error);
#undef SAME
    SQNode first_node = sq_cursor_node(left), second_node = sq_cursor_node(right);
    for (uint32_t index = 0; index < supertype_count; index++) {
      if (sq_node_has_supertype(first_node, supertypes[index]) !=
          sq_node_has_supertype(second_node, supertypes[index])) {
        difference = "supertype"; goto done;
      }
    }
    bool first_child = sq_cursor_goto_first_child(left);
    if (first_child != sq_cursor_goto_first_child(right)) {
      difference = "first child"; goto done;
    }
    if (first_child) continue;
    for (;;) {
      bool sibling = sq_cursor_goto_next_sibling(left);
      if (sibling != sq_cursor_goto_next_sibling(right)) {
        difference = "next sibling"; goto done;
      }
      if (sibling) break;
      bool parent = sq_cursor_goto_parent(left);
      if (parent != sq_cursor_goto_parent(right)) {
        difference = "parent"; goto done;
      }
      if (!parent) goto done;
    }
  }
done:
  sq_cursor_delete(left);
  sq_cursor_delete(right);
  return difference;
}

int main(int count, char **arguments) {
  if (count != 5) return 2;
  alarm(60);
  void *library = dlopen(arguments[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) { result(0, "grammar_load_failed", dlerror(), 0, 0); return 2; }
  const TSLanguage *(*language_function)(void) = dlsym(library, arguments[2]);
  if (!language_function) { result(0, "grammar_load_failed", dlerror(), 0, 0); return 2; }
  const TSLanguage *language = language_function();
  SQError error;
  SQGrammar *grammar = sq_grammar_new(language, &error);
  if (!grammar) { result(0, "grammar_rejected", sq_error_string(error), 0, 0); return 2; }
  SQParseError diagnostic;
  SQParser *direct = sq_parser_new(grammar, &diagnostic);
  if (!direct) { result(0, "grammar_rejected", diagnostic.message, 0, 0); return 2; }
  SQPackContext *pack = sq_pack_context_new(&error);
  TSParser *mainline = ts_parser_new();
  if (!pack || !mainline || !ts_parser_set_language(mainline, language)) return 2;
  SQPackOptions options = sq_pack_options_default();
  options.repack = true;
  FILE *manifest = fopen(arguments[3], "rb");
  if (!manifest) return 2;
  uint32_t first = (uint32_t)strtoul(arguments[4], NULL, 10);
  const char *configured_timeout = getenv("CORPUS_TIMEOUT_SECONDS");
  unsigned timeout_seconds = configured_timeout ? (unsigned)strtoul(configured_timeout, NULL, 10) : 30;
  char *path = NULL;
  size_t capacity = 0;
  uint32_t index = 0;
  alarm(0);
  puts("{\"status\":\"ready\"}");
  fflush(stdout);
  while (getdelim(&path, &capacity, '\0', manifest) >= 0) {
    if (index++ < first) continue;
    alarm(timeout_seconds);
    FILE *source = fopen(path, "rb");
    if (!source || fseek(source, 0, SEEK_END)) {
      if (source) fclose(source);
      result(index - 1, "read_failed", path, 0, 0); continue;
    }
    long length = ftell(source);
    if (length < 0 || (unsigned long)length >= UINT32_MAX) {
      fclose(source); result(index - 1, "read_failed", "file length", 0, 0); continue;
    }
    rewind(source);
    char *bytes = malloc((size_t)length + 1);
    if (!bytes || fread(bytes, 1, (size_t)length, source) != (size_t)length) {
      free(bytes); fclose(source); result(index - 1, "read_failed", path, 0, 0); continue;
    }
    fclose(source);
    bytes[length] = 0;
    TSTree *native = ts_parser_parse_string(mainline, NULL, bytes, (uint32_t)length);
    SQTree *expected = NULL, *actual = NULL;
    const char *status;
    char detail[640] = "";
    uint32_t expected_size = 0, actual_size = 0;
    if (!native) {
      status = "mainline_failed";
    } else if (ts_node_has_error(ts_tree_root_node(native))) {
      status = "mainline_syntax_error";
    } else if (!(expected = sq_pack_context_pack(pack, grammar, native, options, &error))) {
      status = "reference_pack_failed";
      snprintf(detail, sizeof(detail), "%s", sq_error_string(error));
    } else {
      ts_tree_delete(native);
      native = NULL;
      actual = sq_parser_parse(direct, bytes, (uint32_t)length, options, &diagnostic);
      if (!actual) {
        status = diagnostic.code == SQ_ERROR_PARSE ? "feller_rejected" : "feller_failed";
        snprintf(detail, sizeof(detail), "code %u, byte %u: %s", diagnostic.code,
                 diagnostic.byte, diagnostic.message);
      } else {
        const void *expected_bytes = sq_tree_data(expected, &expected_size);
        const void *actual_bytes = sq_tree_data(actual, &actual_size);
        if (expected_size == actual_size && !memcmp(expected_bytes, actual_bytes, expected_size)) {
          status = "equal";
        } else {
          uint32_t ordinal = 0;
          const char *difference = semantic_difference(expected, actual, language, &ordinal);
          snprintf(detail, sizeof(detail), "node %u: %s", ordinal,
                   difference ? difference : "matching structure, attributes, and supertypes");
          status = difference ? "semantic_mismatch" : "bytes_only_mismatch";
        }
      }
    }
    sq_tree_delete(actual);
    sq_tree_delete(expected);
    ts_tree_delete(native);
    free(bytes);
    alarm(0);
    result(index - 1, status, detail, expected_size, actual_size);
  }
  free(path);
  fclose(manifest);
  sq_parser_delete(direct);
  sq_pack_context_delete(pack);
  ts_parser_delete(mainline);
  sq_grammar_delete(grammar);
  dlclose(library);
  return 0;
}
