#ifndef TREE_SITTER_SQUAT_REDUCTIONS_H_
#define TREE_SITTER_SQUAT_REDUCTIONS_H_

// Links use SQ_NONE for absence and follow the encoder's sibling order.
// Children with no visible output are omitted. Aliases and direct fields are
// resolved on attachment; inherited fields and supertypes are resolved on descent.
typedef struct {
  uint32_t first_child, next_sibling;
  uint32_t start_byte, end_byte;
  TSPoint start_point, end_point;
  TSSymbol symbol, alias;
  TSFieldId field;
  bool extra, visible;
  uint32_t visible_descendant_count;
} SQReduction;

typedef struct SQTraversal SQTraversal;
typedef struct SQParser SQParser;

typedef struct {
  uint32_t depth, start_byte, end_byte;
  TSPoint start_point, end_point;
  uint16_t symbol, grammar, field, supertype, flags;
} SQEvent;

SQTraversal *sq_native_traversal_new(void);
void sq_native_traversal_delete(SQTraversal *);
void sq_native_traversal_trim(SQTraversal *);
void sq_native_traversal_end(SQTraversal *);
uint32_t sq_native_traversal_node_count(const SQTraversal *);
bool sq_native_traversal_begin_tree(SQTraversal *, SQGrammar *, const TSTree *, bool, SQError *);
bool sq_native_traversal_begin_reductions(SQTraversal *, SQGrammar *, const SQReduction *, uint32_t,
                                          uint32_t, bool, SQError *);
bool sq_native_traversal_fill(SQTraversal *, SQEvent *, uint32_t, uint32_t *, bool *, SQError *);

typedef struct {
  SQError code;
  uint32_t byte;
  TSPoint point;
  char message[512];
} SQParseError;

SQParser *sq_native_parser_new(SQGrammar *, SQParseError *);
void sq_native_parser_delete(SQParser *);
void sq_native_parser_trim(SQParser *);
#endif
