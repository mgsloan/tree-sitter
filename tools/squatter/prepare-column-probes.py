#!/usr/bin/env python3
"""Independently vary symbol, field-ID, and supertype storage in frozen sources.

The baseline includes the already committed grammar-wide supertype dictionary.
No live runtime files or unrelated working-tree changes enter these builds.
The optional superpow2 byte-reader probe requires a little-endian target.
"""
import importlib.util
from pathlib import Path

spec = importlib.util.spec_from_file_location('kernel_probes', Path(__file__).with_name('prepare-kernel-probes.py'))
kernel = importlib.util.module_from_spec(spec)
spec.loader.exec_module(kernel)
kernel.REVISION = 'ab6162834'
POLICIES = {
    'exact': (0, 0, 0), 'control': (0, 0, 0),
    'field2': (2, 0, 0), 'field5': (5, 0, 0), 'field6': (6, 0, 0),
    'symbol5': (0, 5, 0), 'symbol6': (0, 6, 0),
    'symbol9': (0, 9, 0), 'symbol10': (0, 10, 0),
    'fieldonly': (20, 0, 0), 'symbolonly': (0, 20, 0), 'both': (20, 20, 0),
    'super': (0, 0, 1), 'superpow2': (0, 0, 2),
}
kernel.VARIANTS = list(POLICIES)


