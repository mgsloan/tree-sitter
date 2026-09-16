#include "../internal.h"
#include <assert.h>
#include <stdio.h>
#include "supertype_fixture.h"

static void fixed_layout_limit_tests(void) {
  TSLanguage language = {.abi_version = TREE_SITTER_LANGUAGE_VERSION, .symbol_count = 65535};
  SQError error;
  assert(!sq_grammar_new(&language, &error) && error == SQ_ERROR_OVERFLOW);
  language.symbol_count = 1;
  language.alias_count = UINT32_MAX;
  assert(!sq_grammar_new(&language, &error) && error == SQ_ERROR_OVERFLOW);
  language.alias_count = 0;
  language.field_count = 65536;
  assert(!sq_grammar_new(&language, &error) && error == SQ_ERROR_OVERFLOW);
  SQGrammar grammar = {.language = &language};
  SQLayout layout;
  assert(!sq_layout(&grammar, 1, false, true, SQ_ERRORS | SQ_MISSING, &layout));
  language.field_count = 65535;
  language.symbol_count = 65534;
  assert(sq_layout(&grammar, 1, false, true, SQ_ERRORS | SQ_MISSING, &layout));
  assert(layout.symbol_bits == 16 && layout.field_bits == 16 && layout.supertype_bits == 16);
}

static void empty_column_tests(void) {
  const char *names[] = {"end", "node"};
  const TSSymbolMetadata metadata[] = {{0}, {.visible = true, .named = true}};
  const TSSymbol public_symbols[] = {0, 1};
  const TSLanguage language = {.abi_version = TREE_SITTER_LANGUAGE_VERSION,
                               .symbol_count = 2,
                               .symbol_names = names,
                               .public_symbol_map = public_symbols,
                               .symbol_metadata = metadata};
  SQError error;
  SQGrammar *grammar = sq_grammar_new(&language, &error);
  assert(grammar);
  SQTree *tree = sq_allocate(grammar, 1, true, &error);
  assert(tree && tree->layout.field_bits == 16 && tree->layout.supertype_bits == 16);
  assert(SQ_WASTE_BITS == 16);
  sq_header_set(tree, group_count, 1);
  for (unsigned waste = 0; waste < SQ_GROUP_SIZE; waste++) {
    sq_set_packed(tree->data, tree->layout.waste, 0, SQ_WASTE_BITS, waste);
    uint64_t used = UINT64_MAX >> (64 - (SQ_GROUP_SIZE - waste));
    assert(sq_tree_group_field_equal(tree, 0, 0) == used);
    assert(sq_tree_group_field_equal(tree, 0, 1) == 0);
    assert(sq_tree_group_field_equal(tree, 0, UINT32_MAX) == 0);
    assert(sq_tree_group_field_equal(tree, 1, 0) == 0);
    assert(sq_tree_group_supertype_equal(tree, 0, 0) == used);
    assert(sq_tree_group_supertype_equal(tree, 0, 1) == 0);
    assert(sq_tree_group_supertype_equal(tree, 1, 0) == 0);
    for (unsigned slot = 0; slot < SQ_GROUP_SIZE - waste; slot++) {
      assert(sq_node_field_value((SQNode){tree, slot}) == 0);
      assert(sq_node_field_id((SQNode){tree, slot}) == 0);
      assert(sq_node_supertype((SQNode){tree, slot}) == 0);
      assert(!sq_node_has_supertype((SQNode){tree, slot}, 1));
    }
  }
  const uint16_t targets[] = {0, 32768, UINT16_MAX};
  const uint64_t patterns[] = {0, UINT64_MAX, UINT64_C(0xaaaaaaaaaaaaaaaa),
                                UINT64_C(0x8001800180018001)};
  for (unsigned target = 0; target < sizeof(targets) / sizeof(targets[0]); target++) {
    for (unsigned pattern = 0; pattern < sizeof(patterns) / sizeof(patterns[0]); pattern++) {
      for (unsigned lane = 0; lane < SQ_GROUP_SIZE; lane++) {
        uint16_t value = targets[target] ^ ((patterns[pattern] >> lane & 1) ? 0 : 1);
        sq_set_u16(tree->data, tree->layout.field, lane, value);
      }
      for (unsigned waste = 0; waste < SQ_GROUP_SIZE; waste++) {
        sq_set_u16(tree->data, tree->layout.waste, 0, (uint16_t)waste);
        uint64_t used = UINT64_MAX >> (64 - SQ_GROUP_SIZE + waste);
        assert(sq_tree_group_field_equal(tree, 0, targets[target]) == (patterns[pattern] & used));
        assert(!sq_tree_group_field_equal(tree, 0, 65536));
        assert(!sq_tree_group_field_equal(tree, 1, targets[target]));
      }
    }
  }
  const uint32_t invalid_waste[] = {SQ_GROUP_SIZE, SQ_GROUP_SIZE + 1, UINT16_MAX};
  for (unsigned i = 0; i < sizeof(invalid_waste) / sizeof(invalid_waste[0]); i++) {
    sq_set_packed(tree->data, tree->layout.waste, 0, SQ_WASTE_BITS, invalid_waste[i]);
    assert(!sq_tree_from_bytes(grammar, tree->data, tree->size, &error));
    assert(error == SQ_ERROR_INVALID_SLAB);
    assert(!sq_tree_from_bytes_borrowed(grammar, tree->data, tree->size, &error));
    assert(error == SQ_ERROR_INVALID_SLAB);
    assert(!sq_tree_from_bytes_safety_checked(grammar, tree->data, tree->size, &error));
    assert(error == SQ_ERROR_INVALID_SLAB);
    assert(!sq_tree_from_bytes_borrowed_safety_checked(grammar, tree->data, tree->size, &error));
    assert(error == SQ_ERROR_INVALID_SLAB);
  }
  sq_tree_delete(tree);
  sq_grammar_delete(grammar);
}

