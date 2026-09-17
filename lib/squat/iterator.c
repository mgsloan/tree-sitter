#include "attributes.h"

struct SQNodeIterator {
  const SQTree *tree;
  SQNode current;
  uint32_t next, end, group_start;
};

SQNodeIterator *sq_node_iterator_new(SQNode root) {
  if (!root.tree) {
    return NULL;
  }

  SQNodeIterator *iterator = calloc(1, sizeof(SQNodeIterator));
  if (!iterator) {
    return NULL;
  }

  iterator->tree = root.tree;
  iterator->next = root.slot;
  iterator->end = sq_node_first_slot(root);
  iterator->group_start = root.slot / SQ_GROUP_SIZE * SQ_GROUP_SIZE;

  return iterator;
}

void sq_node_iterator_delete(SQNodeIterator *iterator) {
  free(iterator);
}

SQNode sq_node_iterator_node(const SQNodeIterator *iterator) {
  return iterator ? iterator->current : sq_null();
}

SQNode sq_node_iterator_next(SQNodeIterator *iterator) {
  if (!iterator) {
    return sq_null();
  }

  uint32_t slot = iterator->next;
  if (slot == SQ_NONE || slot < iterator->end) {
    iterator->current = sq_null();
    iterator->next = SQ_NONE;
    return iterator->current;
  }

  iterator->next = slot - 1;
  if (slot == iterator->group_start && slot) {
    uint32_t group = slot / SQ_GROUP_SIZE - 1;
    iterator->next -= sq_group_waste(iterator->tree, group);
    iterator->group_start -= SQ_GROUP_SIZE;
  }

  iterator->current = (SQNode){iterator->tree, slot};
  return iterator->current;
}

TSFieldId sq_node_iterator_field_id(SQNodeIterator *iterator) {
  if (!iterator || !iterator->current.tree) return 0;
  return (TSFieldId)sq_node_field_value(iterator->current);
}

TSSymbol sq_node_iterator_symbol(SQNodeIterator *iterator) {
  if (!iterator || !iterator->current.tree) return 0;
  return sq_node_symbol(iterator->current);
}

void sq_node_iterator_byte_range(SQNodeIterator *iterator, uint32_t *start, uint32_t *end) {
  if (!start || !end) return;
  *start = *end = 0;
  if (!iterator || !iterator->current.tree) return;
  *start = sq_node_start_byte(iterator->current);
  *end = sq_node_end_byte(iterator->current);
}

void sq_node_iterator_attributes(SQNodeIterator *iterator, SQCursorAttributes *out) {
  if (!out) return;
  memset(out, 0, sizeof(*out));
  if (!iterator || !iterator->current.tree) return;
  SQNode node = iterator->current;
  sq_attributes_with_ids(node, sq_node_symbol_id(node), sq_node_grammar_id(node),
                         (TSFieldId)sq_node_field_value(node), out);
}
