#include "../internal.h"
#include <assert.h>
#include <stdio.h>
#include "supertype_fixture.h"

static void width_policy_tests(void) {
  const struct { uint32_t max; uint8_t field, symbol; } cases[] = {
    {0, 0, 0}, {1, 1, 1}, {3, 2, 2}, {4, 4, 4}, {7, 4, 4},
    {15, 4, 4}, {16, 8, 8}, {31, 8, 8}, {63, 8, 8}, {127, 8, 8},
    {255, 8, 8}, {256, 9, 9}, {511, 9, 9}, {512, 10, 10},
    {1023, 10, 10}, {1024, 11, 16}, {2047, 11, 16},
    {4095, 12, 16}, {8191, 13, 16}, {16383, 14, 16},
    {32767, 15, 16}, {65535, 16, 16},
  };
  for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
    assert(sq_field_width(cases[i].max) == (SQ_FIXED_WIDTH ? 16 : cases[i].field));
    assert(sq_symbol_width(cases[i].max) == (SQ_FIXED_WIDTH ? 16 : cases[i].symbol));
  }
}

static void fixed_layout_limit_tests(void) {
#if SQ_FIXED_WIDTH
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
  assert(!sq_layout(&grammar, 1, false, true, &layout));
  language.field_count = 65535;
  language.symbol_count = 65534;
  assert(sq_layout(&grammar, 1, false, true, &layout));
  assert(layout.symbol_bits == 16 && layout.field_bits == 16 && layout.supertype_bits == 16);
#endif
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
#if SQ_FIXED_WIDTH
  assert(tree && tree->layout.field_bits == 16 && tree->layout.supertype_bits == 16);
  assert(SQ_WASTE_BITS == 16);
#else
  assert(tree && !tree->layout.field_bits && !tree->layout.field_lanes);
  assert(sq_column_size(SQ_GROUP_SIZE, 0) == 0);
  assert(tree->layout.field == tree->layout.supertype);
  assert(!tree->layout.supertype_bits);
  assert(tree->layout.supertype == tree->layout.last);
  // Missing columns must not read bytes belonging to the next column.
  tree->data[tree->layout.field] = 0xff;
#endif
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
#if SQ_FIXED_WIDTH
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
#endif
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
      assert(sq_get_packed_cached(serialized, 8, index, bits,
                                  (uint8_t)lanes, (uint32_t)mask) == expected);
    }
  }
}

