// Included by query.c: plans use its compiler and output coordinator,
// while execution reads packed columns without maintaining a tree cursor.

static bool query_execution_local_alternative(const SQQuery *query, const PatternEntry *entry) {
  const QueryStep *step = &query->steps.contents[entry->step_index];
  const QueryPattern *pattern = &query->patterns.contents[entry->pattern_index];
  uint32_t end = pattern->steps.offset + pattern->steps.length - 1;
  if (step->depth || step->field || step->supertype_symbol || step->negated_field_list_id ||
      step->is_immediate || step->is_last_child || step->is_missing || step->is_dead_end ||
      step->is_pass_through || step->alternative_is_skip) {
    return false;
  }

  uint32_t next = entry->step_index + 1;
  for (uint32_t remaining = pattern->steps.length; remaining; remaining--) {
    if (next == end) {
      return true;
    }

    if (next >= end || !query->steps.contents[next].is_dead_end) {
      return false;
    }

    next = query->steps.contents[next].alternative_index;
  }

  return false;
}

static void sq_query__prepare_steps(SQQuery *query) {
  for (uint32_t index = 0; index < query->steps.size; index++) {
    const QueryStep *step = &query->steps.contents[index];
    query->needs_fields |= step->field != 0;
    query->needs_supertypes |= step->supertype_symbol != 0;
  }

  for (uint32_t pattern = 0; pattern < query->capture_quantifiers.size; pattern++) {
    const CaptureQuantifiers *captures = &query->capture_quantifiers.contents[pattern];
    for (uint32_t index = 0; index < captures->size; index++) {
      if (captures->contents[index] == TSQuantifierZeroOrMore ||
          captures->contents[index] == TSQuantifierOneOrMore) {
        query->has_repeated_captures = true;
        break;
      }
    }
  }

  for (uint32_t index = 0; index < query->pattern_map.size; index++) {
    const PatternEntry *entry = &query->pattern_map.contents[index];
    if (entry->is_rooted && query_execution_local_alternative(query, entry)) {
      query->steps.contents[entry->step_index].is_local = true;
    }
  }
}

// Required steps can reject a start even after captures. Capture iteration
// exposes provisional states, whose snapshot timing is not part of the contract.
static void sq_query__prepare_presence(SQQuery *query) {
  for (uint32_t index = 0; index < query->pattern_map.size; index++) {
    PatternEntry *entry = &query->pattern_map.contents[index];
    const QueryStep *root = &query->steps.contents[entry->step_index];
    if (!entry->is_rooted || root->depth || root->alternative_index != NONE) {
      continue;
    }

    const QueryPattern *pattern = &query->patterns.contents[entry->pattern_index];
    const QueryStep *required = NULL;
    for (uint32_t next = entry->step_index + 1;
         next < pattern->steps.offset + pattern->steps.length; next++) {
      const QueryStep *step = &query->steps.contents[next];
      if (!step->depth || step->depth == PATTERN_DONE_MARKER || step->is_dead_end ||
          step->is_pass_through || step->alternative_index != NONE || step->is_missing) {
        break;
      }

      if ((step->symbol && step->symbol != ts_builtin_sym_error) || step->field) {
        required = step;
      }
    }

    if (!required) {
      continue;
    }

    QueryPresenceRequirement requirement = {.field = required->field};
    if (required->symbol) {
      uint32_t symbols = query->language->symbol_count + query->language->alias_count;
      for (uint32_t raw = 0; raw < symbols; raw++) {
        if (query->language->public_symbol_map[raw] != required->symbol) {
          continue;
        }

        if (requirement.symbol_count == 8) {
          requirement.symbol_count = 0;
          break;
        }

        requirement.symbols[requirement.symbol_count++] = raw;
      }
    }

    if (!requirement.symbol_count && !requirement.field) {
      continue;
    }

    uint32_t requirement_index = 0;
    while (requirement_index < query->presence_requirements.size &&
           memcmp(&query->presence_requirements.contents[requirement_index], &requirement,
                  sizeof(requirement))) {
      requirement_index++;
    }

    if (requirement_index == UINT16_MAX) {
      continue;
    }

    if (requirement_index == query->presence_requirements.size) {
      array_push(&query->presence_requirements, requirement);
    }

    entry->presence_requirement = requirement_index + 1;
  }
}