static void equality_tests(void) {
  uint64_t state = 42;
  for (uint8_t bits = 1; bits <= 32; bits++) {
    uint64_t lane_mask = (UINT64_C(1) << bits) - 1;
    for (unsigned trial = 0; trial < 10000; trial++) {
      state = state * UINT64_C(6364136223846793005) + 1;
      uint64_t word = state;
      uint32_t target = (uint32_t)(state >> 32) & (uint32_t)lane_mask;
      uint64_t expected = 0;
      for (unsigned lane = 0; lane < 64 / bits; lane++) {
        if (((word >> (lane * bits)) & lane_mask) == target) {
          expected |= UINT64_C(1) << (lane * bits + bits - 1);
        }
      }

      assert(sq_equal_lanes(word, target, bits) == expected);
    }

    // These adjacent lanes catch borrow/carry false positives in has-zero idioms.
    assert(sq_equal_lanes(0, 0, bits) == (sq_lane_starts(bits) << (bits - 1)));
    assert(sq_equal_lanes(sq_lane_starts(bits), 0, bits) == 0);
  }
}

static void read_tests(void) {
  uint64_t words[17];
  uint8_t serialized[sizeof(words)];
  uint64_t state = 42;
  for (unsigned index = 0; index < 17; index++) {
    state = state * UINT64_C(6364136223846793005) + 1;
    words[index] = state;
    for (unsigned byte = 0; byte < 8; byte++)
      serialized[index * 8 + byte] = (uint8_t)(state >> (byte * 8));
  }

  for (uint8_t bits = 1; bits <= 32; bits++) {
    uint32_t lanes = 64 / bits;
    uint64_t mask = (UINT64_C(1) << bits) - 1;
    for (uint32_t index = 0; index < 16 * lanes; index++) {
      uint32_t expected = (uint32_t)((words[1 + index / lanes] >> (index % lanes * bits)) & mask);
      assert(sq_get_packed(serialized, 8, index, bits) == expected);

    }
  }
}

