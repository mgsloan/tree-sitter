#include "internal.h"

// Pairs pack the display ID above the original ID, so numeric order groups variants.
static int compare_pair(const void *left, const void *right) {
  uint32_t first = *(const uint32_t *)left, second = *(const uint32_t *)right;
  return (first > second) - (first < second);
}

// incoming structural parse-table transition, linked from its destination state
typedef struct {
  uint32_t from, symbol, next;
} SymbolPredecessor;

// reduction endpoint whose production assigns at least one child alias
typedef struct {
  uint32_t state, count, production;
} AliasReduction;

// Append to a temporary array; realloc failure leaves the original allocation owned.
static bool append(void **data, size_t *length, size_t *capacity, size_t size, const void *value) {
  if (*length == *capacity) {
    size_t next = *capacity ? *capacity * 2 : 32;
    if (next < *capacity || next > SIZE_MAX / size) return false;

    void *grown = realloc(*data, next * size);
    if (!grown) return false;

    *data = grown;
    *capacity = next;
  }

  memcpy((uint8_t *)*data + (*length)++ * size, value, size);
  return true;
}

// The runtime alias map omits terminals. Walk structural transitions backwards
// from aliased reductions; merged LR states can add pairs but cannot omit them.
static bool terminal_alias_pairs(const TSLanguage *language, uint32_t **pairs, size_t *length,
                                 size_t *capacity) {
  if (!language->max_alias_sequence_length) return true;

  uint32_t states = language->state_count;
  uint32_t *heads = malloc((size_t)states * sizeof(uint32_t));
  uint32_t *front = malloc((size_t)states * sizeof(uint32_t));
  uint32_t *back = malloc((size_t)states * sizeof(uint32_t));
  bool *seen = calloc(states, sizeof(bool));

  SymbolPredecessor *predecessors = NULL;
  AliasReduction *reductions = NULL;
  size_t predecessor_count = 0, predecessor_capacity = 0;
  size_t reduction_count = 0, reduction_capacity = 0;
  bool ok = false;

  if (!heads || !front || !back || !seen) goto done;
  memset(heads, 0xff, (size_t)states * sizeof(uint32_t));

  // Collect reverse transitions and one copy of each aliased reduction per state.
  for (uint32_t state = 0; state < states; state++) {
    LookaheadIterator iterator = ts_language_lookaheads(language, state);
    size_t first_reduction = reduction_count;
    while (ts_lookahead_iterator__next(&iterator)) {
      for (uint32_t index = 0; index < (iterator.action_count ? iterator.action_count : 1u);
           index++) {
        uint32_t target = iterator.next_state;
        if (iterator.action_count) {
          const TSParseAction *action = &iterator.actions[index];
          if (action->type == TSParseActionTypeReduce) {
            uint32_t production = action->reduce.production_id;
            uint32_t count = action->reduce.child_count;
            bool aliased = false, duplicate = false;
            for (uint32_t position = 0;
                 position < count && position < language->max_alias_sequence_length; position++) {
              aliased |= ts_language_alias_at(language, production, position) != 0;
            }
            for (size_t previous = first_reduction; previous < reduction_count; previous++) {
              duplicate |= reductions[previous].production == production &&
                           reductions[previous].count == count;
            }

            AliasReduction reduction = {state, count, production};
            if (aliased && !duplicate &&
                !append((void **)&reductions, &reduction_count, &reduction_capacity,
                        sizeof(reduction), &reduction))
              goto done;
            continue;
          }

          if (action->type != TSParseActionTypeShift || action->shift.extra ||
              action->shift.repetition)
            continue;
          target = action->shift.state;
        }

        if (!target) continue;
        if (target >= states || predecessor_count >= UINT32_MAX) goto done;

        SymbolPredecessor predecessor = {state, iterator.symbol, heads[target]};
        if (!append((void **)&predecessors, &predecessor_count, &predecessor_capacity,
                    sizeof(predecessor), &predecessor))
          goto done;
        heads[target] = (uint32_t)predecessor_count - 1;
      }
    }
  }

  // Each backward step corresponds to one structural child in the production.
  for (size_t index = 0; index < reduction_count; index++) {
    AliasReduction reduction = reductions[index];
    front[0] = reduction.state;
    uint32_t front_count = 1;
    for (uint32_t position = reduction.count; position-- > 0 && front_count;) {
      uint32_t back_count = 0;
      memset(seen, 0, states);
      TSSymbol alias = position < language->max_alias_sequence_length
                           ? ts_language_alias_at(language, reduction.production, position)
                           : 0;
      for (uint32_t item = 0; item < front_count; item++) {
        for (uint32_t edge = heads[front[item]]; edge != SQ_NONE; edge = predecessors[edge].next) {
          SymbolPredecessor predecessor = predecessors[edge];
          if (alias && predecessor.symbol < language->token_count) {
            uint32_t pair =
                ((uint32_t)language->public_symbol_map[alias] << 16) | predecessor.symbol;
            if (!append((void **)pairs, length, capacity, sizeof(pair), &pair)) goto done;
          }

          if (!seen[predecessor.from]) {
            seen[predecessor.from] = true;
            back[back_count++] = predecessor.from;
          }
        }
      }

      uint32_t *swap = front;
      front = back;
      back = swap;
      front_count = back_count;
    }
  }

  ok = true;

done:
  free(heads);
  free(front);
  free(back);
  free(seen);
  free(predecessors);
  free(reductions);
  return ok;
}

