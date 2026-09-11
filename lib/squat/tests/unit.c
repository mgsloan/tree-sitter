#include "../internal.h"
#include <assert.h>
#include <stdio.h>

static void equality_tests(void) {
  uint64_t state = 42;
  for (uint8_t bits = 2; bits <= 32; bits++) {
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
  uint64_t state = 42;
  for (unsigned index = 0; index < 17; index++) {
    state = state * UINT64_C(6364136223846793005) + 1;
    words[index] = state;
  }

  for (uint8_t bits = 1; bits <= 32; bits++) {
    uint32_t lanes = 64 / bits;
    uint64_t mask = (UINT64_C(1) << bits) - 1;
    for (uint32_t index = 0; index < 16 * lanes; index++) {
      uint32_t expected = (uint32_t)((words[1 + index / lanes] >> (index % lanes * bits)) & mask);
      assert(sq_get_packed((const uint8_t *)words, 8, index, bits) == expected);
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
    for (uint32_t index = 2 * scale; index < sq_header(tree)->group_capacity * scale; index++) {
      assert(sq_get_packed(tree->data, offset, index, bits) == 0);
    }
  }
}

static void exercise_columns(SQTree *tree, bool fill) {
  unsigned tag = 0;
  exercise_column(tree, tree->layout.waste, SQ_WASTE_BITS, 1, tag++, fill);
  exercise_column(tree, tree->layout.span_base, 32, 1, tag++, fill);
  exercise_column(tree, tree->layout.start_byte_base, 32, 1, tag++, fill);
  exercise_column(tree, tree->layout.end_byte_base, 32, 1, tag++, fill);
#if SQ_INCLUDE_POINTS
  exercise_column(tree, tree->layout.start_row_base, 32, 1, tag++, fill);
  exercise_column(tree, tree->layout.end_row_base, 32, 1, tag++, fill);
  exercise_column(tree, tree->layout.start_column_base, 32, 1, tag++, fill);
  exercise_column(tree, tree->layout.end_column_base, 32, 1, tag++, fill);
#endif
  exercise_column(tree, tree->layout.last, 1, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.extra, 1, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.error, 1, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.missing, 1, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.span_delta, 8, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.start_byte_delta, 8, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.end_byte_delta, 16, SQ_GROUP_SIZE, tag++, fill);
#if SQ_INCLUDE_POINTS
  exercise_column(tree, tree->layout.start_row_delta, 8, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.end_row_delta, 8, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.start_column_delta, 8, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.end_column_delta, 8, SQ_GROUP_SIZE, tag++, fill);
#endif
  exercise_column(tree, tree->layout.supertype, 8, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.symbol, tree->layout.symbol_bits, SQ_GROUP_SIZE, tag++, fill);
  exercise_column(tree, tree->layout.grammar_symbol, tree->layout.symbol_bits, SQ_GROUP_SIZE, tag++,
                  fill);
  exercise_column(tree, tree->layout.field, tree->layout.field_bits, SQ_GROUP_SIZE, tag++, fill);
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
      uint64_t expected = (words[index / lanes] & ~(mask << shift)) | ((uint64_t)value << shift);
      uint8_t *bytes = (uint8_t *)words;
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

      assert(words[index / lanes] == expected);
    }
  }
}

int main(void) {
  equality_tests();
  read_tests();
  fixed_width_write_tests();
  unpack_tests();
  coordinate_unpack_tests();
  for (uint32_t symbols = 2; symbols <= 32768; symbols *= 2) {
    TSSymbolMetadata *metadata = calloc(symbols, sizeof(TSSymbolMetadata));
    assert(metadata);
    TSLanguage language = {.abi_version = TREE_SITTER_LANGUAGE_VERSION,
                           .symbol_count = symbols,
                           .field_count = symbols - 1,
                           .symbol_metadata = metadata};
    SQError error;
    SQTree *tree = sq_allocate(&language, 3, &error);
    assert(tree && error == SQ_OK);
    sq_header(tree)->group_count = 2;
    exercise_columns(tree, true);
    const uint32_t capacities[] = {7, 19, 2, 31, 2};
    for (unsigned k = 0; k < sizeof(capacities) / sizeof(capacities[0]); k++) {
      assert(sq_resize(&tree, capacities[k], &error));
      assert(tree->storage == SQ_STORAGE_COLOCATED);
      assert(tree->data == (uint8_t *)tree + sq_runtime_size(&language));
      assert(tree->supertypes == (TSSymbol *)(tree + 1));
      exercise_columns(tree, false);
    }

    assert(!sq_resize(&tree, UINT32_MAX, &error) && error == SQ_ERROR_OVERFLOW);
    sq_tree_delete(tree);
    free(metadata);
  }

  puts("ok: packed-column decoding, stable physical lanes, colocated growth, compaction, overflow");
  return 0;
}