// Packed groups are in reverse preorder. Reverse the hit bits once per
// group so ordered query intervals can retain their ascending scan logic.
static uint64_t query_order_mask(uint64_t bits) {
  bits =
      ((bits >> 1) & UINT64_C(0x5555555555555555)) | ((bits & UINT64_C(0x5555555555555555)) << 1);
  bits =
      ((bits >> 2) & UINT64_C(0x3333333333333333)) | ((bits & UINT64_C(0x3333333333333333)) << 2);
  bits =
      ((bits >> 4) & UINT64_C(0x0f0f0f0f0f0f0f0f)) | ((bits & UINT64_C(0x0f0f0f0f0f0f0f0f)) << 4);
  return __builtin_bswap64(bits) >> (64 - SQ_GROUP_SIZE);
}

static bool sq_query_cursor__presence_matches(SQQueryCursor *self, const PatternEntry *pattern,
                                              SQNode root) {
  if (!self->symbol_scan || self->root_has_error) {
    return true;
  }

  const QueryPresenceRequirement *requirement =
      &self->query->presence_requirements.contents[pattern->presence_requirement - 1];
  QueryPresenceCache *cache = &self->presence_cache.contents[pattern->presence_requirement - 1];
  if (cache->samples == 32) {
    if (cache->rejections < 8) {
      cache->cooldown = 128;
    }

    cache->samples = cache->rejections = 0;
  }

  if (cache->cooldown) {
    cache->cooldown--;
    QUERY_EXEC_COUNT(self, presence_bypassed, 1);
    return true;
  }

  QUERY_EXEC_COUNT(self, presence_checks, 1);
  cache->samples++;
  uint32_t begin = sq_next_position(root.tree, sq_node_position(root) + 1),
           limit = sq_node_end_slot(root);

  // Reuse only an established empty interval. An exhausted scan budget means
  // unknown, never absence, so the ordinary matcher retains all possible starts.
  if (begin >= cache->start && begin <= cache->next) {
    if (cache->next >= limit) {
      QUERY_EXEC_COUNT(self, presence_cached, 1);
      goto absent;
    }

    if (cache->found) {
      QUERY_EXEC_COUNT(self, presence_cached, 1);
      return true;
    }

    begin = cache->next;
  } else {
    cache->start = begin;
  }

  uint32_t scanned_end = begin + (limit - begin < 256 ? limit - begin : 256);
  for (uint32_t slot = begin; slot < scanned_end;) {
    uint32_t group = slot / SQ_GROUP_SIZE;
    uint32_t group_start = group * SQ_GROUP_SIZE;
    uint32_t group_end = group_start + SQ_GROUP_SIZE;
    uint32_t end = group_end < scanned_end ? group_end : scanned_end;
    uint64_t hits = UINT64_MAX;
    if (requirement->symbol_count) {
      hits = 0;
      for (uint32_t index = 0; index < requirement->symbol_count; index++) {
        hits |= query_order_mask(sq_tree_group_symbol_equal(
            root.tree, sq_position_group(root.tree, group), requirement->symbols[index]));
      }
    }

    if (requirement->field) {
      hits &= query_order_mask(sq_tree_group_field_equal(
          root.tree, sq_position_group(root.tree, group), requirement->field));
    }

    hits &= UINT64_MAX << (slot - group_start);
    if (end - group_start < 64) {
      hits &= (UINT64_C(1) << (end - group_start)) - 1;
    }

    QUERY_EXEC_COUNT(self, presence_words, 1);
    if (hits) {
      cache->next = group_start + query_ctz(hits);
      cache->found = true;
      return true;
    }

    slot = end;
  }

  cache->next = scanned_end;
  cache->found = false;
  if (scanned_end < limit) {
    QUERY_EXEC_COUNT(self, presence_unknown, 1);
    return true;
  }

absent:
  cache->rejections++;
  QUERY_EXEC_COUNT(self, presence_rejections, 1);
  return false;
}

