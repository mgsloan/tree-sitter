#include "internal.h"

typedef struct {
  uint32_t magic, supertype_count, count, words;
} GrammarCacheHeader;

#define GRAMMAR_CACHE_MAGIC UINT32_C(0x53514701)
static bool reserve(void **data, uint32_t *capacity, uint32_t count, size_t size) {
  if (count <= *capacity) return true;
  uint32_t next = *capacity ? *capacity : 16;
  while (next < count) {
    if (next > UINT32_MAX / 2) { next = count; break; }
    next *= 2;
  }
  if (size && next > SIZE_MAX / size) return false;
  void *p = realloc(*data, (size_t)next * size);
  if (!p) return false;
  *data = p;
  *capacity = next;
  return true;
}

static uint64_t mask_hash(const uint64_t *mask, uint32_t words) {
  uint64_t hash = UINT64_C(14695981039346656037);
  for (uint32_t i = 0; i < words; i++) {
    hash ^= mask[i];
    hash *= UINT64_C(1099511628211);
    hash ^= hash >> 32;
  }
  return hash;
}

uint32_t sq_supertype_mask_id(const SQSupertypeGrammar *g, const uint64_t *mask) {
  uint32_t bucket = (uint32_t)mask_hash(mask, g->words) & (g->table_capacity - 1);
  while (g->table[bucket]) {
    uint32_t id = g->table[bucket] - 1;
    if (!memcmp(g->masks + (size_t)id * g->words, mask, (size_t)g->words * 8)) return id;
    bucket = (bucket + 1) & (g->table_capacity - 1);
  }
  return SQ_NONE;
}

static bool rehash(SQSupertypeGrammar *g, uint32_t capacity) {
  uint32_t *table = calloc(capacity, sizeof(uint32_t));
  if (!table) return false;
  for (uint32_t id = 0; id < g->count; id++) {
    uint32_t bucket = (uint32_t)mask_hash(g->masks + (size_t)id * g->words, g->words) & (capacity - 1);
    while (table[bucket]) bucket = (bucket + 1) & (capacity - 1);
    table[bucket] = id + 1;
  }
  free(g->table);
  g->table = table;
  g->table_capacity = capacity;
  return true;
}

size_t sq_supertype_grammar_cache_size(const SQSupertypeGrammar *g) {
  if (!g || g->count > (SIZE_MAX - sizeof(GrammarCacheHeader)) / ((size_t)g->words * 8)) return 0;
  return sizeof(GrammarCacheHeader) + (size_t)g->count * g->words * 8;
}

bool sq_supertype_grammar_copy_cache(const SQSupertypeGrammar *g, void *destination,
                                     size_t length, SQError *error) {
  sq_fail(error, SQ_OK);
  size_t expected = sq_supertype_grammar_cache_size(g);
  if (!g || !destination || !expected || length != expected) {
    sq_fail(error, SQ_ERROR_ARGUMENT);
    return false;
  }
  GrammarCacheHeader header = {GRAMMAR_CACHE_MAGIC, g->supertype_count, g->count, g->words};
  sq_set_u32(destination, 0, 0, header.magic);
  sq_set_u32(destination, 0, 1, header.supertype_count);
  sq_set_u32(destination, 0, 2, header.count);
  sq_set_u32(destination, 0, 3, header.words);
  uint8_t *masks = (uint8_t *)destination + sizeof(header);
  for (size_t index = 0; index < (size_t)g->count * g->words; index++) {
    sq_set_u64(masks + index * 8, 0, 0, g->masks[index]);
  }
  return true;
}

static uint32_t add_mask(SQSupertypeGrammar *g, uint32_t *capacity,
                         const uint64_t *mask, SQError *error) {
  uint32_t id = sq_supertype_mask_id(g, mask);
  if (id != SQ_NONE) return id;
  if (g->count == 65536) { sq_fail(error, SQ_ERROR_DICTIONARY_FULL); return SQ_NONE; }
  if (!reserve((void **)&g->masks, capacity, g->count + 1, (size_t)g->words * 8) ||
      (g->count * 2 >= g->table_capacity && !rehash(g, g->table_capacity * 2))) {
    sq_fail(error, SQ_ERROR_ALLOCATION);
    return SQ_NONE;
  }
  id = g->count++;
  memcpy(g->masks + (size_t)id * g->words, mask, (size_t)g->words * 8);
  uint32_t bucket = (uint32_t)mask_hash(mask, g->words) & (g->table_capacity - 1);
  while (g->table[bucket]) bucket = (bucket + 1) & (g->table_capacity - 1);
  g->table[bucket] = id + 1;
  return id;
}