static void coordinate_unpack_tests(void) {
  const uint32_t bases[] = {0, 255, 65535, 0x80000000u, UINT32_MAX};
  const unsigned kernels[] = {0, 1, 2, 4};
  for (uint8_t bits = 8; bits <= 16; bits += 8) {
    // An exact allocation catches vector loads crossing the last packed word.
    uint32_t slots = SQ_ITERATOR_UNPACK_SLOTS + 8;
    uint8_t *data = malloc(sq_column_size(slots, bits));
    assert(data);
    memset(data, 0, sq_column_size(slots, bits));
    for (uint32_t index = 0; index < slots; index++) {
      sq_set_packed(data, 0, index, bits, (index * 7919u) & ((1u << bits) - 1));
    }

    for (unsigned kernel = 0; kernel < sizeof(kernels) / sizeof(kernels[0]); kernel++) {
      SQUnpackCoordinates unpack = sq_unpack_coordinates_select(kernels[kernel]);
      for (unsigned base_index = 0; base_index < sizeof(bases) / sizeof(bases[0]); base_index++) {
        uint32_t base = bases[base_index];
        for (unsigned subtract = 0; subtract < 2; subtract++) {
          for (uint32_t count = 0; count <= SQ_ITERATOR_UNPACK_SLOTS; count++) {
            uint32_t values[SQ_ITERATOR_UNPACK_SLOTS + 2];
            values[0] = values[count + 1] = 0xdeadbeef;

            // Vary the input alignment and exercise every scalar/vector tail.
            uint32_t first = slots - count;
            unpack(data, first, count, bits, base, subtract, values + 1);
            assert(values[0] == 0xdeadbeef && values[count + 1] == 0xdeadbeef);
            for (uint32_t index = 0; index < count; index++) {
              uint32_t delta = sq_get_packed(data, 0, first + index, bits);
              assert(values[index + 1] == (subtract ? base - delta : base + delta));
            }
          }
        }
      }
    }

    free(data);
  }
}

// Use the scalar packed-word contract to check every named column through
// repeated growth/compaction. Tags distinguish equal-width columns.
static void exercise_column(SQTree *tree, uint32_t offset, uint8_t bits, uint32_t scale,
                            unsigned tag, bool fill) {
  uint64_t mask = (UINT64_C(1) << bits) - 1;
  for (uint32_t index = 0; index < 2 * scale; index++) {
    uint32_t expected = (uint32_t)((index * UINT64_C(31337) + tag) & mask);
    if (fill) sq_set_packed(tree->data, offset, index, bits, expected);
    else assert(sq_get_packed(tree->data, offset, index, bits) == expected);
  }

  if (!fill) {
    for (uint32_t index = 2 * scale; index < sq_header_get(tree, group_capacity) * scale; index++) {
      assert(sq_get_packed(tree->data, offset, index, bits) == 0);
    }
  }
}

static void exercise_u64_column(SQTree *tree, uint32_t offset, unsigned tag, bool fill) {
  for (uint32_t index = 0; index < 2; index++) {
    uint64_t expected = index * UINT64_C(0x123456789abcdef) + tag;
    if (fill) sq_set_u64(tree->data, offset, index, expected);
    else assert(sq_get_u64(tree->data, offset, index) == expected);
  }

  if (!fill) {
    for (uint32_t index = 2; index < sq_header_get(tree, group_capacity); index++) {
      assert(sq_get_u64(tree->data, offset, index) == 0);
    }
  }
}

static void exercise_columns(SQTree *tree, bool fill) {
  unsigned tag = 0;
  exercise_column(tree, tree->layout.waste, SQ_WASTE_BITS, 1, tag++, fill);
  exercise_column(tree, tree->layout.span_base, 32, 1, tag++, fill);
  exercise_column(tree, tree->layout.start_byte_base, 32, 1, tag++, fill);
  exercise_column(tree, tree->layout.end_byte_base, 32, 1, tag++, fill);
  exercise_u64_column(tree, tree->layout.start_point_base, tag++, fill);
  exercise_u64_column(tree, tree->layout.end_point_base, tag++, fill);
  exercise_column(tree, tree->layout.last, 1, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.extra, 1, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.error, 1, 1, tag++, fill);
  exercise_column(tree, tree->layout.missing, 1, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.span_delta, 8, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.start_byte_delta, 8, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.end_byte_delta, 16, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.start_point, 16, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.end_point, 16, SQ_GROUP_SIZE, tag++, fill);
  if (tree->layout.supertype_bits) {
    exercise_column(tree, tree->layout.supertype, tree->layout.supertype_bits, SQ_GROUP_SIZE, tag++, fill);
  }
  exercise_column(tree, tree->layout.symbol, tree->layout.symbol_bits, SQ_GROUP_SIZE, tag++, fill);
  if (tree->layout.field_bits) {
    exercise_column(tree, tree->layout.field, tree->layout.field_bits, SQ_GROUP_SIZE, tag++, fill);
  }
  // Verify cached layout constants after initial allocation and every resize.
  assert(tree->layout.symbol_lanes == 64 / tree->layout.symbol_bits);
  assert(tree->layout.field_lanes == (tree->layout.field_bits ? 64 / tree->layout.field_bits : 0));
  for (uint32_t slot = 0; slot < 2 * SQ_GROUP_SIZE; slot++) {
    SQNode node = {tree, slot};
    assert(sq_node_symbol_code(node) == sq_get_packed(tree->data, tree->layout.symbol,
                                                  slot, tree->layout.symbol_bits));
    uint32_t field = tree->layout.field_bits
      ? sq_get_packed(tree->data, tree->layout.field, slot, tree->layout.field_bits) : 0;
    assert(sq_node_field_value(node) == field);
  }
}

