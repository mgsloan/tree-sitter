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

SQTree *sq_pack_reductions(SQPackContext *, SQGrammar *,
                           const SQReduction *, uint32_t count, uint32_t root,
                           SQPackOptions, SQError *);

#endif