typedef struct { uint32_t from, symbol, next; } Predecessor;
typedef struct { uint32_t state, symbol, count, production; } Reduction;
typedef struct { uint32_t child, next; } Edge;
typedef struct { uint32_t mask, last; } Walk;
typedef struct { const uint64_t *mask; uint32_t words; } SortMask;

static bool schedule_walk(Walk **walks, uint32_t *count, uint32_t *capacity,
                           uint64_t **visited, uint32_t *visited_capacity,
                           uint32_t words, uint32_t mask, uint32_t last) {
  uint32_t old_capacity = *visited_capacity;
  if (!reserve((void **)visited, visited_capacity, mask + 1, (size_t)words * 8)) return false;
  memset(*visited + (size_t)old_capacity * words, 0,
         (size_t)(*visited_capacity - old_capacity) * words * 8);
  uint64_t *word = *visited + (size_t)mask * words + last / 64;
  uint64_t bit = UINT64_C(1) << (last % 64);
  if (*word & bit) return true;
  if (*count == UINT32_MAX || !reserve((void **)walks, capacity, *count + 1, sizeof(Walk))) return false;
  *word |= bit;
  (*walks)[(*count)++] = (Walk){mask, last};
  return true;
}

static int compare_reductions(const void *a, const void *b) {
  const Reduction *x = a, *y = b;
#define CMP(field) if (x->field != y->field) return x->field < y->field ? -1 : 1
  CMP(symbol); CMP(state); CMP(count); CMP(production);
#undef CMP
  return 0;
}
static int compare_masks(const void *a, const void *b) {
  const SortMask *x = a, *y = b;
  for (uint32_t i = x->words; i-- > 0;) {
    if (x->mask[i] != y->mask[i]) return x->mask[i] < y->mask[i] ? -1 : 1;
  }
  return 0;
}

static bool edge_add(Edge **edges, uint32_t *count, uint32_t *capacity,
                      uint32_t *heads, uint32_t parent, uint32_t child) {
  for (uint32_t e = heads[parent]; e != SQ_NONE; e = (*edges)[e].next) {
    if ((*edges)[e].child == child) return true;
  }
  if (*count == UINT32_MAX || !reserve((void **)edges, capacity, *count + 1, sizeof(Edge))) return false;
  (*edges)[*count] = (Edge){child, heads[parent]};
  heads[parent] = (*count)++;
  return true;
}

// Multi-child reductions need predecessor states across both visible and hidden
// children. Most supertype paths are unary and never need this larger graph.
static bool collect_predecessors(const TSLanguage *language, uint32_t *heads,
                                  Predecessor **pred, uint32_t *count, uint32_t *capacity) {
  memset(heads, 0xff, language->state_count * sizeof(uint32_t));
  for (uint32_t state = 0; state < language->state_count; state++) {
    LookaheadIterator iter = ts_language_lookaheads(language, (TSStateId)state);
    while (ts_lookahead_iterator__next(&iter)) {
      for (uint32_t i = 0; i < (iter.action_count ? iter.action_count : 1u); i++) {
        uint32_t target;
        if (iter.action_count) {
          const TSParseAction *action = &iter.actions[i];
          if (action->type != TSParseActionTypeShift || action->shift.extra ||
              action->shift.repetition) continue;
          target = action->shift.state;
        } else {
          target = iter.next_state;
          if (!target) continue;
        }
        if (*count == UINT32_MAX ||
            !reserve((void **)pred, capacity, *count + 1, sizeof(Predecessor))) return false;
        (*pred)[*count] = (Predecessor){state, iter.symbol, heads[target]};
        heads[target] = (*count)++;
      }
    }
  }
  return true;
}

// Visible symbols normally stop inheritance. Hidden symbols and supertype
// aliases are candidates; only candidates reached from a root need expansion.
enum {
  DEFINITION_IGNORED,
  DEFINITION_CANDIDATE,
  DEFINITION_QUEUED,
};

