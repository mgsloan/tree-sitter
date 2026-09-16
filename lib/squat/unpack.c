#include "internal.h"
#if defined(__x86_64__)
#include <immintrin.h>
#define SQ_X86_UNPACK 1
#else
#define SQ_X86_UNPACK 0
#endif

static void coordinates_scalar_arithmetic(const uint8_t *column, uint32_t first, uint32_t count,
                                          uint8_t bits, uint32_t base, bool subtract,
                                          uint32_t *out) {
  for (uint32_t index = 0; index < count; index++) {
    uint32_t delta =
        bits == 8 ? sq_get_u8(column, 0, first + index) : sq_get_u16(column, 0, first + index);
    out[index] = subtract ? base - delta : base + delta;
  }
}

static void coordinates_widen(const uint8_t *column, uint32_t first, uint32_t count, uint8_t bits,
                              uint32_t *out) {
  for (uint32_t index = 0; index < count; index++) {
    out[index] =
        bits == 8 ? sq_get_u8(column, 0, first + index) : sq_get_u16(column, 0, first + index);
  }
}

static void coordinates_scalar(const uint8_t *column, uint32_t first, uint32_t count, uint8_t bits,
                               uint32_t base, bool subtract, uint32_t *out) {
  if (!subtract && !base) {
    coordinates_widen(column, first, count, bits, out);
    return;
  }

  coordinates_scalar_arithmetic(column, first, count, bits, base, subtract, out);
}

#if SQ_X86_UNPACK
static void coordinates_sse2(const uint8_t *column, uint32_t first, uint32_t count, uint8_t bits,
                             uint32_t base, bool subtract, uint32_t *out) {
  const uint8_t *input = column + (size_t)first * (bits / 8);
  __m128i bases = _mm_set1_epi32((int32_t)base);
  __m128i zero = _mm_setzero_si128();
  while (count >= 4) {
    __m128i deltas;
    if (bits == 8) {
      // Load exactly four bytes, including at the end of the allocation.
      uint32_t bytes;
      memcpy(&bytes, input, sizeof(bytes));
      deltas = _mm_unpacklo_epi8(_mm_cvtsi32_si128((int32_t)bytes), zero);
    } else {
      deltas = _mm_loadl_epi64((const __m128i *)input);
    }

    deltas = _mm_unpacklo_epi16(deltas, zero);
    __m128i absolute = subtract ? _mm_sub_epi32(bases, deltas) : _mm_add_epi32(bases, deltas);
    _mm_storeu_si128((__m128i *)out, absolute);
    first += 4;
    input += 4 * (bits / 8);
    out += 4;
    count -= 4;
  }

  // Keep the SSE2 path, including its tail, free of the zero-base branch.
  coordinates_scalar_arithmetic(column, first, count, bits, base, subtract, out);
}

__attribute__((target("avx2"))) static void coordinates_avx2(const uint8_t *column, uint32_t first,
                                                             uint32_t count, uint8_t bits,
                                                             uint32_t base, bool subtract,
                                                             uint32_t *out) {
  const uint8_t *input = column + (size_t)first * (bits / 8);
  if (!subtract && !base) {
    while (count >= 8) {
      __m256i values = bits == 8 ? _mm256_cvtepu8_epi32(_mm_loadl_epi64((const __m128i *)input))
                                 : _mm256_cvtepu16_epi32(_mm_loadu_si128((const __m128i *)input));
      _mm256_storeu_si256((__m256i *)out, values);
      first += 8;
      input += 8 * (bits / 8);
      out += 8;
      count -= 8;
    }

    coordinates_widen(column, first, count, bits, out);
    return;
  }

  __m256i bases = _mm256_set1_epi32((int32_t)base);
  while (count >= 8) {
    // Widen unsigned deltas directly from the slab. No intermediate u16 cache
    // or cross-lane carries: each u32 lane gets its own broadcast-base add/sub.
    __m256i deltas = bits == 8 ? _mm256_cvtepu8_epi32(_mm_loadl_epi64((const __m128i *)input))
                               : _mm256_cvtepu16_epi32(_mm_loadu_si128((const __m128i *)input));
    __m256i absolute = subtract ? _mm256_sub_epi32(bases, deltas) : _mm256_add_epi32(bases, deltas);
    _mm256_storeu_si256((__m256i *)out, absolute);
    first += 8;
    input += 8 * (bits / 8);
    out += 8;
    count -= 8;
  }

  coordinates_sse2(column, first, count, bits, base, subtract, out);
}
#endif

SQUnpackCoordinates sq_unpack_coordinates_select(unsigned kernel) {
#if SQ_X86_UNPACK
  if ((kernel == 0 || kernel == 4) && __builtin_cpu_supports("avx2")) {
    return coordinates_avx2;
  }

  if (kernel != 1) {
    return coordinates_sse2;
  }
#else
  (void)kernel;
#endif
  return coordinates_scalar;
}
