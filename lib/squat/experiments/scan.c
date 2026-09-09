#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <stdio.h>
#include <time.h>
#if defined(__x86_64__) || defined(__i386__)
#include <immintrin.h>
#endif

static double now(void) {
  struct timespec time;
  clock_gettime(CLOCK_MONOTONIC, &time);
  return time.tv_sec + time.tv_nsec * 1e-9;
}

typedef uint64_t (*Count)(const uint64_t *, size_t, uint32_t, uint8_t);

static uint64_t scalar(const uint64_t *data, size_t count, uint32_t target, uint8_t bits) {
  uint64_t mask = (UINT64_C(1) << bits) - 1;
  uint64_t matches = 0;
  for (size_t i = 0; i < count; i++) {
    uint64_t word = data[i];
    for (unsigned lane = 0; lane < 64 / bits; lane++) {
      matches += (word & mask) == target;
      word >>= bits;
    }
  }
  return matches;
}

static inline __attribute__((always_inline)) uint64_t swar_core(const uint64_t *data, size_t count,
                                                                uint32_t target, uint8_t bits) {
  uint64_t starts = sq_lane_starts(bits);
  uint64_t high = starts << (bits - 1);
  uint64_t low = high - starts;
  uint64_t broadcast = starts * target;
  uint64_t matches = 0;
  for (size_t i = 0; i < count; i++) {
    uint64_t difference = data[i] ^ broadcast;
    uint64_t equal = ~(((difference & low) + low) | difference | low) & high;
    matches += (unsigned)__builtin_popcountll(equal);
  }
  return matches;
}

static uint64_t swar(const uint64_t *data, size_t count, uint32_t target, uint8_t bits) {
  return swar_core(data, count, target, bits);
}

#if defined(__x86_64__) || defined(__i386__)
__attribute__((target("popcnt"))) static uint64_t swar_popcnt(const uint64_t *data, size_t count,
                                                              uint32_t target, uint8_t bits) {
  return swar_core(data, count, target, bits);
}

__attribute__((target("avx2,popcnt"))) static uint64_t
compiler_avx2(const uint64_t *data, size_t count, uint32_t target, uint8_t bits) {
  return swar_core(data, count, target, bits);
}
#endif

#if defined(__x86_64__) || defined(__i386__)
__attribute__((target("sse2,popcnt"))) static uint64_t sse2(const uint64_t *data, size_t count,
                                                            uint32_t target, uint8_t bits) {
  uint64_t starts = sq_lane_starts(bits);
  uint64_t high_word = starts << (bits - 1);
  __m128i high = _mm_set1_epi64x((long long)high_word);
  __m128i low = _mm_set1_epi64x((long long)(high_word - starts));
  __m128i broadcast = _mm_set1_epi64x((long long)(starts * target));
  uint64_t matches = 0;
  size_t i = 0;
  for (; i + 2 <= count; i += 2) {
    __m128i difference = _mm_xor_si128(_mm_loadu_si128((const __m128i *)(data + i)), broadcast);
    __m128i nonzero = _mm_or_si128(_mm_add_epi64(_mm_and_si128(difference, low), low),
                                   _mm_or_si128(difference, low));
    __m128i equal = _mm_andnot_si128(nonzero, high);
    matches += (unsigned)__builtin_popcountll((uint64_t)_mm_cvtsi128_si64(equal));
    matches +=
        (unsigned)__builtin_popcountll((uint64_t)_mm_cvtsi128_si64(_mm_srli_si128(equal, 8)));
  }
  return matches + swar(data + i, count - i, target, bits);
}

__attribute__((target("avx2,popcnt"))) static uint64_t avx2(const uint64_t *data, size_t count,
                                                            uint32_t target, uint8_t bits) {
  uint64_t starts = sq_lane_starts(bits);
  uint64_t high_word = starts << (bits - 1);
  __m256i high = _mm256_set1_epi64x((long long)high_word);
  __m256i low = _mm256_set1_epi64x((long long)(high_word - starts));
  __m256i broadcast = _mm256_set1_epi64x((long long)(starts * target));
  uint64_t matches = 0;
  size_t i = 0;
  for (; i + 4 <= count; i += 4) {
    __m256i difference =
        _mm256_xor_si256(_mm256_loadu_si256((const __m256i *)(data + i)), broadcast);
    __m256i nonzero = _mm256_or_si256(_mm256_add_epi64(_mm256_and_si256(difference, low), low),
                                      _mm256_or_si256(difference, low));
    __m256i equal = _mm256_andnot_si256(nonzero, high);
    matches += (unsigned)__builtin_popcountll((uint64_t)_mm256_extract_epi64(equal, 0));
    matches += (unsigned)__builtin_popcountll((uint64_t)_mm256_extract_epi64(equal, 1));
    matches += (unsigned)__builtin_popcountll((uint64_t)_mm256_extract_epi64(equal, 2));
    matches += (unsigned)__builtin_popcountll((uint64_t)_mm256_extract_epi64(equal, 3));
  }
  return matches + swar(data + i, count - i, target, bits);
}
#endif

static int compare_double(const void *left, const void *right) {
  double a = *(const double *)left, b = *(const double *)right;
  return (a > b) - (a < b);
}

int main(void) {
  const size_t count = 131075; // deliberately exercises SIMD tails
  uint64_t *data = malloc(count * sizeof(uint64_t));
  if (!data) {
    return 2;
  }
  uint64_t state = 42;
  for (size_t i = 0; i < count; i++) {
    state = state * UINT64_C(6364136223846793005) + 1;
    data[i] = state;
  }
  struct {
    const char *name;
    Count count;
  } methods[] = {
      {"scalar", scalar},
      {"swar", swar},
#if defined(__x86_64__) || defined(__i386__)
      {"swar-popcnt", __builtin_cpu_supports("popcnt") ? swar_popcnt : NULL},
      {"compiler-avx2",
       __builtin_cpu_supports("avx2") && __builtin_cpu_supports("popcnt") ? compiler_avx2 : NULL},
      {"sse2", __builtin_cpu_supports("sse2") && __builtin_cpu_supports("popcnt") ? sse2 : NULL},
      {"avx2", __builtin_cpu_supports("avx2") && __builtin_cpu_supports("popcnt") ? avx2 : NULL},
#endif
  };
  const uint8_t widths[] = {2, 4, 8, 9, 10, 12, 16};
  puts("width,method,median_ns_per_value,matches");
  for (size_t w = 0; w < sizeof(widths); w++) {
    uint8_t bits = widths[w];
    uint32_t target = 2;
    uint64_t expected = scalar(data, count, target, bits);
    for (size_t m = 0; m < sizeof(methods) / sizeof(methods[0]); m++) {
      if (!methods[m].count) {
        continue;
      }
      // A volatile function pointer prevents loop-invariant call elimination.
      Count volatile operation = methods[m].count;
      double timings[11];
      for (unsigned repeat = 0; repeat < 11; repeat++) {
        double start = now();
        uint64_t result = 0;
        for (unsigned batch = 0; batch < 16; batch++) {
          result += operation(data, count, target, bits);
        }
        timings[repeat] = (now() - start) * 1e9 / (16 * count * (64 / bits));
        if (result != expected * 16) {
          fprintf(stderr, "incorrect %s kernel at width %u\n", methods[m].name, bits);
          free(data);
          return 1;
        }
      }
      qsort(timings, 11, sizeof(double), compare_double);
      printf("%u,%s,%.6f,%llu\n", bits, methods[m].name, timings[5], (unsigned long long)expected);
    }
  }
  free(data);
  return 0;
}