// Reconstruct a conservative child graph from all reduction endpoints and
// backwards paths of structural shifts/gotos. Merged LR states can admit extra
// paths; keeping them makes the result independent of runtime pruning/scanners.
// Aliased children are visible in the packer, so they stop inherited masks.
static bool build_dictionary(SQSupertypeGrammar *g, SQError *error) {
  const TSLanguage *language = g->language;
  uint32_t symbols = language->symbol_count + language->alias_count;
  uint32_t states = language->state_count;
  uint32_t *heads = NULL, *pred_heads = NULL, *supertype_indexes = NULL, *seen = NULL;
  uint32_t *front = NULL, *back = NULL, *queue = NULL, *super_heads = NULL;
  uint32_t *hidden_heads = NULL, *reduction_offsets = NULL;
  uint8_t *extras = NULL, *definitions = NULL;
  uint32_t *action_generation = NULL;
  Predecessor *pred = NULL;
  Reduction *reductions = NULL;
  Edge *edges = NULL, *super_edges = NULL, *hidden = NULL;
  Walk *walks = NULL;
  uint64_t *mask = NULL, *visited = NULL;
  SortMask *sorted = NULL;
  uint64_t *ordered = NULL;
  uint32_t pred_count = 0, pred_capacity = 0, reduction_count = 0, reduction_capacity = 0;
  uint32_t hidden_count = 0, hidden_capacity = 0;
  uint32_t edge_count = 0, edge_capacity = 0, super_count = 0, super_capacity = 0;
  uint32_t mask_capacity = 0, walk_count = 0, walk_capacity = 0, visited_capacity = 0;
  bool ok = false;
  if (!states || !language->parse_table || !language->parse_actions) {
    sq_fail(error, SQ_ERROR_LANGUAGE);
    return false;
  }
#define ALLOC(name, count, type) \
  do { name = calloc((count), sizeof(type)); if (!name) goto allocation; } while (0)
  ALLOC(heads, symbols, uint32_t);
  ALLOC(hidden_heads, states, uint32_t);
  ALLOC(reduction_offsets, symbols + 1, uint32_t);
  ALLOC(supertype_indexes, symbols, uint32_t);
  ALLOC(extras, symbols, uint8_t);
  ALLOC(definitions, symbols, uint8_t);
  // Parse-table action-list indices are uint16_t. State + 1 is a generation
  // stamp, avoiding a full clear of this table between states.
  ALLOC(action_generation, (size_t)UINT16_MAX + 1, uint32_t);
  ALLOC(seen, states > symbols ? states : symbols, uint32_t);

  ALLOC(queue, symbols, uint32_t);
  ALLOC(super_heads, g->supertype_count, uint32_t);
  ALLOC(mask, g->words, uint64_t);
  memset(heads, 0xff, symbols * sizeof(uint32_t));
  memset(hidden_heads, 0xff, states * sizeof(uint32_t));
  memset(super_heads, 0xff, g->supertype_count * sizeof(uint32_t));
  // One-based indices reserve zero for symbols without their own supertype bit.
  for (uint32_t s = 0, bit = 0; s < symbols; s++) {
    if (language->symbol_metadata[s].supertype) supertype_indexes[s] = ++bit;
  }
  uint32_t pending_count = 0;
  for (uint32_t raw = 0; raw < language->symbol_count; raw++) {
    bool root = supertype_indexes[raw] != 0;
    const TSSymbol *aliases, *end;
    ts_language_aliases_for_symbol(language, (TSSymbol)raw, &aliases, &end);
    for (; aliases < end; aliases++) root |= supertype_indexes[*aliases] != 0;
    definitions[raw] = !language->symbol_metadata[raw].visible || root
        ? DEFINITION_CANDIDATE : DEFINITION_IGNORED;
    if (root) {
      definitions[raw] = DEFINITION_QUEUED;
      queue[pending_count++] = raw;
    }
  }
  for (uint32_t state = 0; state < states; state++) {
    LookaheadIterator iter = ts_language_lookaheads(language, (TSStateId)state);
    while (ts_lookahead_iterator__next(&iter)) {
      uint32_t target = 0;
      if (iter.action_count) {
        bool hidden_token = !language->symbol_metadata[iter.symbol].visible;
        // Visible shifts cannot extend a supertype path. Shared action lists
        // need only one reduction visit per state, regardless of lookahead.
        if (!hidden_token && iter.action_count == 1 &&
            iter.actions[0].type == TSParseActionTypeShift) continue;
        bool duplicate = action_generation[iter.table_value] == state + 1;
        action_generation[iter.table_value] = state + 1;
        if (duplicate && !hidden_token && iter.symbol != ts_builtin_sym_end) continue;
        for (uint32_t i = 0; i < iter.action_count; i++) {
          const TSParseAction *action = &iter.actions[i];
          if (action->type == TSParseActionTypeReduce) {
            // Only a null lookahead at the end of a nonterminal extra marks
            // its reduction extra. A self-loop goto alone can be ordinary recursion.
            if (iter.symbol == ts_builtin_sym_end && language->lex_modes &&
                ts_language_lex_mode_for_state(language, (TSStateId)state).lex_state == UINT16_MAX) {
              extras[action->reduce.symbol] = true;
            }
            if (duplicate || definitions[action->reduce.symbol] == DEFINITION_IGNORED) continue;
            if (reduction_count == UINT32_MAX ||
                !reserve((void **)&reductions, &reduction_capacity, reduction_count + 1, sizeof(Reduction))) goto allocation;
            reductions[reduction_count++] = (Reduction){state, action->reduce.symbol,
                action->reduce.child_count, action->reduce.production_id};
          } else if (action->type == TSParseActionTypeShift) {
            if (action->shift.extra) extras[iter.symbol] = true;
            else if (!action->shift.repetition) {
              target = action->shift.state;
              if (target >= states) goto invalid;
              if (!language->symbol_metadata[iter.symbol].visible &&
                  !edge_add(&hidden, &hidden_count, &hidden_capacity, hidden_heads,
                            target, iter.symbol)) goto allocation;
            }
          }
        }
      } else if (iter.next_state) {
        target = iter.next_state;
        if (target >= states) goto invalid;
        if (!language->symbol_metadata[iter.symbol].visible &&
            !edge_add(&hidden, &hidden_count, &hidden_capacity, hidden_heads,
                      target, iter.symbol)) goto allocation;
      }
    }
  }
  if (reduction_count) qsort(reductions, reduction_count, sizeof(Reduction), compare_reductions);
  for (uint32_t r = 0; r < reduction_count; r++) reduction_offsets[reductions[r].symbol + 1]++;
  for (uint32_t raw = 0; raw < symbols; raw++) reduction_offsets[raw + 1] += reduction_offsets[raw];
  // An extra can start a hidden path from any supertype. Its definition must be
  // explored even when no ordinary production refers to it.
  for (uint32_t raw = 0; raw < language->symbol_count; raw++) {
    if (extras[raw] && !language->symbol_metadata[raw].visible && definitions[raw] != DEFINITION_QUEUED) {
      definitions[raw] = DEFINITION_QUEUED;
      queue[pending_count++] = raw;
    }
  }
  for (uint32_t pending = 0; pending < pending_count; pending++) {
    uint32_t parent = queue[pending];
    for (uint32_t r = reduction_offsets[parent]; r < reduction_offsets[parent + 1]; r++) {
      Reduction reduction = reductions[r];
      if (r && !compare_reductions(&reduction, &reductions[r - 1])) continue;
      if (reduction.count == 1) {
        if (language->max_alias_sequence_length &&
            ts_language_alias_at(language, reduction.production, 0)) continue;
        // A unary reduction's only child is its incoming structural transition.
        // Visible children terminate inheritance, so their transitions are irrelevant.
        for (uint32_t e = hidden_heads[reduction.state]; e != SQ_NONE; e = hidden[e].next) {
          if (!edge_add(&edges, &edge_count, &edge_capacity, heads, parent, hidden[e].child)) goto allocation;
        }
      } else if (reduction.count > 1) {
        if (!pred_heads) {
          ALLOC(pred_heads, states, uint32_t);
          ALLOC(front, states, uint32_t);
          ALLOC(back, states, uint32_t);
          if (!collect_predecessors(language, pred_heads, &pred, &pred_count, &pred_capacity)) goto allocation;
        }
        front[0] = reduction.state;
        uint32_t front_count = 1;
        for (uint32_t position = reduction.count; position-- > 0 && front_count;) {
          uint32_t back_count = 0;
          memset(seen, 0, states * sizeof(uint32_t));
          TSSymbol alias = position < language->max_alias_sequence_length
              ? ts_language_alias_at(language, reduction.production, position) : 0;
          for (uint32_t f = 0; f < front_count; f++) {
            for (uint32_t p = pred_heads[front[f]]; p != SQ_NONE; p = pred[p].next) {
              Predecessor predecessor = pred[p];
              if (!alias && !language->symbol_metadata[predecessor.symbol].visible &&
                  !edge_add(&edges, &edge_count, &edge_capacity, heads, parent, predecessor.symbol)) goto allocation;
              if (!seen[predecessor.from]) {
                seen[predecessor.from] = 1;
                back[back_count++] = predecessor.from;
              }
            }
          }
          uint32_t *swap = front; front = back; back = swap;
          front_count = back_count;
        }
      }
    }
    for (uint32_t e = heads[parent]; e != SQ_NONE; e = edges[e].next) {
      uint32_t child = edges[e].child;
      if (definitions[child] != DEFINITION_QUEUED) {
        definitions[child] = DEFINITION_QUEUED;
        queue[pending_count++] = child;
      }
    }
  }
  // Extras may occur inside any production. Only hidden ones can extend a
  // mask; visible ERROR/extra nodes reset it. Hidden _ERROR contributes no bit
  // and recovery wraps discarded subtrees in visible ERROR, so seed every
  // supertype independently below (including detached recovery roots).
  for (uint32_t extra = 0; extra < symbols; extra++) {
    if (extras[extra] && !language->symbol_metadata[extra].visible) {
      for (uint32_t parent = 0; parent < language->symbol_count; parent++) {
        if (!edge_add(&edges, &edge_count, &edge_capacity, heads, parent, extra)) goto allocation;
      }
    }
  }
  // Collapse hidden non-supertype wrappers. An alias is always visible, but
  // if it is itself a supertype it starts its own bit on the raw node's children.
  for (uint32_t raw = 0; raw < language->symbol_count; raw++) {
    const TSSymbol *aliases, *aliases_end;
    ts_language_aliases_for_symbol(language, (TSSymbol)raw, &aliases, &aliases_end);
    for (uint32_t variant = 0; variant <= (uint32_t)(aliases_end - aliases); variant++) {
      uint32_t effective = variant ? aliases[variant - 1] : raw;
      if (!supertype_indexes[effective]) continue;
      uint32_t source = supertype_indexes[effective] - 1;
      memset(seen, 0, symbols * sizeof(uint32_t));
      uint32_t queue_count = 1;
      queue[0] = raw;
      seen[raw] = 1;
      for (uint32_t q = 0; q < queue_count; q++) {
        for (uint32_t e = heads[queue[q]]; e != SQ_NONE; e = edges[e].next) {
          uint32_t child = edges[e].child;
          if (supertype_indexes[child]) {
            if (!edge_add(&super_edges, &super_count, &super_capacity, super_heads,
                          source, supertype_indexes[child] - 1)) goto allocation;
          } else if (!seen[child]) {
            seen[child] = 1;
            queue[queue_count++] = child;
          }
        }
      }
    }
  }
  if (!rehash(g, 32)) goto allocation;
  if (add_mask(g, &mask_capacity, mask, error) == SQ_NONE) goto cleanup;
  // Track (mask, last supertype), not just masks: the same set can end at
  // different symbols, which have different outgoing nesting relationships.
  for (uint32_t bit = 0; bit < g->supertype_count; bit++) {
    memset(mask, 0, (size_t)g->words * 8);
    mask[bit / 64] = UINT64_C(1) << (bit % 64);
    uint32_t id = add_mask(g, &mask_capacity, mask, error);
    if (id == SQ_NONE) goto cleanup;
    if (!schedule_walk(&walks, &walk_count, &walk_capacity, &visited, &visited_capacity,
                       g->words, id, bit)) goto allocation;
  }
  for (uint32_t w = 0; w < walk_count; w++) {
    Walk walk = walks[w];
    for (uint32_t e = super_heads[walk.last]; e != SQ_NONE; e = super_edges[e].next) {
      uint32_t child = super_edges[e].child;
      memcpy(mask, g->masks + (size_t)walk.mask * g->words, (size_t)g->words * 8);
      mask[child / 64] |= UINT64_C(1) << (child % 64);
      uint32_t id = add_mask(g, &mask_capacity, mask, error);
      if (id == SQ_NONE) goto cleanup;
      if (!schedule_walk(&walks, &walk_count, &walk_capacity, &visited, &visited_capacity,
                         g->words, id, child)) goto allocation;
    }
  }
  ALLOC(sorted, g->count, SortMask);
  ALLOC(ordered, (size_t)g->count * g->words, uint64_t);
  for (uint32_t i = 0; i < g->count; i++) sorted[i] = (SortMask){g->masks + (size_t)i * g->words, g->words};
  qsort(sorted, g->count, sizeof(SortMask), compare_masks);
  for (uint32_t i = 0; i < g->count; i++) memcpy(ordered + (size_t)i * g->words, sorted[i].mask, (size_t)g->words * 8);
  free(g->masks);
  g->masks = ordered;
  ordered = NULL;
  if (!rehash(g, g->table_capacity)) goto allocation;
  ok = true;
  goto cleanup;
invalid:
  sq_fail(error, SQ_ERROR_LANGUAGE);
  goto cleanup;
allocation:
  sq_fail(error, SQ_ERROR_ALLOCATION);
cleanup:
  free(heads); free(pred_heads); free(supertype_indexes); free(seen);
  free(front); free(back); free(queue); free(super_heads); free(extras); free(definitions); free(action_generation);
  free(hidden_heads); free(hidden); free(reduction_offsets);
  free(pred); free(reductions); free(edges); free(super_edges);
  free(walks); free(mask); free(visited); free(sorted); free(ordered);
  return ok;
#undef ALLOC
}

