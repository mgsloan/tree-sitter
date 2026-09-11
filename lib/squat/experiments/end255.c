#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <assert.h>
#include <dlfcn.h>
#include <immintrin.h>
#include <stdio.h>
#include <time.h>

#if !SQ_INCLUDE_POINTS
#error "The end-column probe requires point positions"
#endif

// Private copies preserve end = base - u8_delta. Eligible groups choose
// base 255; no new encoding, widths, flags, or group boundaries are introduced.
_Static_assert(SQ_GROUP_SIZE == 16, "this probe compares fixed 16-slot decoders");
#ifdef NDEBUG
#error "The experiment requires its correctness assertions"
#endif

typedef struct {
  uint8_t *delta[2], *live;
  uint32_t *base[2], *absolute;
  uint32_t groups, nodes, eligible, already_255;
} Column;

typedef void (*Decode)(const uint8_t *, uint32_t, uint32_t *);
typedef uint32_t (*Read)(const uint8_t *, const uint32_t *, uint32_t);
#define NOIPA __attribute__((noipa))
#define SCALAR __attribute__((optimize("no-tree-vectorize")))

static NOIPA SCALAR uint32_t read_base(const uint8_t *d, const uint32_t *b, uint32_t slot) {
  return b[slot / SQ_GROUP_SIZE] - d[slot];
}

static NOIPA SCALAR uint32_t read_complement(const uint8_t *d, const uint32_t *b, uint32_t slot) {
  uint32_t base = b[slot / SQ_GROUP_SIZE];
  return base == 255 ? (d[slot] ^ 255u) : base - d[slot];
}

static NOIPA SCALAR void scalar_base(const uint8_t *d, uint32_t base, uint32_t *out) {
  for (unsigned i = 0; i < SQ_GROUP_SIZE; i++) out[i] = base - d[i];
}

static NOIPA SCALAR void scalar_complement(const uint8_t *d, uint32_t base, uint32_t *out) {
  if (base == 255) {
    for (unsigned i = 0; i < SQ_GROUP_SIZE; i++) out[i] = (d[i] ^ 255u);
  } else {
    for (unsigned i = 0; i < SQ_GROUP_SIZE; i++) out[i] = base - d[i];
  }
}

static inline void sse_subtract(const uint8_t *d, uint32_t base, uint32_t *out) {
  __m128i bases = _mm_set1_epi32((int32_t)base), zero = _mm_setzero_si128();
  for (unsigned i = 0; i < SQ_GROUP_SIZE; i += 4) {
    uint32_t bytes;
    memcpy(&bytes, d + i, 4);
    __m128i values = _mm_unpacklo_epi8(_mm_cvtsi32_si128((int32_t)bytes), zero);
    values = _mm_unpacklo_epi16(values, zero);
    _mm_storeu_si128((__m128i *)(out + i), _mm_sub_epi32(bases, values));
  }
}

static NOIPA void sse_base(const uint8_t *d, uint32_t base, uint32_t *out) {
  sse_subtract(d, base, out);
}

static NOIPA void sse_complement(const uint8_t *d, uint32_t base, uint32_t *out) {
  if (base == 255) {
    __m128i bytes = _mm_xor_si128(_mm_loadu_si128((const __m128i *)d), _mm_set1_epi8(-1));
    __m128i zero = _mm_setzero_si128();
    __m128i low = _mm_unpacklo_epi8(bytes, zero), high = _mm_unpackhi_epi8(bytes, zero);
    _mm_storeu_si128((__m128i *)out, _mm_unpacklo_epi16(low, zero));
    _mm_storeu_si128((__m128i *)(out + 4), _mm_unpackhi_epi16(low, zero));
    _mm_storeu_si128((__m128i *)(out + 8), _mm_unpacklo_epi16(high, zero));
    _mm_storeu_si128((__m128i *)(out + 12), _mm_unpackhi_epi16(high, zero));
  } else {
    sse_subtract(d, base, out);
  }
}