static void sq_query__prepare_execution(SQQuery *query) {
  QueryExecutionPlan *plan = &query->execution_plan;
  plan->supported = false;
  plan->local_patterns = 0;
  array_clear(&plan->steps);
  array_clear(&plan->roots);
  if (query->patterns.size > 64) {
    goto unsupported;
  }

  array_grow_by(&plan->steps, query->steps.size);
  uint64_t patterns = 0;
  for (uint32_t index = 0; index < query->pattern_map.size; index++) {
    const PatternEntry *entry = &query->pattern_map.contents[index];
    const QueryPattern *pattern = &query->patterns.contents[entry->pattern_index];
    uint64_t bit = (uint64_t)1 << entry->pattern_index;
    if (!entry->is_rooted) {
      goto unsupported;
    }

    uint32_t end = pattern->steps.offset + pattern->steps.length - 1;
    plan->end_steps[entry->pattern_index] = end;
    if (query->steps.contents[entry->step_index].symbol &&
        query_execution_local_alternative(query, entry)) {
      if (patterns & bit) {
        if (!(plan->local_patterns & bit)) {
          goto unsupported;
        }

        const QueryStep *first = &query->steps.contents[plan->start_steps[entry->pattern_index]];
        const QueryStep *step = &query->steps.contents[entry->step_index];
        if (memcmp(first->capture_ids, step->capture_ids, sizeof(step->capture_ids))) {
          goto unsupported;
        }

        for (uint32_t previous = 0; previous < index; previous++) {
          const PatternEntry *other = &query->pattern_map.contents[previous];
          if (other->pattern_index == entry->pattern_index &&
              query->steps.contents[other->step_index].symbol == step->symbol) {
            goto unsupported;
          }
        }
      } else {
        plan->start_steps[entry->pattern_index] = entry->step_index;
      }

      patterns |= bit;
      plan->local_patterns |= bit;
      plan->steps.contents[entry->step_index] =
          (QueryExecutionStep){.relation = QueryExecutionRoot};
      continue;
    }

    if (entry->step_index != pattern->steps.offset || (patterns & bit)) {
      goto unsupported;
    }

    patterns |= bit;
    plan->start_steps[entry->pattern_index] = entry->step_index;
    if (end == pattern->steps.offset || query->steps.contents[end].depth != PATTERN_DONE_MARKER) {
      goto unsupported;
    }

    for (uint32_t step_index = pattern->steps.offset; step_index < end; step_index++) {
      const QueryStep *step = &query->steps.contents[step_index];
      bool root = step_index == pattern->steps.offset;
      if (step->alternative_index != NONE || step->supertype_symbol ||
          step->negated_field_list_id || step->is_pass_through || step->is_dead_end ||
          step->is_inside_alternation || step->is_missing || step->alternative_is_skip) {
        goto unsupported;
      }

      if (root) {
        if (step->depth || step->field || step->is_immediate || step->is_last_child) {
          goto unsupported;
        }
      } else {
        // Named anchors leave exactly one possible child at each step. No
        // branching or longest-match deduplication is needed for these plans.
        bool named = step->symbol ? ts_language_symbol_metadata(query->language, step->symbol).named
                                  : step->is_named;
        if (step->depth != 1 || !step->is_immediate || !named) {
          goto unsupported;
        }
      }

      plan->steps.contents[step_index] = (QueryExecutionStep){
          .relation = root                                      ? QueryExecutionRoot
                      : step_index == pattern->steps.offset + 1 ? QueryExecutionFirstNamedChild
                                                                : QueryExecutionNextNamedSibling,
          .symbol = step->symbol,
          .field = step->field,
          .last_named_child = step->is_last_child,
      };
    }
  }

  uint32_t symbol_count = query->language->symbol_count + query->language->alias_count;
  array_grow_by(&plan->roots, symbol_count + 2);
  memset(plan->roots.contents, 0, plan->roots.size * sizeof(uint64_t));
  for (uint32_t raw = 0; raw < symbol_count; raw++) {
    TSSymbol symbol = query->language->public_symbol_map[raw];
    bool named = ts_language_symbol_metadata(query->language, raw).named;
    for (uint32_t index = 0; index < query->pattern_map.size; index++) {
      const PatternEntry *entry = &query->pattern_map.contents[index];
      const QueryStep *step = &query->steps.contents[entry->step_index];
      if (step->symbol ? step->symbol == symbol : !step->is_named || named) {
        plan->roots.contents[raw] |= (uint64_t)1 << entry->pattern_index;
      }
    }
  }

  plan->supported = true;
  return;
unsupported:
  array_delete(&plan->steps);
  array_delete(&plan->roots);
}

static void sq_query_cursor__execution_start(SQQueryCursor *self, SQNode root) {
  if (!self->query || !root.tree || root.tree->language != self->query->language ||
      !self->query->execution_plan.supported || sq_node_has_error(root) ||
      self->max_start_depth != UINT32_MAX ||
      !sq_query__range_is_unrestricted(&self->included_range) ||
      !sq_query__range_is_unrestricted(&self->containing_range)) {
    return;
  }

  self->execution_root = root;
  self->execution_position = sq_node_position(root);
  self->execution_last_node = UINT32_MAX;
  self->execution_active = true;
  self->execution_stats.planned = true;
}