static void fixed_width_write_tests(void) {
  for (uint8_t bits = 1; bits <= 32; bits *= 2) {
    if (bits == 2 || bits == 4) continue;
    uint64_t words[3] = {UINT64_MAX, UINT64_MAX, UINT64_MAX};
    uint32_t lanes = 64 / bits;
    uint64_t mask = (UINT64_C(1) << bits) - 1;
    for (uint32_t index = 0; index < 3 * lanes; index++) {
      uint32_t value = (uint32_t)((index * UINT64_C(31337)) & mask);
      uint32_t shift = index % lanes * bits;
      uint8_t *bytes = (uint8_t *)words;
      uint64_t expected = (sq_get_u64(bytes, 0, index / lanes) & ~(mask << shift)) |
                          ((uint64_t)value << shift);
      switch (bits) {
      case 1:
        sq_set_bit(bytes, 0, index, value);
        assert(sq_get_bit(bytes, 0, index) == value);
        break;
      case 8:
        sq_set_u8(bytes, 0, index, value);
        assert(sq_get_u8(bytes, 0, index) == value);
        break;
      case 16:
        sq_set_u16(bytes, 0, index, value);
        assert(sq_get_u16(bytes, 0, index) == value);
        break;
      case 32:
        sq_set_u32(bytes, 0, index, value);
        assert(sq_get_u32(bytes, 0, index) == value);
        break;
      }

      for (unsigned byte = 0; byte < 8; byte++)
        assert(bytes[index / lanes * 8 + byte] == (uint8_t)(expected >> (byte * 8)));
    }
  }
}