static inline void sse_subtract_sixteen(const uint8_t *d, uint32_t base, uint32_t *out) {
  __m128i bytes = _mm_loadu_si128((const __m128i *)d), zero = _mm_setzero_si128();
  __m128i low = _mm_unpacklo_epi8(bytes, zero), high = _mm_unpackhi_epi8(bytes, zero);
  __m128i bases = _mm_set1_epi32((int32_t)base);
  _mm_storeu_si128((__m128i *)out, _mm_sub_epi32(bases, _mm_unpacklo_epi16(low, zero)));
  _mm_storeu_si128((__m128i *)(out + 4), _mm_sub_epi32(bases, _mm_unpackhi_epi16(low, zero)));
  _mm_storeu_si128((__m128i *)(out + 8), _mm_sub_epi32(bases, _mm_unpacklo_epi16(high, zero)));
  _mm_storeu_si128((__m128i *)(out + 12), _mm_sub_epi32(bases, _mm_unpackhi_epi16(high, zero)));
}

static NOIPA void sse_sixteen_base(const uint8_t *d, uint32_t base, uint32_t *out) {
  sse_subtract_sixteen(d, base, out);
}

static NOIPA void sse_sixteen_complement(const uint8_t *d, uint32_t base, uint32_t *out) {
  if (base == 255) {
    __m128i bytes = _mm_xor_si128(_mm_loadu_si128((const __m128i *)d), _mm_set1_epi8(-1));
    __m128i zero = _mm_setzero_si128();
    __m128i low = _mm_unpacklo_epi8(bytes, zero), high = _mm_unpackhi_epi8(bytes, zero);
    _mm_storeu_si128((__m128i *)out, _mm_unpacklo_epi16(low, zero));
    _mm_storeu_si128((__m128i *)(out + 4), _mm_unpackhi_epi16(low, zero));
    _mm_storeu_si128((__m128i *)(out + 8), _mm_unpacklo_epi16(high, zero));
    _mm_storeu_si128((__m128i *)(out + 12), _mm_unpackhi_epi16(high, zero));
  } else {
    sse_subtract_sixteen(d, base, out);
  }
}

__attribute__((target("avx2"))) static inline void avx_subtract(const uint8_t *d, uint32_t base,
                                                                uint32_t *out) {
  __m256i bases = _mm256_set1_epi32((int32_t)base);
  for (unsigned i = 0; i < SQ_GROUP_SIZE; i += 8) {
    __m256i values = _mm256_cvtepu8_epi32(_mm_loadl_epi64((const __m128i *)(d + i)));
    _mm256_storeu_si256((__m256i *)(out + i), _mm256_sub_epi32(bases, values));
  }
}

static NOIPA __attribute__((target("avx2"))) void avx_base(const uint8_t *d, uint32_t base,
                                                           uint32_t *out) {
  avx_subtract(d, base, out);
}

static NOIPA __attribute__((target("avx2"))) void
avx_eight_complement(const uint8_t *d, uint32_t base, uint32_t *out) {
  if (base == 255) {
    for (unsigned i = 0; i < SQ_GROUP_SIZE; i += 8) {
      __m128i bytes = _mm_xor_si128(_mm_loadl_epi64((const __m128i *)(d + i)), _mm_set1_epi8(-1));
      _mm256_storeu_si256((__m256i *)(out + i), _mm256_cvtepu8_epi32(bytes));
    }
  } else {
    avx_subtract(d, base, out);
  }
}

static NOIPA __attribute__((target("avx2"))) void
avx_sixteen_complement(const uint8_t *d, uint32_t base, uint32_t *out) {
  if (base == 255) {
    // One exact 16-byte load and complement covers the whole physical group.
    __m128i bytes = _mm_xor_si128(_mm_loadu_si128((const __m128i *)d), _mm_set1_epi8(-1));
    _mm256_storeu_si256((__m256i *)out, _mm256_cvtepu8_epi32(bytes));
    _mm256_storeu_si256((__m256i *)(out + 8), _mm256_cvtepu8_epi32(_mm_srli_si128(bytes, 8)));
  } else {
    avx_subtract(d, base, out);
  }
}

static Decode decoders[5][2] = {{scalar_base, scalar_complement},
                                {sse_base, sse_complement},
                                {sse_sixteen_base, sse_sixteen_complement},
                                {avx_base, avx_eight_complement},
                                {avx_base, avx_sixteen_complement}};
