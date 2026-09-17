#ifndef TREE_SITTER_SQUAT_H_
#define TREE_SITTER_SQUAT_H_

#include <tree_sitter/api.h>

#ifdef __cplusplus
extern "C" {
#endif

// Immutable packed trees. Link this library alongside this checkout's runtime.
// Slabs use little-endian encoding and require the exact matching grammar.
typedef struct SQTree SQTree;
typedef struct SQGrammar SQGrammar;
typedef struct {
  const SQTree *tree;
  // Physical reverse-preorder index; preorder moves downward.
  uint32_t slot;
} SQNode;

typedef struct SQCursor SQCursor;
typedef struct SQNodeIterator SQNodeIterator;

// A constant-time snapshot. Strings borrow the tree's retained language.
// Child and descendant counts are available through explicit node APIs.
typedef struct {
  const char *type, *grammar_type;
  uint32_t start_byte, end_byte;
  TSPoint start_point, end_point;
  TSSymbol symbol, grammar_symbol;
  TSFieldId field_id;
  bool is_named, is_extra, is_missing, is_error;
  // Same conservative block-level predicate as sq_node_has_error.
  bool has_error;
} SQCursorAttributes;

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
  // Zero selects an estimate. Small values are useful for limiting initial allocation.
  uint32_t initial_group_capacity;
  bool repack;
  bool symbol_presence;
  // Store source row/column positions. Point APIs use byte offsets on row zero
  // when these columns are omitted.
  bool points;
} SQPackOptions;

const char *sq_error_string(SQError);
// Actual compiled slab format/configuration, for persistence identity.
uint64_t sq_representation_id(void);
SQPackOptions sq_pack_options_default(void);
// Prepared immutable metadata shared across contexts and trees. Native grammar
// libraries must remain loaded until every derived handle has been released.
// Construction copies cached dictionary bytes; their storage may be released on
// return. Other tables are derived from the language. More than 65536 dictionary
// masks fails with SQ_ERROR_DICTIONARY_FULL. Grammars reject symbol
// or field IDs exceeding 16 bits with SQ_ERROR_OVERFLOW. No global registry is used.
SQGrammar *sq_grammar_new(const TSLanguage *, SQError *);
SQGrammar *sq_grammar_new_with_cache(const TSLanguage *, const void *, size_t, SQError *);
SQGrammar *sq_grammar_copy(SQGrammar *);
void sq_grammar_delete(SQGrammar *);
const TSLanguage *sq_grammar_language(const SQGrammar *);
// Only the costly dictionary is serialized. Zero size means no dictionary.
uint32_t sq_grammar_cache_size(const SQGrammar *);
bool sq_grammar_copy_cache(const SQGrammar *, void *, size_t, SQError *);

SQTree *sq_tree_pack(SQGrammar *, const TSTree *, SQPackOptions, SQError *);

// A context reuses mutable scratch across grammars. Calls reset transient
// state even after failure. Separate contexts can share a grammar concurrently;
// one context requires exclusive access. Output trees outlive the context.
typedef struct SQPackContext SQPackContext;
SQPackContext *sq_pack_context_new(SQError *);
SQTree *sq_pack_context_pack(SQPackContext *, SQGrammar *, const TSTree *, SQPackOptions, SQError *);
// Release high-water scratch. NULL is allowed.
void sq_pack_context_trim(SQPackContext *);
void sq_pack_context_delete(SQPackContext *);

// Parse without an old tree, pack, then release the mainline tree.
SQTree *sq_tree_parse(SQGrammar *, TSParser *, const char *, uint32_t, SQPackOptions, SQError *);
void sq_tree_delete(SQTree *);
const TSLanguage *sq_tree_language(const SQTree *);
// Borrowed handle, valid while the tree is alive.
SQGrammar *sq_tree_grammar(const SQTree *);
const void *sq_tree_data(const SQTree *, uint32_t *length);

// Copies into one owned allocation and validates topology and auxiliary indexes.
SQTree *sq_tree_from_bytes(SQGrammar *, const void *, size_t, SQError *);

// Copies and validates layout, topology, indexes, and coordinate arithmetic,
// without checking auxiliary index membership or canonical padding contents.
// Incorrect but bounded auxiliary contents may produce incorrect query results.
// This does not verify grammar identity or the tree's agreement with source text.
SQTree *sq_tree_from_bytes_safety_checked(SQGrammar *, const void *, size_t, SQError *);

// Validates without copying the slab. Bytes must remain alive and immutable
// until this tree and its nodes/cursors are no longer used. They must be aligned
// to 8 bytes (64 with the experimental column-alignment build). Deletion frees
// only the runtime descriptor; the caller retains ownership of the bytes.
SQTree *sq_tree_from_bytes_borrowed(SQGrammar *, const void *, size_t, SQError *);
// Same lifetime/alignment contract, with the safety-checked validation policy.
SQTree *sq_tree_from_bytes_borrowed_safety_checked(SQGrammar *, const void *, size_t, SQError *);

uint32_t sq_tree_grammar_cache_size(const SQTree *);
bool sq_tree_copy_grammar_cache(const SQTree *, void *, size_t, SQError *);