static void symbol_pair_tests(uint32_t symbols, bool separate) {
  SupertypeFixture fixture;
  supertype_fixture(&fixture, 0, false);
  TSLanguage language = fixture.language;
  TSSymbol *public_symbols = malloc(symbols * sizeof(TSSymbol));
  TSSymbolMetadata *metadata = calloc(symbols, sizeof(TSSymbolMetadata));
  assert(public_symbols && metadata);
  for (uint32_t symbol = 0; symbol < symbols; symbol++) public_symbols[symbol] = symbol;
  // Two grammar IDs share display zero. The error IDs force the fallback at 32767.
  public_symbols[1] = 0;
  language.symbol_count = symbols;
  language.public_symbol_map = public_symbols;
  language.symbol_metadata = metadata;
  language.state_count = language.large_state_count = 1;
  SQError error;
  SQGrammar *grammar = sq_grammar_new(&language, &error);
  assert(grammar && grammar->symbols.separate == separate);
  assert(grammar->symbols.shift == (separate ? 0 : symbols <= 254 ? 8 : symbols == 300 ? 2 : 1));
  if (symbols == 300) {
    assert(grammar->symbols.encoding == SQ_SYMBOL_GLOBAL);
    assert(grammar->symbols.default_codes[0] != 0);
    assert((grammar->symbols.default_codes[2] & 3) == 0);
  }
  const uint32_t groups = (130 + SQ_GROUP_SIZE - 1) / SQ_GROUP_SIZE;
  SQTree *tree = sq_allocate(grammar, groups + 3, true, &error);
  assert(tree);
  sq_header_set(tree, group_count, groups);
  sq_set_packed(tree->data, tree->layout.waste, groups - 1, SQ_WASTE_BITS,
                groups * SQ_GROUP_SIZE - 130);
  sq_set_bit(tree->data, tree->layout.last, 0, true);
  sq_set_bit(tree->data, tree->layout.last, 129, true);
  sq_set_u8(tree->data, tree->layout.span_delta, 129, 129);
  for (uint32_t slot = 0; slot < 130; slot++) {
    uint16_t original = slot % 4;
    sq_set_u16(tree->data, tree->layout.symbol, slot, grammar->symbols.default_codes[original]);
    if (separate) sq_set_u16(tree->data, tree->layout.grammar, slot, original);
  }
  SQTree *loaded = sq_tree_from_bytes(grammar, tree->data, tree->size, &error);
  assert(loaded && error == SQ_OK);
  sq_tree_delete(loaded);
  uint32_t invalid_column = separate ? tree->layout.grammar : tree->layout.symbol;
  sq_set_u16(tree->data, invalid_column, 0, separate ? UINT16_MAX : symbols <= 254 ? 0xff00 : 5);
  assert(!sq_tree_from_bytes(grammar, tree->data, tree->size, &error));
  assert(error == SQ_ERROR_INVALID_SLAB);
  assert(!sq_tree_from_bytes_borrowed_safety_checked(grammar, tree->data, tree->size, &error));
  assert(error == SQ_ERROR_INVALID_SLAB);
  sq_set_u16(tree->data, invalid_column, 0, separate ? 0 : grammar->symbols.default_codes[0]);
  const uint32_t capacities[] = {groups + 7, groups, groups + 1};
  for (unsigned pass = 0; pass < sizeof(capacities) / sizeof(capacities[0]); pass++) {
    assert(sq_resize(&tree, capacities[pass], &error));
    for (uint32_t slot = 0; slot < 130; slot++) {
      SQNode node = {tree, slot};
      assert(sq_node_symbol_id(node) == (slot % 4 < 2 ? 0 : slot % 4));
      assert(sq_node_grammar_id(node) == slot % 4);
    }
    for (uint32_t group = 0; group < groups; group++) {
      for (uint32_t symbol = 0; symbol < 4; symbol++) {
        uint64_t expected = 0;
        for (uint32_t lane = 0; lane < SQ_GROUP_SIZE; lane++) {
          uint32_t slot = group * SQ_GROUP_SIZE + lane;
          if (slot < 130 && slot % 4 == symbol) expected |= UINT64_C(1) << lane;
        }
        assert(sq_tree_group_grammar_symbol_equal(tree, group, symbol) == expected);
      }
    }
    loaded = sq_tree_repack(tree, &error);
    assert(loaded && sq_tree_group_capacity(loaded) == groups);
    sq_tree_delete(loaded);
  }
  sq_tree_delete(tree);
  sq_grammar_delete(grammar);
  free(metadata);
  free(public_symbols);
}

static void terminal_alias_tests(void) {
  TSSymbol public_symbols[300], aliases[] = {0, 0, 3, 0}, alias_map[] = {0};
  TSSymbolMetadata metadata[300] = {0};
  uint16_t parse_table[4 * 300] = {0};
  for (uint32_t symbol = 0; symbol < 300; symbol++) public_symbols[symbol] = symbol;
  TSParseActionEntry actions[5] = {0};
  actions[1].entry.count = 1;
  actions[2].action = (TSParseAction){.shift = {.type = TSParseActionTypeShift, .state = 2}};
  actions[3].entry.count = 1;
  actions[4].action = (TSParseAction){.reduce = {
      .type = TSParseActionTypeReduce, .symbol = 2, .child_count = 2, .production_id = 1}};
  parse_table[300 + 1] = 1;
  parse_table[600 + 2] = 3;
  parse_table[900] = 3;
  // A terminal shift followed by a nonterminal goto, with the first child aliased.
  TSLanguage language = {.abi_version = TREE_SITTER_LANGUAGE_VERSION,
      .symbol_count = 300, .token_count = 2, .state_count = 4, .large_state_count = 4,
      .production_id_count = 2, .max_alias_sequence_length = 2,
      .public_symbol_map = public_symbols, .symbol_metadata = metadata,
      .parse_table = parse_table, .parse_actions = actions,
      .alias_map = alias_map, .alias_sequences = aliases};
  SQError error;
  SQGrammar *grammar = sq_grammar_new(&language, &error);
  assert(grammar && grammar->symbols.encoding == SQ_SYMBOL_GLOBAL);
  assert(grammar->symbols.counts[3] == 2);
  uint32_t code = sq_symbol_code(grammar, 3, 1);
  assert(code != SQ_NONE && code >> grammar->symbols.shift == 3);
  SQTree *tree = sq_allocate(grammar, 1, true, &error);
  assert(tree);
  sq_set_u16(tree->data, tree->layout.symbol, 0, code);
  assert(sq_node_grammar_id((SQNode){tree, 0}) == 1);
  sq_tree_delete(tree);
  sq_grammar_delete(grammar);
}