static const char *kernels[] = {"scalar-node",  "scalar-group", "sse2-four",
                                "sse2-sixteen", "avx2-eight",   "avx2-sixteen"};
static const char *variants[] = {"baseline", "existing-complement", "fixed-subtract",
                                 "fixed-complement"};

static double now(void) {
  struct timespec value;
  clock_gettime(CLOCK_MONOTONIC, &value);
  return value.tv_sec + value.tv_nsec * 1e-9;
}

static Column column_new(uint32_t groups) {
  Column c = {.groups = groups};
  c.live = calloc(groups, 1);
  c.absolute = calloc((size_t)groups * SQ_GROUP_SIZE, 4);
  assert(c.live && c.absolute);
  for (unsigned v = 0; v < 2; v++) {
    c.delta[v] = calloc((size_t)groups * SQ_GROUP_SIZE, 1);
    c.base[v] = calloc(groups, 4);
    assert(c.delta[v] && c.base[v]);
  }

  return c;
}

static void column_delete(Column *c) {
  free(c->live);
  free(c->absolute);
  for (unsigned v = 0; v < 2; v++) {
    free(c->delta[v]);
    free(c->base[v]);
  }
}

static void rebase(Column *c) {
  for (uint32_t g = 0; g < c->groups; g++) {
    uint32_t first = g * SQ_GROUP_SIZE, max = 0;
    for (unsigned i = 0; i < c->live[g]; i++) {
      uint32_t value = c->base[0][g] - c->delta[0][first + i];
      c->absolute[first + i] = value;
      if (value > max) max = value;
    }

    bool fits = max <= UINT8_MAX;
    c->eligible += fits;
    c->already_255 += c->base[0][g] == 255;
    c->nodes += c->live[g];
    c->base[1][g] = fits ? 255 : c->base[0][g];
    for (unsigned i = 0; i < c->live[g]; i++)
      c->delta[1][first + i] = fits ? 255 - c->absolute[first + i] : c->delta[0][first + i];
  }
}

static void validate(const Column *c) {
  for (unsigned kernel = 0; kernel < 6; kernel++) {
    if (kernel >= 4 && !__builtin_cpu_supports("avx2")) continue;
    for (unsigned variant = 0; variant < 4; variant++) {
      unsigned v = variant / 2, skip = variant % 2;
      for (uint32_t g = 0; g < c->groups; g++) {
        uint32_t values[SQ_GROUP_SIZE], first = g * SQ_GROUP_SIZE;
        if (kernel) decoders[kernel - 1][skip](c->delta[v] + first, c->base[v][g], values);
        else {
          Read read = skip ? read_complement : read_base;
          for (unsigned i = 0; i < c->live[g]; i++)
            values[i] = read(c->delta[v], c->base[v], first + i);
        }

        for (unsigned i = 0; i < c->live[g]; i++) assert(values[i] == c->absolute[first + i]);
      }
    }
  }
}

static NOIPA uint64_t walk(const Column *c, unsigned kernel, unsigned variant, unsigned batches) {
  unsigned v = variant / 2, skip = variant % 2;
  Read read = skip ? read_complement : read_base;
  Decode decode = kernel ? decoders[kernel - 1][skip] : NULL;
  uint64_t sum = 0;
  for (unsigned batch = 0; batch < batches; batch++) {
    for (uint32_t group = c->groups; group;) {
      uint32_t g = --group, first = g * SQ_GROUP_SIZE;
      if (kernel) {
        uint32_t values[SQ_GROUP_SIZE];
        uint32_t base = c->base[v][g];
        decode(c->delta[v] + first, base, values);
        sum += values[(g + batch) % c->live[g]];
      } else {
        for (unsigned i = c->live[g]; i;) sum += read(c->delta[v], c->base[v], first + --i);
      }
    }
  }

  return sum;
}