// Choose byte, local-variant, or global-selector codes; fall back to a separate
// original-ID column when display and variant bits cannot share a u16.
bool sq_native_symbol_table_init(const TSLanguage *language, SQSymbolTable *table, SQError *error) {
  uint32_t symbols = language->symbol_count + language->alias_count + 2;

  // Both IDs fit literally, so aliases need no dictionary or grammar analysis.
  if (symbols <= 256) {
    table->encoding = SQ_SYMBOL_BYTES;
    table->shift = 8;
    table->default_codes = malloc((size_t)symbols * sizeof(uint16_t));
    if (!table->default_codes) goto allocation;

    for (uint32_t original = 0; original < symbols; original++) {
      uint32_t display = original < symbols - 2 ? language->public_symbol_map[original] : original;
      table->default_codes[original] = (display << 8) | original;
    }

    return true;
  }

  // Include natural displays and aliases before counting variants per display.
  size_t capacity = symbols;
  if (language->alias_map) {
    for (const TSSymbol *entry = language->alias_map; *entry;) {
      uint32_t count = entry[1];
      if (capacity > SIZE_MAX / sizeof(uint32_t) - count) goto allocation;
      capacity += count;
      entry += count + 2;
    }
  }
  uint32_t *pairs = malloc(capacity * sizeof(uint32_t));
  if (!pairs) goto allocation;

  size_t length = 0;
  for (uint32_t original = 0; original < symbols; original++) {
    uint32_t display = original < symbols - 2 ? language->public_symbol_map[original] : original;
    pairs[length++] = (display << 16) | original;
  }

  if (language->alias_map) {
    for (const TSSymbol *entry = language->alias_map; *entry;) {
      uint32_t original = *entry++, count = *entry++;
      for (uint32_t index = 0; index < count; index++) {
        pairs[length++] = ((uint32_t)language->public_symbol_map[*entry++] << 16) | original;
      }
    }
  }

  if (!terminal_alias_pairs(language, &pairs, &length, &capacity)) goto failure;
  qsort(pairs, length, sizeof(uint32_t), compare_pair);

  size_t unique = 0;
  uint32_t maximum = 0, count = 0, previous = UINT32_MAX;
  for (size_t index = 0; index < length; index++) {
    uint32_t pair = pairs[index];
    if (unique && pairs[unique - 1] == pair) continue;

    uint32_t display = pair >> 16;
    count = display == previous ? count + 1 : 1;
    previous = display;
    if (count > maximum) maximum = count;
    pairs[unique++] = pair;
  }

  uint8_t shift = sq_native_width(maximum - 1);
  table->separate = ((uint64_t)symbols << shift) > UINT16_MAX + UINT64_C(1);
  table->shift = table->separate ? 0 : shift;
  table->default_codes = malloc((size_t)symbols * sizeof(uint16_t));
  if (!table->default_codes) goto failure;

  for (uint32_t original = 0; original < symbols; original++) {
    table->default_codes[original] =
        original < symbols - 2 ? language->public_symbol_map[original] : original;
  }

  // Local codes reserve enough variant bits for the most ambiguous display.
  if (!table->separate) {
    table->length = symbols << shift;
    table->grammar_ids = calloc(table->length, sizeof(uint16_t));
    table->counts = calloc(symbols, sizeof(uint16_t));
    if (!table->grammar_ids || !table->counts) goto failure;

    for (size_t index = 0; index < unique; index++) {
      uint32_t display = pairs[index] >> 16;
      uint16_t original = (uint16_t)pairs[index];
      uint32_t code = (display << shift) | table->counts[display]++;
      table->grammar_ids[code] = original;
      uint32_t natural = original < symbols - 2 ? language->public_symbol_map[original] : original;
      if (natural == display) table->default_codes[original] = (uint16_t)code;
    }
  }

  // A global selector shares originals across displays; zero denotes the unique
  // default. Prefer this smaller dictionary when its selectors still fit.
  if (!table->separate) {
    uint16_t *codes = calloc(symbols, sizeof(uint16_t));
    uint16_t *defaults = calloc(symbols, sizeof(uint16_t));
    if (!codes || !defaults) {
      free(codes);
      free(defaults);
      goto failure;
    }

    uint32_t ambiguous = 0;
    for (size_t index = 0; index < unique; index++) {
      uint32_t display = pairs[index] >> 16;
      uint16_t original = pairs[index];
      defaults[display] = original;
      if (table->counts[display] > 1 && !codes[original]) codes[original] = ++ambiguous;
    }

    uint8_t global_shift = sq_native_width(ambiguous);
    if (((uint64_t)symbols << global_shift) <= UINT16_MAX + UINT64_C(1)) {
      uint16_t *dictionary = calloc(ambiguous + 1, sizeof(uint16_t));
      if (!dictionary) {
        free(codes);
        free(defaults);
        goto failure;
      }

      for (uint32_t original = 0; original < symbols; original++) {
        if (codes[original]) dictionary[codes[original]] = original;
        uint32_t display =
            original < symbols - 2 ? language->public_symbol_map[original] : original;
        table->default_codes[original] =
            (display << global_shift) | (table->counts[display] == 1 ? 0 : codes[original]);
      }

      free(table->grammar_ids);
      table->grammar_ids = dictionary;
      table->length = ambiguous + 1;
      table->grammar_codes = codes;
      table->defaults = defaults;
      table->encoding = SQ_SYMBOL_GLOBAL;
      table->shift = global_shift;
    } else {
      free(codes);
      free(defaults);
    }
  }

  free(pairs);
  return true;

failure:
  free(pairs);
  sq_native_symbol_table_delete(table);

allocation:
  sq_native_fail(error, SQ_ERROR_ALLOCATION);
  return false;
}