static void optional_flag_tests(void) {
  const char *names[] = {"end", "node"};
  const TSSymbolMetadata metadata[] = {{0}, {.visible = true, .named = true}};
  const TSSymbol public_symbols[] = {0, 1};
  const TSLanguage language = {.abi_version = TREE_SITTER_LANGUAGE_VERSION,
                               .symbol_count = 2, .symbol_names = names,
                               .public_symbol_map = public_symbols,
                               .symbol_metadata = metadata};
  SQError error;
  SQGrammar *grammar = sq_grammar_new(&language, &error);
  assert(grammar);
  const uint32_t combinations[] = {0, SQ_EXTRAS, SQ_ERRORS, SQ_EXTRAS | SQ_ERRORS,
                                   SQ_MISSING | SQ_ERRORS, SQ_EXTRAS | SQ_MISSING | SQ_ERRORS};
  for (unsigned representation = 0; representation < 3; representation++) {
    grammar->symbols.separate = representation != 0;
    grammar->symbols.encoding = representation ? SQ_SYMBOL_LOCAL : SQ_SYMBOL_BYTES;
    grammar->symbols.shift = representation ? 0 : 8;
    for (unsigned combination = 0; combination < 6; combination++) {
      uint32_t flags = combinations[combination] |
                       (representation == 2 ? SQ_SEPARATE_GRAMMAR : 0);
      for (unsigned variant = 0; variant < 4; variant++) {
        SQTree *tree = sq_allocate(grammar, 70, variant & 1, &error);
        assert(tree);
        uint32_t root = 64 * SQ_GROUP_SIZE;
        sq_header_set(tree, group_count, 65);
        sq_set_u16(tree->data, tree->layout.waste, 64, SQ_GROUP_SIZE - 1);
        sq_set_u32(tree->data, tree->layout.span_base, 64, root);
        sq_set_bit(tree->data, tree->layout.last, 0, true);
        sq_set_bit(tree->data, tree->layout.last, root, true);
        for (uint32_t slot = 0; slot <= root; slot++) {
          sq_set_u16(tree->data, tree->layout.symbol, slot, representation ? 1 : 257);
          if (representation)
            sq_set_u16(tree->data, tree->layout.grammar, slot, representation == 2 && slot == 2 ? 0 : 1);
        }
        if (flags & SQ_EXTRAS) sq_set_bit(tree->data, tree->layout.extra, 2, true);
        if (flags & SQ_MISSING)
          sq_set_bit(tree->data, tree->layout.missing, 63 * SQ_GROUP_SIZE, true);
        if (flags & SQ_ERRORS) {
          sq_set_bit(tree->data, tree->layout.error, 63, true);
          sq_set_bit(tree->data, tree->layout.error, 64, true);
        }
        assert(sq_prepare_final(&tree, variant & 2 ? 65 : 70, 0, flags, &error));
        assert((sq_header_get(tree, format_flags) & SQ_OPTIONAL_FLAGS) == flags);
        assert(tree->layout.extra <= tree->layout.missing &&
               tree->layout.missing <= tree->layout.error &&
               tree->layout.error <= tree->layout.grammar && tree->layout.grammar <= tree->size);
        uint32_t capacity = sq_header_get(tree, group_capacity);
        SQLayout full;
        assert(sq_layout(grammar, capacity, true, variant & 1, SQ_EXTRAS | SQ_MISSING | SQ_ERRORS |
                          (representation ? SQ_SEPARATE_GRAMMAR : 0), &full));
        uint32_t saved = 0;
        if (!(flags & SQ_EXTRAS)) saved += full.missing - full.extra;
        if (!(flags & SQ_MISSING)) saved += full.error - full.missing;
        if (!(flags & SQ_ERRORS)) saved += full.grammar - full.error;
        if (!(flags & SQ_SEPARATE_GRAMMAR)) saved += full.end - full.grammar;
        assert(tree->size == full.end - saved);
        for (unsigned pass = 0; pass < 3; pass++) {
          for (uint32_t slot = 0; slot <= root; slot++) {
            SQNode node = {tree, slot};
            assert(sq_node_symbol_id(node) == 1);
            assert(sq_node_grammar_id(node) == (representation == 2 && slot == 2 ? 0 : 1));
            assert(sq_node_is_extra(node) == ((flags & SQ_EXTRAS) && slot == 2));
            assert(sq_node_is_missing(node) ==
                   ((flags & SQ_MISSING) && slot == 63 * SQ_GROUP_SIZE));
            assert(sq_node_has_error(node) ==
                   ((flags & SQ_ERRORS) && slot / SQ_GROUP_SIZE >= 63));
          }
          uint64_t expected = representation == 2 ? UINT64_C(1) << 2 : 0;
          assert(sq_tree_group_grammar_symbol_equal(tree, 0, 0) == expected);
          uint32_t size = sq_tree_compact_size(tree);
          uint8_t *bytes = sq_allocate_data(size);
          assert(bytes && sq_tree_copy_compact(tree, bytes, size, &error));
          SQTree *copy = sq_tree_from_bytes(grammar, bytes, size, &error);
          SQTree *borrowed = sq_tree_from_bytes_borrowed(grammar, bytes, size, &error);
          assert(copy && borrowed);
          assert(sq_node_has_error(sq_tree_root_node(borrowed)) == !!(flags & SQ_ERRORS));
          sq_tree_delete(borrowed);
          free(bytes);
          sq_tree_delete(tree);
          tree = copy;
          assert(sq_resize(&tree, pass == 0 ? 90 : 65, &error));
        }
        sq_tree_delete(tree);
      }
    }
  }
  sq_grammar_delete(grammar);
}