static volatile uint64_t sink;
static void benchmark(Column *c, const char *name, unsigned repeats) {
  validate(c);
  for (unsigned kernel = 0; kernel < 6; kernel++) {
    if (kernel >= 4 && !__builtin_cpu_supports("avx2")) continue;
    unsigned batches = 1;
    for (;;) {
      double start = now();
      sink += walk(c, kernel, 0, batches);
      if (now() - start >= 0.004 || batches >= (1u << 20)) break;
      batches *= 2;
    }

    for (unsigned repeat = 0; repeat < repeats; repeat++) {
      // Rotate and reverse blocks: each variant occupies each position equally.
      for (unsigned step = 0; step < 4; step++) {
        unsigned variant = (repeat / 4 % 2 ? 3 - step : step);
        variant = (variant + repeat) % 4;
        double start = now();
        uint64_t sum = walk(c, kernel, variant, batches);
        double elapsed = now() - start;
        sink += sum;
        printf("%s,%s,%s,%u,%u,%u,%u,%u,%u,%.9f,%llu\n", name, kernels[kernel], variants[variant],
               repeat, batches, c->groups, c->nodes, c->eligible, c->already_255, elapsed,
               (unsigned long long)sum);
      }
    }
  }
}

static void boundary_checks(void) {
  // Exact allocations catch a full-group load past the end; every byte value is
  // checked at base 255. Also exercise zero, 256, high-bit bases, and partial groups.
  Column c = column_new(20);
  for (unsigned g = 4; g < 20; g++) {
    c.live[g] = 16;
    c.base[0][g] = 255;
    for (unsigned i = 0; i < 16; i++) c.delta[0][g * 16 + i] = (g - 4) * 16 + i;
  }

  const uint32_t bases[] = {0, 256, 0x80000100u, UINT32_MAX};
  for (unsigned g = 0; g < 4; g++) {
    c.live[g] = g + 1;
    c.base[0][g] = bases[g];
    for (unsigned i = 0; i < c.live[g]; i++) c.delta[0][g * 16 + i] = i;
  }

  rebase(&c);
  validate(&c);
  column_delete(&c);
}

int main(int argc, char **argv) {
  boundary_checks();
  if (argc == 2 && !strcmp(argv[1], "--check")) return 0;
  if (argc != 5) {
    fprintf(stderr, "usage: end255 LIBRARY SYMBOL SOURCE REPEATS\n");
    return 2;
  }

  unsigned repeats = (unsigned)strtoul(argv[4], NULL, 10);
  assert(repeats && repeats % 8 == 0);
  void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!library) {
    fprintf(stderr, "%s\n", dlerror());
    return 2;
  }

  const TSLanguage *(*language)(void) = (const TSLanguage *(*)(void))dlsym(library, argv[2]);
  assert(language);
  TSParser *parser = ts_parser_new();
  assert(ts_parser_set_language(parser, language()));
  FILE *file = fopen(argv[3], "rb");
  assert(file && !fseek(file, 0, SEEK_END));
  long length = ftell(file);
  assert(length >= 0 && length <= 16 * 1024 * 1024);
  rewind(file);
  char *source = malloc((size_t)length + 1);
  assert(source && fread(source, 1, (size_t)length, file) == (size_t)length);
  fclose(file);
  TSTree *parsed = ts_parser_parse_string(parser, NULL, source, (uint32_t)length);
  assert(parsed);
  SQError error;
  SQTree *tree = sq_tree_pack(parsed, sq_pack_options_default(), &error);
  assert(tree);
  puts("column,kernel,variant,repeat,batches,groups,nodes,eligible,already_255,seconds,checksum");
  {
    Column c = column_new(sq_header(tree)->group_count);
    for (uint32_t g = 0; g < c.groups; g++) {
      c.live[g] = SQ_GROUP_SIZE - sq_group_waste(tree, g);
      c.base[0][g] = (uint32_t)sq_group_end_point_base(tree, g);
      for (uint32_t lane = 0; lane < c.live[g]; lane++) {
        SQNode node = {tree, g * SQ_GROUP_SIZE + lane};
        c.delta[0][g * SQ_GROUP_SIZE + lane] = (uint8_t)sq_node_end_point_key(node);
      }
    }

    rebase(&c);
    benchmark(&c, "end_column", repeats);
    column_delete(&c);
  }

  fprintf(stderr, "checksum: %llu\n", (unsigned long long)sink);
  sq_tree_delete(tree);
  ts_tree_delete(parsed);
  ts_parser_delete(parser);
  free(source);
  dlclose(library);
}