QueryExecutionStats sq_query_cursor__execution_stats(const SQQueryCursor *self) {
  return self->execution_stats;
}

static void sq_query_cursor__execution_fallback(SQQueryCursor *self) {
  if (!self->execution_active) {
    return;
  }

  self->execution_active = false;
  if (self->execution_last_node == UINT32_MAX) {
    self->execution_stats.planned = false;
  }

  if (self->execution_last_node == UINT32_MAX || self->halted) {
    return;
  }

  // Setters defer this until iteration resumes: the previous tree may have
  // been freed before a cursor is configured for its next exec.
  // Restore the last enter event
  // and each live state's depth, retaining captures, IDs, and consumption.
  query_tree_cursor_reset(&self->cursor, query_identity_node(self->execution_root));
  self->depth = 0;
  SQNode current = self->execution_root;
  for (;;) {
    QUERY_EXEC_COUNT(self, seek_restoration_steps, 1);
    for (uint32_t index = 0; index < self->states.size; index++) {
      QueryState *state = &self->states.contents[index];
      if (self->execution_states.contents[state->heap_insert_order].root ==
          sq_node_position(current)) {
        state->start_depth = self->depth;
      }
    }

    if (sq_node_position(current) == self->execution_last_node) {
      break;
    }

    uint32_t end = sq_node_end_slot(current);
    if (self->execution_last_node < end && query_tree_cursor_goto_first_child(&self->cursor)) {
      self->depth++;
    } else {
      while (!query_tree_cursor_goto_next_sibling(&self->cursor)) {
        bool ascended = query_tree_cursor_goto_parent(&self->cursor);
        ts_assert(ascended);
        self->depth--;
      }
    }

    current = query_tree_cursor_node(&self->cursor);
  }

  self->dirty_patterns = UINT64_MAX;
  self->states_need_sort = true;
  self->states_max_depth = UINT32_MAX;
  array_clear(&self->execution_states);
  self->execution_free_state = UINT32_MAX;
  self->ascending = !query_tree_cursor_goto_first_child(&self->cursor);
  if (!self->ascending) {
    self->depth++;
  }
}

static uint32_t query_execution_named_child(const SQTree *tree, const TSLanguage *language,
                                            uint32_t slot, uint32_t end) {
  (void)language;
  slot = sq_next_position(tree, slot);
  while (slot < end) {
    SQNode node = sq_position_node(tree, slot);
    if (sq_node_is_named(node)) {
      break;
    }

    slot = sq_node_end_slot(node);
  }

  return slot;
}

static void sq_query__prepare_symbol_filter(SQQuery *query) {
  QuerySymbolFilter *filter = &query->scan_filter;
  uint32_t count = query->scan_targets.size;

  // Bound compilation work for large sets that retain scalar membership tests.
  if (!count || count > 32) {
    return;
  }

  filter->symbol_count = count;
  memcpy(filter->symbols, query->scan_targets.contents, count * sizeof(uint16_t));
  uint32_t width = sq_symbol_width(query->language->symbol_count + query->language->alias_count + 1);
  if (width < 2) {
    width = 2;
  }

  struct {
    uint32_t value, mask;
  } matches[32];
  for (uint32_t index = 0; index < count; index++) {
    matches[index].value = query->scan_targets.contents[index];
    matches[index].mask = (UINT32_C(1) << width) - 1;
  }

  // Equal masks differing in one value bit describe two halves of an exact
  // larger set. Merging preserves disjointness and never admits another symbol.
  bool merged;
  do {
    merged = false;
    for (uint32_t index = 0; index < count && !merged; index++) {
      for (uint32_t other = index + 1; other < count; other++) {
        uint32_t difference = matches[index].value ^ matches[other].value;
        if (matches[index].mask != matches[other].mask || !difference ||
            (difference & (difference - 1))) {
          continue;
        }

        matches[index].mask &= ~difference;
        matches[index].value &= ~difference;
        memmove(matches + other, matches + other + 1, (--count - other) * sizeof(*matches));
        merged = true;
        break;
      }
    }
  } while (merged);
  if (count > 8) {
    return;
  }

  uint64_t least_bits = sq_lane_starts(width);
  filter->width = width;
  filter->count = count;
  filter->high_bits = least_bits << (width - 1);
  filter->low_bits = filter->high_bits - least_bits;
  for (uint32_t index = 0; index < count; index++) {
    filter->values[index] = matches[index].value * least_bits;
    filter->masks[index] = matches[index].mask * least_bits;
  }
}

