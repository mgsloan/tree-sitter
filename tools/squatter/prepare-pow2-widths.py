#!/usr/bin/env python3
"""Compare exact column widths with independent power-of-two widening.

Frozen baseline, shared zero-column omission, and byte extraction for 1/2/4-bit
values keep each comparison focused on storage width. No runtime files change.
"""
import importlib.util
from pathlib import Path

spec = importlib.util.spec_from_file_location('columns', Path(__file__).with_name('prepare-column-probes.py'))
columns = importlib.util.module_from_spec(spec)
spec.loader.exec_module(columns)
kernel = columns.kernel
POLICIES = {'exact': (0, 1), 'control': (0, 1)}
for column, number in [('field', 1), ('symbol', 2), ('super', 3)]:
    for target in [2, 4, 8, 16]:
        POLICIES[f'{column}{target}'] = (number, target)
kernel.VARIANTS = list(POLICIES)


class Powers(columns.Columns):
    output = 'build/pow2-widths'

    def flags(self, variant):
        column, target = POLICIES[variant]
        return f'-DSQ_FIELD_ROUND=31 -DSQ_SYMBOL_ROUND=0 -DSQ_SUPER_VARIABLE=2 -DSQ_ROUND_COLUMN={column} -DSQ_ROUND_TARGET={target}'

    def apply(self, source, variant, patches):
        super().apply(source, 'fieldlesssuper', patches)
        def slab(s):
            start = s.index('uint8_t sq_probe_width(')
            end = s.index('\nstatic uint8_t probe_supertype_bits', start)
            s = s[:start] + '''static uint8_t round_width(uint8_t bits, unsigned column) {
  return bits && column == SQ_ROUND_COLUMN && bits < SQ_ROUND_TARGET ? SQ_ROUND_TARGET : bits;
}
uint8_t sq_probe_width(uint32_t max, bool field) {
  uint8_t bits = 0;
  while (max) { bits++; max >>= 1; }
  return round_width(bits, field ? 1 : 2);
}
''' + s[end:]
            s = s.replace('return (uint8_t)count;', 'return round_width((uint8_t)count, 3);')
            s = s.replace('return bits; // Dense dictionary', 'return round_width(bits, 3); // Dense dictionary')
            return s
        kernel.edit(source, 'lib/squat/slab.c', slab, patches)
        def header(s):
            start = s.index('#define SQ_VERSION ')
            end = s.index('\n\n', start)
            s = s[:start] + '''#define SQ_VERSION (UINT32_C(0xD0000090) | ((uint32_t)SQ_ROUND_COLUMN << 17) | \\
                    ((uint32_t)SQ_ROUND_TARGET << 19))''' + s[end:]
            marker = 'static inline uint32_t sq_node_field_value('
            pos = s.index(marker)
            helper = '''static inline uint32_t sq_probe_small_value(const SQTree *tree, uint32_t offset,
                                             uint32_t slot, uint8_t bits, uint8_t lanes,
                                             uint32_t mask) {
  if (!bits) return 0;
  if (bits == 1) return sq_get_bit(tree->data, offset, slot);
  if (bits == 2) return (sq_get_u8(tree->data, offset, slot / 4) >> ((slot % 4) * 2)) & 3u;
  if (bits == 4) return (sq_get_u8(tree->data, offset, slot / 2) >> ((slot % 2) * 4)) & 15u;
  return sq_get_packed_cached(tree->data, offset, slot, bits, lanes, mask);
}

'''
            # Put the helper before both symbol and field accessors.
            pos = s.index('static inline uint32_t sq_node_symbol_id(')
            s = s[:pos] + helper + s[pos:]
            for name in ['symbol', 'field']:
                s = s.replace(f'sq_get_packed_cached(node.tree->data, node.tree->layout.{name}, node.slot,', f'sq_probe_small_value(node.tree, node.tree->layout.{name}, node.slot,')
            return s
        kernel.edit(source, 'lib/squat/internal.h', header, patches)
        kernel.edit(source, 'lib/squat/tests/supertypes.c', lambda s: s.replace(
            '(SQ_SUPER_VARIABLE ? 9 : 16)', '(SQ_ROUND_COLUMN == 3 && SQ_ROUND_TARGET > 9 ? SQ_ROUND_TARGET : 9)'), patches)


if __name__ == '__main__':
    kernel.main(Powers())
