#ifndef TREE_SITTER_SQUAT_H_
#define TREE_SITTER_SQUAT_H_

#include <tree_sitter/api.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Immutable packed trees. Link this library alongside this checkout's runtime.
 * Slabs use native endianness and require the exact matching grammar. */
typedef struct SQTree SQTree;
typedef struct {
  const SQTree *tree;
  uint32_t slot;
} SQNode;
typedef struct SQCursor SQCursor;
/* A copied snapshot. Strings borrow the tree's retained language. Counts include
 * visible nodes only; descendant_count includes the current node. */
typedef struct {
  const char *type, *grammar_type;
  uint32_t start_byte, end_byte;
  TSPoint start_point, end_point;
  uint32_t child_count, named_child_count, descendant_count;
  TSSymbol symbol, grammar_symbol;
  TSFieldId field_id;
  bool is_named, is_extra, is_missing, is_error, has_error;
} SQCursorAttributes;
/* Encoded node columns: coordinate values are deltas from their group bases;
 * symbols are raw IDs with builtin errors remapped after the grammar range. */
typedef enum {
  SQ_COLUMN_SUBTREE_SIZE,
  SQ_COLUMN_START_BYTE,
  SQ_COLUMN_END_BYTE_SUB,
  SQ_COLUMN_START_ROW,
  SQ_COLUMN_END_ROW_SUB,
  SQ_COLUMN_START_COL,
  SQ_COLUMN_END_COL_SUB,
  SQ_COLUMN_SUPERTYPES,
  SQ_COLUMN_DISPLAY_SYMBOL,
  SQ_COLUMN_GRAMMAR_SYMBOL,
  SQ_COLUMN_FIELD,
  SQ_COLUMN_COUNT
} SQColumn;
typedef enum {
  SQ_OK = 0,
  SQ_ERROR_ARGUMENT,
  SQ_ERROR_ALLOCATION,
  SQ_ERROR_OVERFLOW,
  SQ_ERROR_DICTIONARY_FULL,
  SQ_ERROR_INVALID_SLAB,
  SQ_ERROR_LANGUAGE
} SQError;
typedef struct {
  /* Zero selects an estimate. Small values are useful for limiting initial allocation. */
  uint32_t initial_group_capacity;
  bool repack;
  bool symbol_presence;
} SQPackOptions;

const char *sq_error_string(SQError);
SQPackOptions sq_pack_options_default(void);
SQTree *sq_tree_pack(const TSTree *, SQPackOptions, SQError *);
/* Parse without an old tree, pack, then release the mainline tree. */
SQTree *sq_tree_parse(TSParser *, const char *, uint32_t, SQPackOptions, SQError *);
void sq_tree_delete(SQTree *);
const TSLanguage *sq_tree_language(const SQTree *);
const void *sq_tree_data(const SQTree *, uint32_t *length);
/* Copies and validates input, including topology and auxiliary indexes. */
SQTree *sq_tree_from_bytes(const TSLanguage *, const void *, size_t, SQError *);
/* Returns an independent compact copy; nodes in the original remain valid. */
SQTree *sq_tree_repack(const SQTree *, SQError *);
uint32_t sq_tree_group_count(const SQTree *);
uint32_t sq_tree_group_capacity(const SQTree *);
uint32_t sq_tree_slot_count(const SQTree *);
SQNode sq_tree_root_node(const SQTree *);
/* Invalid/wasted slots return null. Logical slots are stable across repacking. */
SQNode sq_tree_node_at_slot(const SQTree *, uint32_t);
/* False positives are possible for common symbols, never false negatives. */
/* One bit per physical slot within a group; leading waste never matches.
 * This compares encoded values and does not apply public-symbol mapping. */
uint64_t sq_tree_group_equal(const SQTree *, uint32_t group, SQColumn, uint32_t value);
bool sq_tree_group_has_symbol(const SQTree *, uint32_t group, TSSymbol public_symbol);

bool sq_node_is_null(SQNode);
bool sq_node_eq(SQNode, SQNode);
TSSymbol sq_node_symbol(SQNode);
TSSymbol sq_node_grammar_symbol(SQNode);
const char *sq_node_type(SQNode);
const char *sq_node_grammar_type(SQNode);
uint32_t sq_node_start_byte(SQNode);
uint32_t sq_node_end_byte(SQNode);
TSPoint sq_node_start_point(SQNode);
TSPoint sq_node_end_point(SQNode);
bool sq_node_is_named(SQNode);
bool sq_node_is_extra(SQNode);
bool sq_node_is_missing(SQNode);
bool sq_node_is_error(SQNode);
bool sq_node_has_error(SQNode);
bool sq_node_has_changes(SQNode);
bool sq_node_has_supertype(SQNode, TSSymbol);
TSFieldId sq_node_field_id(SQNode);
const char *sq_node_field_name(SQNode);
/* First physical slot outside this subtree; may equal the tree's slot count. */
uint32_t sq_node_end_slot(SQNode);
uint32_t sq_node_descendant_count(SQNode);
uint32_t sq_node_child_count(SQNode);
uint32_t sq_node_named_child_count(SQNode);
SQNode sq_node_parent(SQNode);
SQNode sq_node_child(SQNode, uint32_t);
SQNode sq_node_named_child(SQNode, uint32_t);
SQNode sq_node_next_sibling(SQNode);
/* Structural iteration includes empty siblings that mainline's node accessor
 * skips. Child enumeration and cursors use this form. */
SQNode sq_node_next_sibling_including_empty(SQNode);
SQNode sq_node_prev_sibling(SQNode);
SQNode sq_node_next_named_sibling(SQNode);
SQNode sq_node_prev_named_sibling(SQNode);
SQNode sq_node_child_by_field_id(SQNode, TSFieldId);
SQNode sq_node_child_by_field_name(SQNode, const char *, uint32_t);
const char *sq_node_field_name_for_child(SQNode, uint32_t);
const char *sq_node_field_name_for_named_child(SQNode, uint32_t);
SQNode sq_node_child_with_descendant(SQNode, SQNode);
SQNode sq_node_first_child_for_byte(SQNode, uint32_t);
SQNode sq_node_first_named_child_for_byte(SQNode, uint32_t);
SQNode sq_node_descendant_for_byte_range(SQNode, uint32_t, uint32_t);
SQNode sq_node_named_descendant_for_byte_range(SQNode, uint32_t, uint32_t);
SQNode sq_node_descendant_for_point_range(SQNode, TSPoint, TSPoint);
SQNode sq_node_named_descendant_for_point_range(SQNode, TSPoint, TSPoint);
/* Preorder traversal stays within this node's tree, and returns null at ends. */
SQNode sq_node_next_preorder(SQNode);
SQNode sq_node_prev_preorder(SQNode);

SQCursor *sq_cursor_new(SQNode);
void sq_cursor_delete(SQCursor *);
SQNode sq_cursor_node(const SQCursor *);
SQNode sq_cursor_parent_node(const SQCursor *);
uint32_t sq_cursor_depth(const SQCursor *);
bool sq_cursor_goto_first_child(SQCursor *);
bool sq_cursor_goto_last_child(SQCursor *);
bool sq_cursor_goto_next_sibling(SQCursor *);
bool sq_cursor_goto_parent(SQCursor *);

/* Trees must outlive cursors. Each cursor owns its ancestor stack. */
void sq_cursor_attributes(SQCursor *, SQCursorAttributes *);

#ifdef __cplusplus
}
#endif
#endif
