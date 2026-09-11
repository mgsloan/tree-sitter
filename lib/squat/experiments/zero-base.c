#define _POSIX_C_SOURCE 200809L
#include "../internal.h"
#include <assert.h>
#include <dlfcn.h>
#include <immintrin.h>
#include <stdio.h>
#include <time.h>

// Private columns preserve the production base +/- delta encoding. Only
// additive columns are rebased; end columns retain their original encoding.
typedef struct {
  uint8_t *delta[2], *live;
  uint32_t *base[2], *absolute;
  uint32_t groups, nodes, eligible, already_zero;
  bool subtract;
} Column;

typedef void (*Decode)(const uint8_t *, uint32_t, bool, uint32_t *);
typedef uint32_t (*Read)(const uint8_t *, const uint32_t *, uint32_t, bool);
#define NOIPA __attribute__((noipa))
#define SCALAR __attribute__((optimize("no-tree-vectorize")))

// Separate functions keep the comparison free of a per-call mode switch.
// Scalar functions explicitly disable autovectorization.
#define READ(NAME, SKIP, SUBTRACT)                                                                 \
  static NOIPA SCALAR uint32_t NAME(const uint8_t *delta, const uint32_t *bases, uint32_t slot,    \
                                    bool subtract) {                                               \
    (void)subtract;                                                                                \
    uint32_t base = bases[slot / SQ_GROUP_SIZE];                                                   \
    uint32_t value = delta[slot];                                                                  \
    if (SKIP && !base) return value;                                                               \
    return SUBTRACT ? base - value : base + value;                                                 \
  }

READ(read_add, 0, 0)
READ(read_add_skip, 1, 0)
READ(read_sub, 0, 1)
READ(read_sub_skip, 1, 1)
static Read readers[2][2] = {{read_add, read_add_skip}, {read_sub, read_sub_skip}};

#define SCALAR_DECODE(NAME, SKIP)                                                                  \
  static NOIPA SCALAR void NAME(const uint8_t *delta, uint32_t base, bool subtract,                \
                                uint32_t *out) {                                                   \
    if (SKIP && !base) {                                                                           \
      for (unsigned i = 0; i < SQ_GROUP_SIZE; i++) out[i] = delta[i];                              \
    } else if (subtract) {                                                                         \
      for (unsigned i = 0; i < SQ_GROUP_SIZE; i++) out[i] = base - delta[i];                       \
    } else {                                                                                       \
      for (unsigned i = 0; i < SQ_GROUP_SIZE; i++) out[i] = base + delta[i];                       \
    }                                                                                              \
  }

SCALAR_DECODE(scalar_base, 0)
SCALAR_DECODE(scalar_skip, 1)

#define SSE_DECODE(NAME, SKIP)                                                                     \
  static NOIPA void NAME(const uint8_t *delta, uint32_t base, bool subtract, uint32_t *out) {      \
    __m128i bases = _mm_set1_epi32((int32_t)base);                                                 \
    for (unsigned i = 0; i < SQ_GROUP_SIZE; i += 4) {                                              \
      uint32_t bytes;                                                                              \
      memcpy(&bytes, delta + i, 4);                                                                \
      __m128i values = _mm_unpacklo_epi8(_mm_cvtsi32_si128((int32_t)bytes), _mm_setzero_si128());  \
      values = _mm_unpacklo_epi16(values, _mm_setzero_si128());                                    \
      if (!(SKIP && !base))                                                                        \
        values = subtract ? _mm_sub_epi32(bases, values) : _mm_add_epi32(bases, values);           \
      _mm_storeu_si128((__m128i *)(out + i), values);                                              \
    }                                                                                              \
  }

SSE_DECODE(sse_base, 0)
SSE_DECODE(sse_skip, 1)

#define AVX_DECODE(NAME, SKIP)                                                                     \
  static NOIPA __attribute__((target("avx2"))) void NAME(const uint8_t *delta, uint32_t base,      \
                                                         bool subtract, uint32_t *out) {           \
    __m256i bases = _mm256_set1_epi32((int32_t)base);                                              \
    for (unsigned i = 0; i < SQ_GROUP_SIZE; i += 8) {                                              \
      __m256i values = _mm256_cvtepu8_epi32(_mm_loadl_epi64((const __m128i *)(delta + i)));        \
      if (!(SKIP && !base))                                                                        \
        values = subtract ? _mm256_sub_epi32(bases, values) : _mm256_add_epi32(bases, values);     \
      _mm256_storeu_si256((__m256i *)(out + i), values);                                           \
    }                                                                                              \
  }

AVX_DECODE(avx_base, 0)
AVX_DECODE(avx_skip, 1)

static Decode decoders[3][2] = {
    {scalar_base, scalar_skip}, {sse_base, sse_skip}, {avx_base, avx_skip}};
static const char *kernels[] = {"scalar-node", "scalar-group", "sse2", "avx2"};
static const char *variants[] = {"baseline", "existing-skip", "zero-add", "zero-skip"};

static double now(void) {
  struct timespec value;
  clock_gettime(CLOCK_MONOTONIC, &value);
  return value.tv_sec + value.tv_nsec * 1e-9;
}

