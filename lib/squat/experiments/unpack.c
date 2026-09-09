#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <assert.h>
#include <stdio.h>
#include <time.h>

static double now(void) {
  struct timespec value;
  clock_gettime(CLOCK_MONOTONIC, &value);
  return value.tv_sec + value.tv_nsec * 1e-9;
}
static int compare_double(const void *left, const void *right) {
  double a = *(const double *)left, b = *(const double *)right;
  return (a > b) - (a < b);
}
int main(void) {
  const uint32_t slots = 131072;
  const char *names[] = {"", "scalar", "swar", "bmi2", "avx2"};
  volatile uint64_t checksum = 0;
  puts("group_size,bits,kernel,ns_per_value");
  for (uint8_t bits = 2; bits <= 16; bits++) {
    size_t bytes = (size_t)sq_column_size(slots, bits);
    uint8_t *column = malloc(bytes);
    assert(column);
    memset(column, 0xff, bytes);
    for (uint32_t index = 0; index < slots; index++) {
      sq_set(column, 0, index, bits, (index * 7919u) & ((1u << bits) - 1));
    }
    double timings[5][9] = {{0}};
    for (unsigned kernel = 1; kernel <= 4; kernel++) {
      if (!sq_unpack_supported(kernel)) continue;
      // Volatile indirection prevents hoisting repeated unpack calls. Each
      // kernel writes the same u16 group and consumes every value for checking.
      SQUnpack volatile unpack = sq_unpack_select(kernel);
      uint16_t values[SQ_GROUP_SIZE];
      for (uint32_t first = 0; first < slots; first += SQ_GROUP_SIZE) {
        unpack(column, first, SQ_GROUP_SIZE, bits, values);
        for (unsigned lane = 0; lane < SQ_GROUP_SIZE; lane++) {
          assert(values[lane] == sq_get(column, 0, first + lane, bits));
        }
      }
    }
    // Rotate kernel order on each repeat to reduce clock/temperature bias.
    for (unsigned repeat = 0; repeat < 9; repeat++) {
      for (unsigned step = 0; step < 4; step++) {
        unsigned kernel = 1 + (repeat + step) % 4;
        if (!sq_unpack_supported(kernel)) continue;
        SQUnpack volatile unpack = sq_unpack_select(kernel);
        uint16_t values[SQ_GROUP_SIZE];
        double start = now();
        uint64_t sum = 0;
        for (unsigned batch = 0; batch < 8; batch++) {
          for (uint32_t first = 0; first < slots; first += SQ_GROUP_SIZE) {
            unpack(column, first, SQ_GROUP_SIZE, bits, values);
            sum += values[0];
          }
        }
        checksum += sum;
        timings[kernel][repeat] = (now() - start) * 1e9 / (8 * slots);
      }
    }
    for (unsigned kernel = 1; kernel <= 4; kernel++) {
      if (!sq_unpack_supported(kernel)) continue;
      qsort(timings[kernel], 9, sizeof(double), compare_double);
      printf("%u,%u,%s,%.6f\n", SQ_GROUP_SIZE, bits, names[kernel], timings[kernel][4]);
    }
    free(column);
  }
  fprintf(stderr, "checksum: %llu\n", (unsigned long long)checksum);
}