// Returns an independent compact copy; nodes in the original remain valid.
SQTree *sq_tree_repack(const SQTree *, SQError *);
// Compact serialization without an intermediate slab allocation. Returns zero
// for a null tree. copy requires exactly compact_size bytes, disjoint from the
// source. Destination need not be initialized or aligned; success initializes
// every byte. The source and all its nodes remain unchanged.
uint32_t sq_tree_compact_size(const SQTree *);
bool sq_tree_copy_compact(const SQTree *, void *destination, size_t length, SQError *);
uint32_t sq_tree_group_count(const SQTree *);
uint32_t sq_tree_group_capacity(const SQTree *);
uint32_t sq_tree_slot_count(const SQTree *);
// False means point APIs expose byte offsets as columns on row zero.
bool sq_tree_has_points(const SQTree *);
SQNode sq_tree_root_node(const SQTree *);

// Invalid/wasted slots return null. Physical slots are stable across repacking.
SQNode sq_tree_node_at_slot(const SQTree *, uint32_t);

// Exact equality on encoded values: coordinates are deltas, and symbol IDs
// have builtin errors remapped after the grammar range. Display IDs are public;
// grammar IDs retain their original values. One bit per physical lane; trailing
// waste never matches.
uint64_t sq_tree_group_span_delta_equal(const SQTree *, uint32_t group, uint32_t value);
uint64_t sq_tree_group_start_byte_delta_equal(const SQTree *, uint32_t group, uint32_t value);
uint64_t sq_tree_group_end_byte_delta_equal(const SQTree *, uint32_t group, uint32_t value);
// Point keys store the row delta in the high byte and column delta in the low byte.
// Point-free trees have no encoded point lanes, so these functions return zero.
uint64_t sq_tree_group_start_point_equal(const SQTree *, uint32_t group, uint32_t value);
uint64_t sq_tree_group_end_point_equal(const SQTree *, uint32_t group, uint32_t value);

uint64_t sq_tree_group_supertype_equal(const SQTree *, uint32_t group, uint32_t value);
uint64_t sq_tree_group_symbol_equal(const SQTree *, uint32_t group, uint32_t value);
uint64_t sq_tree_group_grammar_symbol_equal(const SQTree *, uint32_t group, uint32_t value);
uint64_t sq_tree_group_field_equal(const SQTree *, uint32_t group, uint32_t value);

// False positives are possible for common symbols, never false negatives.
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
// Reports whether any node in this physical block has an error in its subtree.
// May return true for an error-free node sharing a block with an erroneous node.
bool sq_node_has_error(SQNode);
bool sq_node_has_changes(SQNode);
bool sq_node_has_supertype(SQNode, TSSymbol);
TSFieldId sq_node_field_id(SQNode);
const char *sq_node_field_name(SQNode);

// First physical slot outside this subtree; may equal the tree's slot count.
uint32_t sq_node_end_slot(SQNode);
uint32_t sq_node_descendant_count(SQNode);
uint32_t sq_node_child_count(SQNode);
uint32_t sq_node_named_child_count(SQNode);
SQNode sq_node_parent(SQNode);
SQNode sq_node_child(SQNode, uint32_t);
SQNode sq_node_named_child(SQNode, uint32_t);
SQNode sq_node_next_sibling(SQNode);

// Structural iteration includes empty siblings that mainline's node accessor
// skips. Child enumeration and cursors use this form.
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
// Preorder traversal stays within this node's tree, and returns null at ends.
SQNode sq_node_next_preorder(SQNode);
SQNode sq_node_prev_preorder(SQNode);

SQCursor *sq_cursor_new(SQNode);
void sq_cursor_delete(SQCursor *);
SQNode sq_cursor_node(const SQCursor *);
SQNode sq_cursor_parent_node(const SQCursor *);
uint32_t sq_cursor_depth(const SQCursor *);
// Retains allocated ancestor storage; node becomes the cursor's root.
void sq_cursor_reset(SQCursor *, SQNode);
bool sq_cursor_goto_first_child(SQCursor *);
bool sq_cursor_goto_last_child(SQCursor *);
bool sq_cursor_goto_next_sibling(SQCursor *);
// Previous sibling and child seeking can scan siblings. Failed moves preserve position.
bool sq_cursor_goto_previous_sibling(SQCursor *);
int64_t sq_cursor_goto_first_child_for_byte(SQCursor *, uint32_t);
int64_t sq_cursor_goto_first_child_for_point(SQCursor *, TSPoint);
bool sq_cursor_goto_parent(SQCursor *);

// Constant-time snapshots; null nodes/cursors produce zeroed snapshots.
void sq_node_attributes(SQNode, SQCursorAttributes *);

// Trees must outlive cursors. Each cursor owns its ancestor stack.
void sq_cursor_attributes(SQCursor *, SQCursorAttributes *);

// Iterates root and its descendants in visible preorder, including empty nodes.
// The tree must outlive the iterator and returned SQNodes.
// next returns null permanently after exhaustion. Attributes/field_id refer to
// the last returned node and are zero before iteration and after exhaustion.
SQNodeIterator *sq_node_iterator_new(SQNode root);
void sq_node_iterator_delete(SQNodeIterator *);
SQNode sq_node_iterator_next(SQNodeIterator *);
SQNode sq_node_iterator_node(const SQNodeIterator *);
void sq_node_iterator_attributes(SQNodeIterator *, SQCursorAttributes *);
TSFieldId sq_node_iterator_field_id(SQNodeIterator *);
TSSymbol sq_node_iterator_symbol(SQNodeIterator *);
void sq_node_iterator_byte_range(SQNodeIterator *, uint32_t *start, uint32_t *end);

#ifdef __cplusplus
}
#endif
#endif