class Columns:
    output = 'build/column-probes'

    def flags(self, variant):
        field, symbol, supertype = POLICIES[variant]
        return f'-DSQ_FIELD_ROUND={field} -DSQ_SYMBOL_ROUND={symbol} -DSQ_SUPER_VARIABLE={supertype}'

    def apply(self, source, variant, patches):
        def header(s):
            start = s.index('#define SQ_VERSION ')
            end = s.index('\n\n', start)
            s = s[:start] + '''#ifndef SQ_FIELD_ROUND
#define SQ_FIELD_ROUND 0
#endif
#ifndef SQ_SYMBOL_ROUND
#define SQ_SYMBOL_ROUND 0
#endif
#ifndef SQ_SUPER_VARIABLE
#define SQ_SUPER_VARIABLE 0
#endif
// Experimental formats encode the column policy and the actual supertype
// width. Slabs from another policy cannot silently pass a load check.
#define SQ_SUPER_BITS_MASK (UINT32_C(31) << 12)
#define SQ_SUPER_HEADER_BITS(flags) (((flags) >> 12) & 31u)
#define SQ_VERSION (UINT32_C(0xC8000090) | ((uint32_t)SQ_FIELD_ROUND << 17) | \\
                    ((uint32_t)SQ_SYMBOL_ROUND << 22) | ((uint32_t)SQ_SUPER_VARIABLE << 5))''' + s[end:]
            s = s.replace('uint8_t symbol_lanes, field_lanes;', 'uint8_t symbol_lanes, field_lanes, supertype_lanes;')
            s = s.replace('uint32_t symbol_mask, field_mask;', 'uint32_t symbol_mask, field_mask, supertype_mask;')
            s = s.replace('bool wide_supertypes, SQLayout *', 'uint8_t supertype_bits, SQLayout *')
            s = s.replace('uint8_t sq_width(uint32_t max);', 'uint8_t sq_width(uint32_t max);\nuint8_t sq_probe_width(uint32_t max, bool field);')
            a = s.index('static inline uint32_t sq_node_supertype(')
            b = s.index('\n}\n', a) + 3
            s = s[:a] + '''static inline uint32_t sq_node_supertype(SQNode node) {
#if SQ_SUPER_VARIABLE
  uint8_t bits = node.tree->layout.supertype_bits;
  if (!bits) return 0;
  if (bits == 1) return sq_get_bit(node.tree->data, node.tree->layout.supertype, node.slot);
#if SQ_SUPER_VARIABLE == 2
  if (bits == 2) return (sq_get_u8(node.tree->data, node.tree->layout.supertype, node.slot / 4) >> ((node.slot % 4) * 2)) & 3u;
  if (bits == 4) return (sq_get_u8(node.tree->data, node.tree->layout.supertype, node.slot / 2) >> ((node.slot % 2) * 4)) & 15u;
#endif
  return sq_get_packed_cached(node.tree->data, node.tree->layout.supertype, node.slot,
                              bits, node.tree->layout.supertype_lanes, node.tree->layout.supertype_mask);
#else
  return node.tree->layout.supertype_bits == 16
      ? sq_get_u16(node.tree->data, node.tree->layout.supertype, node.slot)
      : sq_get_u8(node.tree->data, node.tree->layout.supertype, node.slot);
#endif
}
''' + s[b:]
            return s
        kernel.edit(source, 'lib/squat/internal.h', header, patches)

        def slab(s):
            marker = 'uint64_t sq_column_size('
            s = s.replace(marker, '''uint8_t sq_probe_width(uint32_t max, bool field) {
  uint8_t bits = sq_width(max);
  unsigned policy = field ? SQ_FIELD_ROUND : SQ_SYMBOL_ROUND;
  if (policy == bits) return bits <= 8 ? 8 : 16;
  if (policy == 20 && ((field && bits >= 6 && bits < 8) || (!field && bits >= 9 && bits < 16)))
    return field ? 8 : 16;
  return bits;
}

static uint8_t probe_supertype_bits(uint32_t count, uint32_t dictionary_count) {
  if (!SQ_SUPER_VARIABLE) return dictionary_count > 256 ? 16 : 8;
  if (count <= 8) return (uint8_t)count; // Direct masks: one bit per supertype; omit an all-zero column.
  uint8_t bits = 0;
  for (uint32_t max = dictionary_count - 1; max; max >>= 1) bits++;
  return bits; // Dense dictionary IDs need ceil(log2(dictionary_count)) bits.
}

''' + marker)
            s = s.replace('uint64_t sq_column_size(uint32_t count, uint8_t bits) {', 'uint64_t sq_column_size(uint32_t count, uint8_t bits) {\n  if (!bits) return 0;')
            s = s.replace('bool wide_supertypes, SQLayout *layout)', 'uint8_t supertype_bits, SQLayout *layout)')
            s = s.replace('  layout->supertype_bits = wide_supertypes ? 16 : 8;', '''  if (supertype_bits > 16) return false;
  layout->supertype_bits = supertype_bits;
  layout->supertype_lanes = supertype_bits ? 64 / supertype_bits : 0;
  layout->supertype_mask = (uint32_t)((UINT64_C(1) << supertype_bits) - 1);''')
            s = s.replace('sq_width(language->symbol_count + language->alias_count + 1)', 'sq_probe_width(language->symbol_count + language->alias_count + 1, false)')
            s = s.replace('sq_width(language->field_count)', 'sq_probe_width(language->field_count, true)')
            s = s.replace('sq_array_size(slots, layout->supertype_bits / 8)', 'sq_column_size(slots, layout->supertype_bits)')
            s = s.replace('grammar && grammar->count > 256, &layout', 'probe_supertype_bits(supertype_count, grammar ? grammar->count : 0), &layout')
            s = s.replace('(tree->layout.supertype_bits == 16 ? SQ_WIDE_SUPERTYPES : 0)', '''((uint32_t)tree->layout.supertype_bits << 12) |
                         (tree->supertype_grammar && tree->supertype_grammar->count > 256 ? SQ_WIDE_SUPERTYPES : 0)''')
            s = s.replace('tree->layout.supertype_bits == 16, &next', 'tree->layout.supertype_bits, &next')
            s = s.replace('sq_array_size(slots, next.supertype_bits / 8)', 'sq_column_size(slots, next.supertype_bits)')
            return s
        kernel.edit(source, 'lib/squat/slab.c', slab, patches)
        kernel.edit(source, 'lib/squat/query_plan.c', lambda s: s.replace(
            'sq_width(query->language->symbol_count + query->language->alias_count + 1)',
            'sq_probe_width(query->language->symbol_count + query->language->alias_count + 1, false)'), patches)
        kernel.edit(source, 'lib/squat/index.c', lambda s: s.replace(
            '~(SQ_PRESENCE | SQ_WIDE_SUPERTYPES | SQ_GRAMMAR_OVERRIDES)',
            '~(SQ_PRESENCE | SQ_WIDE_SUPERTYPES | SQ_GRAMMAR_OVERRIDES | SQ_SUPER_BITS_MASK)').replace(
            '(header.format_flags & SQ_WIDE_SUPERTYPES) != 0, &layout', 'SQ_SUPER_HEADER_BITS(header.format_flags), &layout').replace(
            'if (header.supertype_dictionary_count != dictionary_count ||',
            'if (layout.supertype_bits != tree->layout.supertype_bits ||\n      header.supertype_dictionary_count != dictionary_count ||'), patches)
        kernel.edit(source, 'lib/squat/pack.c', lambda s: s.replace(
            'bool wide_supertypes = tree->layout.supertype_bits == 16;', 'uint8_t supertype_bits = tree->layout.supertype_bits;').replace(
            '    if (wide_supertypes) {', '''    if (SQ_SUPER_VARIABLE) {
      if (supertype_bits) sq_set_packed(data, supertype_offset, first + i, supertype_bits, builder->pending[i].super);
      else ts_assert(builder->pending[i].super == 0);
    } else if (supertype_bits == 16) {'''), patches)
        kernel.edit(source, 'lib/squat/scan.c', lambda s: s.replace(
            '  uint32_t lanes = 64 / bits;\n  uint32_t first_slot', '''  if (!bits) {
    uint32_t used = SQ_GROUP_SIZE - sq_group_waste(tree, group);
    return value ? 0 : UINT64_MAX >> (64 - used);
  }
  uint32_t lanes = 64 / bits;
  uint32_t first_slot'''), patches)
        # The relocation fixture intentionally writes arbitrary values into
        # every allocated column; a zero-bit column has no storage to exercise.
        kernel.edit(source, 'lib/squat/tests/unit.c', lambda s: s.replace(
            '  exercise_column(tree, tree->layout.supertype, tree->layout.supertype_bits, SQ_GROUP_SIZE, tag++, fill);',
            '  if (tree->layout.supertype_bits) exercise_column(tree, tree->layout.supertype, tree->layout.supertype_bits, SQ_GROUP_SIZE, tag++, fill);'), patches)
        kernel.edit(source, 'lib/squat/tests/supertypes.c', lambda s: s.replace(
            'layout.supertype_bits == 16', 'layout.supertype_bits == (SQ_SUPER_VARIABLE ? 9 : 16)'), patches)

    def rust_harness(self, s):
        return s.replace('job.kind == "highlights" || job.kind == "tags"', 'job.kind == "highlights" || job.kind == "tags" || job.kind == "supertype"')

    def walk_harness(self, s):
        s = s.replace('static uint64_t attributes(void *arg) {', '''static uint64_t membership_walk(void *arg) {
  Batch *batch = arg;
  uint64_t sum = 0;
  for (unsigned i = 0; i < batch->count; i++) {
    const SQTree *tree = batch->inputs[i].tree;
    for (SQNode node = sq_tree_root_node(tree); node.tree; node = sq_node_next_preorder(node)) {
      sum++;
      for (uint32_t t = 0; t < tree->supertype_count; t++)
        sum += sq_node_has_supertype(node, tree->supertypes[t]);
    }
  }
  return sum;
}

static uint64_t attributes(void *arg) {''')
        s = s.replace('"structural_queries", "field_queries"};', '"structural_queries", "field_queries", "membership_walk"};')
        s = s.replace('uncached, structural_queries, field_queries};', 'uncached, structural_queries, field_queries, membership_walk};')
        s = s.replace('const unsigned walks[] = {6, 7, 2};', 'const unsigned walks[] = {6, 7, 2, 10};')
        s = s.replace('walks_only ? 3u : 6u', 'walks_only ? 4u : 6u')
        s = s.replace('\\"field_bits\\":%u,\\"modes\\":{', '\\"field_bits\\":%u,\\"supertype_bits\\":%u,\\"supertype_count\\":%u,\\"dictionary_count\\":%u,\\"modes\\":{')
        s = s.replace('batch.inputs[0].tree->layout.field_bits);', '''batch.inputs[0].tree->layout.field_bits,
         batch.inputs[0].tree->layout.supertype_bits, batch.inputs[0].tree->supertype_count,
         sq_header(batch.inputs[0].tree)->supertype_dictionary_count);''')
        return s


if __name__ == '__main__':
    kernel.main(Columns())
