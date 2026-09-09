#include "internal.h"
#if defined(__x86_64__)
#include <immintrin.h>
#define SQ_X86_UNPACK 1
#else
#define SQ_X86_UNPACK 0
#endif

/* Expand four densely packed fields into four u16 lanes. Splitting the two
 * pairs before inserting gaps avoids overlap when 4 * bits exceeds 32. */
static inline uint64_t spread_four(uint64_t word, uint8_t bits, uint64_t mask) {
  uint64_t pair_mask = (UINT64_C(1) << (2 * bits)) - 1;
  uint64_t pairs = (word & pair_mask) | ((word >> (2 * bits)) << 32);
  uint64_t low_lanes = mask | (mask << 32);
  return (pairs & low_lanes) | ((pairs & (low_lanes << bits)) << (16 - bits));
}

/* The narrow layouts already occupy whole bytes. Widen eight bytes together
 * with SSE2, or copy native u16 lanes, rather than routing them through PDEP. */
static bool unpack_narrow(const uint8_t *column, uint32_t first, uint32_t count,
                           uint8_t bits, uint16_t *out) {
  const uint16_t endian = 1;
  if (!*(const uint8_t *)&endian) {
    return false;
  }
  if (bits == 16) {
    memcpy(out, column + (size_t)first * 2, (size_t)count * 2);
    return true;
  }
  if (bits != 8) {
    return false;
  }
  column += first;
#if defined(__x86_64__)
  while (count >= 8) {
    __m128i bytes = _mm_loadl_epi64((const __m128i *)column);
    _mm_storeu_si128((__m128i *)out, _mm_unpacklo_epi8(bytes, _mm_setzero_si128()));
    column += 8;
    out += 8;
    count -= 8;
  }
#endif
  while (count--) {
    *out++ = *column++;
  }
  return true;
}

typedef uint64_t (*ExpandFour)(uint64_t, uint8_t, uint64_t);

static inline void unpack_words(const uint8_t *column, uint32_t first, uint32_t count,
                                uint8_t bits, uint16_t *out, ExpandFour expand) {
  uint32_t lanes = 64 / bits;
  uint32_t word_index = first / lanes;
  uint32_t skip = first % lanes;
  uint64_t mask = (UINT64_C(1) << bits) - 1;
  while (count) {
    uint64_t word;
    memcpy(&word, column + (size_t)word_index++ * 8, sizeof(word));
    word >>= skip * bits;
    uint32_t take = lanes - skip;
    if (take > count) {
      take = count;
    }
    count -= take;
    skip = 0;
    while (take >= 4) {
      uint64_t expanded = expand(word, bits, mask);
      const uint16_t endian = 1;
      if (*(const uint8_t *)&endian) {
        memcpy(out, &expanded, sizeof(expanded));
      } else {
        for (unsigned index = 0; index < 4; index++) {
          out[index] = (uint16_t)(expanded >> (16 * index));
        }
      }
      out += 4;
      take -= 4;
      // Four 16-bit lanes consume the whole word; never shift by 64.
      if (take) {
        word >>= 4 * bits;
      }
    }
    while (take--) {
      *out++ = (uint16_t)(word & mask);
      word >>= bits;
    }
  }
}

void sq_unpack_u16_scalar(const uint8_t *column, uint32_t first, uint32_t count,
                          uint8_t bits, uint16_t *out) {
  for (uint32_t index = 0; index < count; index++) {
    out[index] = (uint16_t)sq_get(column, 0, first + index, bits);
  }
}
void sq_unpack_u16_swar(const uint8_t *column, uint32_t first, uint32_t count,
                        uint8_t bits, uint16_t *out) {
  if (unpack_narrow(column, first, count, bits, out)) return;
  unpack_words(column, first, count, bits, out, spread_four);
}

#if SQ_X86_UNPACK
__attribute__((target("bmi2")))
static uint64_t deposit_four(uint64_t word, uint8_t bits, uint64_t mask) {
  (void)bits;
  // PDEP places the low 4*bits source bits into the selected u16 destinations.
  return _pdep_u64(word, mask * UINT64_C(0x0001000100010001));
}
__attribute__((target("bmi2")))
void sq_unpack_u16_bmi2(const uint8_t *column, uint32_t first, uint32_t count,
                        uint8_t bits, uint16_t *out) {
  if (unpack_narrow(column, first, count, bits, out)) return;
  unpack_words(column, first, count, bits, out, deposit_four);
}

__attribute__((target("avx2")))
static uint64_t vector_four(uint64_t word, uint8_t bits, uint64_t mask) {
  __m256i shifts = _mm256_setr_epi64x(0, bits, 2 * bits, 3 * bits);
  __m256i values = _mm256_srlv_epi64(_mm256_set1_epi64x((long long)word), shifts);
  values = _mm256_and_si256(values, _mm256_set1_epi64x((long long)mask));
  // Each 128-bit half contributes two u16 values. Compact those pairs, then
  // join them in the low 64 bits without scalar lane extraction.
  __m256i shuffle = _mm256_setr_epi8(0, 1, 8, 9, -1, -1, -1, -1,
                                    -1, -1, -1, -1, -1, -1, -1, -1,
                                    0, 1, 8, 9, -1, -1, -1, -1,
                                    -1, -1, -1, -1, -1, -1, -1, -1);
  values = _mm256_shuffle_epi8(values, shuffle);
  __m128i joined = _mm_unpacklo_epi32(_mm256_castsi256_si128(values),
                                     _mm256_extracti128_si256(values, 1));
  return (uint64_t)_mm_cvtsi128_si64(joined);
}
__attribute__((target("avx2")))
void sq_unpack_u16_avx2(const uint8_t *column, uint32_t first, uint32_t count,
                        uint8_t bits, uint16_t *out) {
  if (unpack_narrow(column, first, count, bits, out)) return;
  unpack_words(column, first, count, bits, out, vector_four);
}
#endif

bool sq_unpack_supported(unsigned kernel) {
  if (kernel <= 2) {
    return true;
  }
#if SQ_X86_UNPACK
  if (kernel == 3) {
    return __builtin_cpu_supports("bmi2");
  }
  if (kernel == 4) {
    return __builtin_cpu_supports("avx2");
  }
#endif
  return false;
}
SQUnpack sq_unpack_select(unsigned kernel) {
#if SQ_X86_UNPACK
  // Automatic BMI2 dispatch is limited to the vendor measured by the current
  // experiment. Other hosts retain SWAR; explicit experiments can request BMI2.
  if (!kernel && __builtin_cpu_is("intel") && sq_unpack_supported(3)) {
    return sq_unpack_u16_bmi2;
  }
#endif
  switch (kernel) {
  case 1: return sq_unpack_u16_scalar;
#if SQ_X86_UNPACK
  case 3:
    if (sq_unpack_supported(3)) return sq_unpack_u16_bmi2;
    break;
  case 4:
    if (sq_unpack_supported(4)) return sq_unpack_u16_avx2;
    break;
#endif
  }
  return sq_unpack_u16_swar;
}