static void unpack_tests(void) {
  for (uint8_t bits = 1; bits <= 16; bits++) {
    uint32_t lanes = 64 / bits;
    uint32_t slots = 4 * SQ_ITERATOR_UNPACK_SLOTS + lanes;
    size_t bytes = (size_t)sq_column_size(slots, bits);
    uint8_t *data = malloc(bytes);
    assert(data);

    // Deliberately dirty unused tail bits: no decoder may treat them as lanes.
    memset(data, 0xff, bytes);
    for (uint32_t index = 0; index < slots; index++) {
      sq_set_packed(data, 0, index, bits, (index * 7919u) & ((1u << bits) - 1));
    }

    for (unsigned kernel = 1; kernel <= 4; kernel++) {
      if (!sq_unpack_supported(kernel)) {
        continue;
      }

      SQUnpack unpack = sq_unpack_select(kernel);
      for (uint32_t first = 0; first < lanes; first++) {
        for (uint32_t count = 0; count <= SQ_ITERATOR_UNPACK_SLOTS; count++) {
          uint16_t values[SQ_ITERATOR_UNPACK_SLOTS + 2];
          for (unsigned index = 0; index < SQ_ITERATOR_UNPACK_SLOTS + 2; index++)
            values[index] = 0xbeef;
          unpack(data, first, count, bits, values + 1);
          assert(values[0] == 0xbeef && values[count + 1] == 0xbeef);
          for (uint32_t index = 0; index < count; index++) {
            assert(values[index + 1] == sq_get_packed(data, 0, first + index, bits));
          }
        }
      }

      // The last window ends at the last allocated word, with no overread slack.
      uint16_t values[SQ_ITERATOR_UNPACK_SLOTS];
      unpack(data, slots - SQ_ITERATOR_UNPACK_SLOTS, SQ_ITERATOR_UNPACK_SLOTS, bits, values);
      for (uint32_t index = 0; index < SQ_ITERATOR_UNPACK_SLOTS; index++) {
        assert(values[index] ==
               sq_get_packed(data, 0, slots - SQ_ITERATOR_UNPACK_SLOTS + index, bits));
      }
    }

    free(data);
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
  exercise_column(tree, tree->layout.error, 1, SQ_GROUP_SIZE, tag++, fill);
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
    assert(sq_node_symbol_id(node) == sq_get_packed(tree->data, tree->layout.symbol,
                                                  slot, tree->layout.symbol_bits));
    assert(sq_node_grammar_id(node) == sq_node_symbol_id(node));
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

static void sparse_grammar_tests(bool dictionary) {
  SupertypeFixture fixture;
  supertype_fixture(&fixture, dictionary ? 9 : 0, false);
  TSLanguage language = fixture.language;
  // The override fixture needs sixteen raw symbol IDs.
  language.symbol_count = 16;
  language.state_count = language.large_state_count = 1;
  SQError error;
  SQGrammar *grammar = sq_grammar_new(&language, &error);
  assert(grammar);
  const uint32_t groups = (130 + SQ_GROUP_SIZE - 1) / SQ_GROUP_SIZE;
  SQTree *tree = sq_allocate(grammar, groups + 3, true, &error);
  assert(tree);
  sq_header_set(tree, group_count, groups);
  sq_set_packed(tree->data, tree->layout.waste, groups - 1, SQ_WASTE_BITS,
                groups * SQ_GROUP_SIZE - 130);
  // 129 leaf siblings followed physically by their root.
  sq_set_bit(tree->data, tree->layout.last, 0, true);
  sq_set_bit(tree->data, tree->layout.last, 129, true);
  sq_set_u8(tree->data, tree->layout.span_delta, 129, 129);
  const uint32_t slots[] = {0, 63, 64, 127, 128, 129};
  const uint32_t count = sizeof(slots) / sizeof(slots[0]);
  uint32_t bytes = (uint32_t)sq_grammar_size(tree, count);
  assert(sq_prepare_final(&tree, groups + 3, bytes, &error));
  uint32_t offset = sq_grammar_offset(tree), words = sq_grammar_words(tree);
  uint32_t bitmap = offset + 8, ranks = bitmap + words * 8;
  uint32_t values = ranks + (uint32_t)sq_array_size(words, 4);
  memset(tree->data + offset, 0, bytes);
  sq_set_u32(tree->data, offset, 0, count);
  for (uint32_t i = 0; i < count; i++) {
    sq_set_bit(tree->data, bitmap, slots[i], true);
    sq_set_packed(tree->data, values, i, tree->layout.symbol_bits, i + 1);
  }
  // Known ranks at the word boundaries, independent of the reader's popcount.
  sq_set_u32(tree->data, ranks, 0, 0);
  sq_set_u32(tree->data, ranks, 1, 2);
  sq_set_u32(tree->data, ranks, 2, 4);
  sq_header_set(tree, format_flags, sq_header_get(tree, format_flags) | SQ_GRAMMAR_OVERRIDES);
  SQTree *loaded = sq_tree_from_bytes(grammar, tree->data, tree->size, &error);
  assert(loaded && error == SQ_OK);
  sq_tree_delete(loaded);
  const uint32_t capacities[] = {groups + 7, groups, groups + 1};
  for (unsigned pass = 0; pass < sizeof(capacities) / sizeof(capacities[0]); pass++) {
    assert(sq_resize(&tree, capacities[pass], &error));
    for (uint32_t slot = 0; slot < 130; slot++) {
      uint32_t expected = 0;
      for (uint32_t i = 0; i < count; i++) if (slots[i] == slot) expected = i + 1;
      assert(sq_node_grammar_id((SQNode){tree, slot}) == expected);
    }
    for (uint32_t group = 0; group < groups; group++) {
      for (uint32_t symbol = 0; symbol < 8; symbol++) {
        uint64_t expected = 0;
        for (uint32_t lane = 0; lane < SQ_GROUP_SIZE; lane++) {
          uint32_t slot = group * SQ_GROUP_SIZE + lane;
          if (slot >= 130) continue;
          uint32_t grammar = 0;
          for (uint32_t i = 0; i < count; i++) if (slots[i] == slot) grammar = i + 1;
          if (grammar == symbol) expected |= UINT64_C(1) << lane;
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
}

int main(void) {
  width_policy_tests();
  fixed_layout_limit_tests();
  empty_column_tests();
  sparse_grammar_tests(false);
  sparse_grammar_tests(true);
  equality_tests();
  read_tests();
  fixed_width_write_tests();
  unpack_tests();
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

  puts("ok: sparse grammar IDs, packed-column decoding, stable physical lanes, colocated growth, compaction, overflow");
  return 0;
}
