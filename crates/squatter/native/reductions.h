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

typedef struct SQParser SQParser;

// inline diagnostic storage so messages survive the parser call without a borrow
typedef struct {
  SQError code;
  uint32_t byte;
  TSPoint point;
  char message[512];
} SQParseError;

SQParser *sq_native_parser_new(SQGrammar *, SQParseError *);
void sq_native_parser_delete(SQParser *);
void sq_native_parser_drop_scratch(SQParser *);
const SQReduction *sq_native_parser_reductions(const SQParser *, uint32_t *, uint32_t *);

#endif