int main(void) {
  optional_flag_tests();
  terminal_alias_tests();
  fixed_layout_limit_tests();
  empty_column_tests();
  symbol_pair_tests(16, false);
  symbol_pair_tests(300, false);
  symbol_pair_tests(32766, false);
  symbol_pair_tests(32767, true);
  equality_tests();
  read_tests();
  fixed_width_write_tests();
  coordinate_unpack_tests();
  for (uint32_t symbols = 2; symbols <= 32768; symbols *= 2) {
    TSSymbolMetadata *metadata = calloc(symbols, sizeof(TSSymbolMetadata));
    assert(metadata);
    metadata[1].supertype = true;
    TSSymbol *public_symbols = calloc(symbols, sizeof(TSSymbol));
    assert(public_symbols);
    for (uint32_t symbol = 0; symbol < symbols; symbol++) public_symbols[symbol] = symbol;
    TSLanguage language = {.public_symbol_map = public_symbols,
                           .abi_version = TREE_SITTER_LANGUAGE_VERSION,
                           .symbol_count = symbols,
                           .field_count = symbols - 1,
                           .symbol_metadata = metadata};
    SQError error;
    SQGrammar *grammar = sq_grammar_new(&language, &error);
    assert(grammar);
    SQTree *tree = sq_allocate(grammar, 3, true, &error);
    assert(tree && error == SQ_OK);
    sq_header_set(tree, group_count, 2);
    exercise_columns(tree, true);
    const uint32_t capacities[] = {7, 19, 2, 31, 2};
    for (unsigned k = 0; k < sizeof(capacities) / sizeof(capacities[0]); k++) {
      assert(sq_resize(&tree, capacities[k], &error));
      assert(tree->storage == SQ_STORAGE_COLOCATED);
      assert(tree->data == (uint8_t *)tree + sq_runtime_size());
      assert(tree->supertypes == grammar->supertypes);
      exercise_columns(tree, false);
      SQTree *same = tree;
      assert(sq_resize(&tree, capacities[k], &error) && tree == same);
    }

    assert(!sq_resize(&tree, UINT32_MAX, &error) && error == SQ_ERROR_OVERFLOW);
    sq_tree_delete(tree);
    sq_grammar_delete(grammar);
    free(public_symbols);
    free(metadata);
  }

  puts("ok: combined and separate grammar IDs, packed-column decoding, stable physical lanes, colocated growth, compaction, overflow");
  return 0;
}