// Release all dictionary modes and restore the zero-initialized state.
void sq_native_symbol_table_delete(SQSymbolTable *table) {
  free(table->grammar_ids);
  free(table->default_codes);
  free(table->counts);
  free(table->defaults);
  free(table->grammar_codes);

  memset(table, 0, sizeof(*table));
}

// Encode a prepared display/original pair. Separate mode returns only the display;
// an unrepresentable dictionary pair returns SQ_NONE.
uint32_t sq_native_symbol_code(const SQGrammar *grammar, uint32_t display, uint32_t original) {
  const SQSymbolTable *table = &grammar->symbols;
  if (table->separate) return display;
  if (table->encoding == SQ_SYMBOL_BYTES) return (display << 8) | original;

  if (table->encoding == SQ_SYMBOL_GLOBAL) {
    uint32_t variant = table->counts[display] == 1 ? 0 : table->grammar_codes[original];
    if ((!variant && table->counts[display] != 1) ||
        (!variant && table->defaults[display] != original))
      return SQ_NONE;
    return (display << table->shift) | variant;
  }

  uint32_t start = display << table->shift;
  for (uint32_t variant = 0; variant < table->counts[display]; variant++) {
    if (table->grammar_ids[start + variant] == original) return start + variant;
  }

  return SQ_NONE;
}