static Column column_new(uint32_t groups, bool subtract) {
  Column c = {.groups = groups, .subtract = subtract};
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
      uint32_t value = c->subtract ? c->base[0][g] - c->delta[0][first + i]
                                   : c->base[0][g] + c->delta[0][first + i];
      c->absolute[first + i] = value;
      if (value > max) max = value;
    }

    bool fits = !c->subtract && max <= UINT8_MAX;
    c->eligible += fits;
    c->already_zero += c->base[0][g] == 0;
    c->nodes += c->live[g];
    c->base[1][g] = fits ? 0 : c->base[0][g];
    for (unsigned i = 0; i < c->live[g]; i++)
      c->delta[1][first + i] = fits ? c->absolute[first + i] : c->delta[0][first + i];
  }
}

static void validate(const Column *c) {
  for (unsigned kernel = 0; kernel < 4; kernel++) {
    if (kernel == 3 && !__builtin_cpu_supports("avx2")) continue;
    for (unsigned variant = 0; variant < 4; variant++) {
      unsigned v = variant / 2, skip = variant % 2;
      for (uint32_t g = 0; g < c->groups; g++) {
        uint32_t values[SQ_GROUP_SIZE], first = g * SQ_GROUP_SIZE;
        bool sub = c->subtract;
        if (kernel) decoders[kernel - 1][skip](c->delta[v] + first, c->base[v][g], sub, values);
        else {
          Read read = readers[c->subtract][skip];
          for (unsigned i = 0; i < c->live[g]; i++)
            values[i] = read(c->delta[v], c->base[v], first + i, c->subtract);
        }

        for (unsigned i = 0; i < c->live[g]; i++) assert(values[i] == c->absolute[first + i]);
      }
    }
  }
}

static NOIPA uint64_t walk(const Column *c, unsigned kernel, unsigned variant, unsigned batches) {
  unsigned v = variant / 2, skip = variant % 2;
  Read read = readers[c->subtract][skip];
  Decode decode = kernel ? decoders[kernel - 1][skip] : NULL;
  uint64_t sum = 0;
  for (unsigned batch = 0; batch < batches; batch++) {
    for (uint32_t group = c->groups; group;) {
      uint32_t g = --group, first = g * SQ_GROUP_SIZE;
      if (kernel) {
        uint32_t values[SQ_GROUP_SIZE];
        uint32_t base = c->base[v][g];
        bool sub = c->subtract;
        decode(c->delta[v] + first, base, sub, values);
        sum += values[(g + batch) % c->live[g]];
      } else {
        for (unsigned i = c->live[g]; i;)
          sum += read(c->delta[v], c->base[v], first + --i, c->subtract);
      }
    }
  }

  return sum;
}

static volatile uint64_t sink;
static void benchmark(Column *c, const char *name, unsigned repeats) {
  validate(c);
  for (unsigned kernel = 0; kernel < 4; kernel++) {
    if (kernel == 3 && !__builtin_cpu_supports("avx2")) continue;
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
               repeat, batches, c->groups, c->nodes, c->eligible, c->already_zero, elapsed,
               (unsigned long long)sum);
      }
    }
  }
}

static void boundary_checks(void) {
  for (unsigned subtract = 0; subtract < 2; subtract++) {
    Column c = column_new(6, subtract);
    const uint32_t bases[] = {0, 240, 255, 256, 0x80000100u, UINT32_MAX - 16};
    for (unsigned g = 0; g < c.groups; g++) {
      c.live[g] = g + 1;
      c.base[0][g] = bases[g];
      for (unsigned i = 0; i < c.live[g]; i++) c.delta[0][g * SQ_GROUP_SIZE + i] = i;
    }

    rebase(&c);
    validate(&c);
    column_delete(&c);
  }
}

int main(int argc, char **argv) {
  boundary_checks();
  if (argc == 2 && !strcmp(argv[1], "--check")) return 0;
  if (argc != 5) {
    fprintf(stderr, "usage: zero-base LIBRARY SYMBOL SOURCE REPEATS\n");
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
  const uint32_t bases[] = {tree->layout.span_base, tree->layout.start_column_base,
                            tree->layout.end_column_base};
  const uint32_t deltas[] = {tree->layout.span_delta, tree->layout.start_column_delta,
                             tree->layout.end_column_delta};
  const char *names[] = {"span", "start_column", "end_column"};
  puts("column,kernel,variant,repeat,batches,groups,nodes,eligible,already_zero,seconds,checksum");
  for (unsigned col = 0; col < 3; col++) {
    Column c = column_new(sq_header(tree)->group_count, col == 2);
    for (uint32_t g = 0; g < c.groups; g++) {
      c.live[g] = SQ_GROUP_SIZE - sq_group_waste(tree, g);
      c.base[0][g] = sq_get_u32(tree->data, bases[col], g);
      memcpy(c.delta[0] + g * SQ_GROUP_SIZE, tree->data + deltas[col] + g * SQ_GROUP_SIZE,
             SQ_GROUP_SIZE);
    }

    rebase(&c);
    benchmark(&c, names[col], repeats);
    column_delete(&c);
  }

  fprintf(stderr, "checksum: %llu\n", (unsigned long long)sink);
  sq_tree_delete(tree);
  ts_tree_delete(parsed);
  ts_parser_delete(parser);
  free(source);
  dlclose(library);
}