static uint32_t query_execution_find_symbols(SQQueryCursor *cursor, const SQTree *tree,
                                             const QuerySymbolFilter *filter, uint32_t start,
                                             uint32_t end) {
  uint32_t width = tree->layout.symbol_bits, lanes = 64 / width;
  uint32_t slots = sq_tree_slot_count(tree);
  while (start < end) {
    if (sq_query_cursor__scan_cancelled(cursor, sq_position_node(tree, start))) {
      return end;
    }

    uint32_t group = start / SQ_GROUP_SIZE;
    uint32_t group_end = (group + 1) * SQ_GROUP_SIZE;
    if (group_end > end) {
      group_end = end;
    }

    // The persisted index is profitable for a small selective root set. Its
    // keys are public IDs, while lane filters compare raw display IDs.
    if (sq_presence_offset(tree) && filter->symbol_count <= 4) {
      bool interested = false;
      for (uint32_t index = 0; index < filter->symbol_count; index++) {
        TSSymbol symbol = sq_decode_symbol(tree, filter->symbols[index]);
        symbol = ts_language_public_symbol(tree->language, symbol);
        if (sq_tree_group_has_symbol(tree, sq_position_group(tree, group), symbol)) {
          interested = true;
          break;
        }
      }

      if (!interested) {
        start = sq_next_position(tree, group_end);
        continue;
      }
    }

    // Preorder advances toward lower physical lanes. Visit packed words and
    // their matching high bits from high to low to return the earliest hit.
    uint32_t low = slots - group_end, high = slots - start;
    uint32_t word_index = (high - 1) / lanes;
    for (;;) {
      uint64_t word = sq_get_u64(tree->data, tree->layout.symbol, word_index), hits = 0;
      for (uint32_t index = 0; index < filter->count; index++) {
        uint64_t difference = (word ^ filter->values[index]) & filter->masks[index];
        hits |= ~(((difference & filter->low_bits) + filter->low_bits) | difference) &
                filter->high_bits;
      }

      while (hits) {
        unsigned bit = 63u - (unsigned)__builtin_clzll(hits);
        uint32_t physical = word_index * lanes + bit / width;
        if (physical >= low && physical < high) return slots - 1 - physical;
        hits &= ~(UINT64_C(1) << bit);
      }

      if (word_index == low / lanes) break;
      word_index--;
    }

    start = sq_next_position(tree, group_end);
  }

  return end;
}

static uint32_t query_execution_find_root(SQQueryCursor *self, const SQTree *tree, uint32_t start,
                                          uint32_t end) {
  const SQQuery *query = self->query;
  start = sq_next_position(tree, start);
  while (start < end) {
    if (sq_query_cursor__scan_cancelled(self, sq_position_node(tree, start))) {
      return end;
    }

    if (query->scan_filter.count) {
      start = query_execution_find_symbols(self, tree, &query->scan_filter, start, end);
      if (start == end) {
        break;
      }
    }

    if (query->execution_plan.roots.contents[sq_node_symbol_id(sq_position_node(tree, start))]) {
      return start;
    }

    start = sq_next_position(tree, start + 1);
  }

  return end;
}

// heap_insert_order is only an ordering key after a state finishes. During
// planned execution it indexes this side table, keeping NFA states unchanged.
static uint32_t query_execution_acquire_state(SQQueryCursor *self, QueryExecutionState position) {
  uint32_t index = self->execution_free_state;
  if (index == UINT32_MAX) {
    index = self->execution_states.size;
    array_push(&self->execution_states, position);
  } else {
    self->execution_free_state = self->execution_states.contents[index].root;
    self->execution_states.contents[index] = position;
  }

  return index;
}

static void query_execution_release_state(SQQueryCursor *self, const QueryState *state) {
  uint32_t index = state->heap_insert_order;
  self->execution_states.contents[index].root = self->execution_free_state;
  self->execution_free_state = index;
}