void sq_supertype_grammar_delete(SQSupertypeGrammar *g) {
  if (!g) return;
  ts_language_delete(g->language);
  free(g->masks);
  free(g->table);
  free(g);
}

static int compare_mask_values(const uint64_t *left, const uint64_t *right, uint32_t words) {
  for (uint32_t word = words; word-- > 0;) {
    if (left[word] != right[word]) return left[word] < right[word] ? -1 : 1;
  }
  return 0;
}

SQSupertypeGrammar *sq_supertype_grammar_new_cached(const TSLanguage *language,
                                                    uint32_t supertype_count,
                                                    const void *data, size_t length,
                                                    SQError *error) {
  GrammarCacheHeader header;
  if (!data || length < sizeof(header)) goto invalid;
  header = (GrammarCacheHeader){sq_get_u32(data, 0, 0), sq_get_u32(data, 0, 1),
                                sq_get_u32(data, 0, 2), sq_get_u32(data, 0, 3)};
  uint32_t words = (supertype_count + 63) / 64;
  if (header.magic != GRAMMAR_CACHE_MAGIC || header.supertype_count != supertype_count ||
      header.words != words || !header.count || header.count > 65536 ||
      header.count > (SIZE_MAX - sizeof(header)) / ((size_t)words * 8) ||
      length != sizeof(header) + (size_t)header.count * words * 8) goto invalid;

  SQSupertypeGrammar *g = calloc(1, sizeof(*g));
  if (!g) goto allocation;
  g->language = ts_language_copy(language);
  g->supertype_count = supertype_count;
  g->words = words;
  g->count = header.count;
  g->masks = malloc((size_t)g->count * words * 8);
  if (!g->masks) {
    sq_supertype_grammar_delete(g);
    goto allocation;
  }
  const uint8_t *masks = (const uint8_t *)data + sizeof(header);
  for (size_t index = 0; index < (size_t)g->count * words; index++) {
    g->masks[index] = sq_get_u64(masks + index * 8, 0, 0);
  }
  uint64_t high_mask = supertype_count % 64
                           ? (UINT64_C(1) << (supertype_count % 64)) - 1
                           : UINT64_MAX;
  for (uint32_t id = 0; id < g->count; id++) {
    const uint64_t *mask = g->masks + (size_t)id * words;
    if ((mask[words - 1] & ~high_mask) ||
        (id && compare_mask_values(mask - words, mask, words) >= 0)) {
      sq_supertype_grammar_delete(g);
      goto invalid;
    }
  }
  uint32_t capacity = 32;
  while (capacity < g->count * 2 && capacity < (1u << 31)) capacity *= 2;
  if (!rehash(g, capacity)) {
    sq_supertype_grammar_delete(g);
    goto allocation;
  }
  return g;

invalid:
  sq_fail(error, SQ_ERROR_INVALID_SLAB);
  return NULL;
allocation:
  sq_fail(error, SQ_ERROR_ALLOCATION);
  return NULL;
}

SQSupertypeGrammar *sq_supertype_grammar_new(const TSLanguage *language,
                                             uint32_t supertype_count, SQError *error) {
  SQSupertypeGrammar *g = calloc(1, sizeof(*g));
  if (!g) { sq_fail(error, SQ_ERROR_ALLOCATION); return NULL; }
  g->language = ts_language_copy(language);
  g->supertype_count = supertype_count;
  g->words = (supertype_count + 63) / 64;
  if (!build_dictionary(g, error)) { sq_supertype_grammar_delete(g); return NULL; }
  return g;
}