static bool sq_query_cursor__execution_advance(SQQueryCursor *self, bool stop_on_definite_step) {
  if (self->halted) {
    return false;
  }

  const SQQuery *query = self->query;
  const QueryExecutionPlan *plan = &query->execution_plan;
  const SQTree *tree = self->execution_root.tree;
  for (;;) {
    uint32_t next = self->scan_root_end;
    for (uint32_t index = 0; index < self->states.size; index++) {
      uint32_t position =
          self->execution_states.contents[self->states.contents[index].heap_insert_order].next;
      if (position < next) {
        next = position;
      }
    }

    uint32_t node = query_execution_find_root(self, tree, self->execution_position, next);
    if (self->halted) {
      return false;
    }

    QUERY_EXEC_COUNT(self, records_skipped, node - self->execution_position);

    // A childless or exhausted parent fails on exit, before any next root.
    for (uint32_t index = 0; index < self->states.size;) {
      QueryState *state = &self->states.contents[index];
      if (self->execution_states.contents[state->heap_insert_order].end <= node) {
        capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
        query_execution_release_state(self, state);
        array_erase(&self->states, index);
      } else {
        index++;
      }
    }

    if (node == self->scan_root_end) {
      self->halted = true;
      return false;
    }

    if (++self->operation_count == OP_COUNT_PER_QUERY_CALLBACK_CHECK) {
      self->operation_count = 0;
      if (self->query_options && self->query_options->progress_callback) {
        self->query_state.current_byte_offset = sq_node_start_byte(sq_position_node(tree, node));
        if (self->query_options->progress_callback(&self->query_state)) {
          self->halted = true;
          return false;
        }
      }
    }

    self->execution_position = sq_next_position(tree, node + 1);
    self->execution_last_node = node;
    uint16_t raw = sq_node_symbol_id(sq_position_node(tree, node));
    TSSymbol symbol = ts_language_public_symbol(query->language, sq_decode_symbol(tree, raw));
    uint64_t roots = plan->roots.contents[raw];
    while (roots) {
      uint32_t pattern = query_ctz(roots);
      roots &= roots - 1;
      QUERY_EXEC_COUNT(self, candidates, 1);
      array_push(
          &self->states,
          ((QueryState){
              .id = UINT32_MAX,
              .capture_list_id = CAPTURE_LIST_NONE,
              .heap_insert_order = query_execution_acquire_state(
                  self, (QueryExecutionState){node, node,
                                              sq_node_end_slot(sq_position_node(tree, node))}),
              .step_index = plan->start_steps[pattern],
              .pattern_index = pattern,
          }));
    }

    bool did_match = false;
    for (uint32_t index = 0; index < self->states.size;) {
      QueryState *state = &self->states.contents[index];
      if (state->dead) {
        capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
        query_execution_release_state(self, state);
        array_erase(&self->states, index);
        continue;
      }

      QueryExecutionState *position = &self->execution_states.contents[state->heap_insert_order];
      if (position->next != node) {
        index++;
        continue;
      }

      QUERY_EXEC_COUNT(self, execution_steps, 1);
      const QueryExecutionStep *operation = &plan->steps.contents[state->step_index];
      QueryStep *step = &query->steps.contents[state->step_index];
      bool matches =
          (!operation->symbol || operation->symbol == symbol) &&
          (!operation->field || operation->field == sq_node_field_id(sq_position_node(tree, node)));
      uint32_t sibling = sq_node_end_slot(sq_position_node(tree, node));
      if (operation->last_named_child &&
          query_execution_named_child(tree, query->language, sibling, position->end) <
              position->end) {
        matches = false;
      }

      if (!matches) {
        capture_list_pool_release(&self->capture_list_pool, state->capture_list_id);
        query_execution_release_state(self, state);
        array_erase(&self->states, index);
        continue;
      }

      if (step->capture_ids[0] != NONE) {
        sq_query_cursor__capture(self, state, step,
                                 sq_position_node(self->execution_root.tree, node));
        if (state->dead) {
          query_execution_release_state(self, state);
          array_erase(&self->states, index);
          continue;
        }
      }

      state->step_index = (plan->local_patterns & ((uint64_t)1 << state->pattern_index))
                              ? plan->end_steps[state->pattern_index]
                              : state->step_index + 1;
      const QueryStep *next_step = &query->steps.contents[state->step_index];
      if (stop_on_definite_step && next_step->root_pattern_guaranteed) {
        did_match = true;
      }

      if (next_step->depth == PATTERN_DONE_MARKER) {
        query_execution_release_state(self, state);
        sq_query_cursor__push_finished_state(self, state);
        array_erase(&self->states, index);
        did_match = true;
      } else {
        uint32_t start =
            plan->steps.contents[state->step_index].relation == QueryExecutionFirstNamedChild
                ? node + 1
                : sibling;
        position->next = query_execution_named_child(tree, query->language, start, position->end);
        index++;
      }
    }

    if (did_match) {
      return true;
    }
  }
}
