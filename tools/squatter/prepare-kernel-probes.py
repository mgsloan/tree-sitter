#!/usr/bin/env python3
"""Build isolated query/unpack probes; never edit production C sources.

Uses the same committed runtime as the byte-width experiments. Requires GCC
and an x86-64 test host with BMI2/AVX2 for the explicitly selected ISA probes.
All mutations happen under --output; production sources remain untouched.
"""
import argparse
import difflib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[2]
REVISION = 'ab9143b2fa3aff3fbde2062eb85988be7c6c7042'
FLAGS = '-O3 -g -fno-omit-frame-pointer'
VARIANTS = ['exact', 'control', 'noscan', 'simd', 'clipped', 'swar', 'avx2', 'special',
            'diagnostic', 'cache16', 'cache64', 'cache256', 'cachefull',
            'autoavx', 'scalarfilter', 'scanwidth', 'hoist', 'lookup', 'warmexact', 'warmfull',
            'presence64', 'presence256', 'grouppext']


def presence_probe(text, window):
    marker = 'static bool sq_query_cursor__presence_matches('
    helper = f'''// Cache decoded blocks specifically for repeated descendant-presence tests.
// The node-entry path retains its original direct packed reads in this probe.
static uint64_t query_presence_equal(SQQueryCursor *self, const SQTree *tree,
                                     uint32_t group, uint16_t value, bool field) {{
  _Static_assert(SQ_GROUP_SIZE == 16, "presence SIMD probe uses 16-slot groups");
  uint32_t first = group * SQ_GROUP_SIZE, base = first & ~{window - 1}u;
  unsigned set = (base / {window}) & 3u;
  (void)query_cached_id(&self->cursor, (SQNode){{tree, first}}, field);
  const uint16_t *values = (field ? self->cursor.blocks[set].field : self->cursor.blocks[set].symbol) + first - base;
  __m128i wanted = _mm_set1_epi16(value);
  __m128i a = _mm_cmpeq_epi16(_mm_loadu_si128((const __m128i *)values), wanted);
  __m128i b = _mm_cmpeq_epi16(_mm_loadu_si128((const __m128i *)(values + 8)), wanted);
  uint64_t hits = (unsigned)_mm_movemask_epi8(_mm_packs_epi16(a, b));
  return hits & ((UINT64_C(1) << (SQ_GROUP_SIZE - sq_group_waste(tree, group))) - 1);
}}

'''
    text = text.replace(marker, helper + marker)
    text = text.replace('sq_tree_group_symbol_equal(\n            root.tree, sq_position_group(root.tree, group), requirement->symbols[index])',
                        'query_presence_equal(\n            self, root.tree, sq_position_group(root.tree, group), requirement->symbols[index], false)')
    text = text.replace('sq_tree_group_field_equal(\n          root.tree, sq_position_group(root.tree, group), requirement->field)',
                        'query_presence_equal(\n          self, root.tree, sq_position_group(root.tree, group), requirement->field, true)')
    return text


def group_pext(text):
    text = text.replace('#include "internal.h"', '#include "internal.h"\n#include <immintrin.h>')
    text = text.replace('static uint64_t group_equal(', '__attribute__((target("bmi2"))) static uint64_t group_equal(')
    a = text.index('  uint32_t lanes = 64 / bits;', text.index('static uint64_t group_equal('))
    b = text.index('\n  uint32_t waste =', a)
    return text[:a] + '''  uint32_t lanes = 64 / bits;
  uint32_t first_slot = group * SQ_GROUP_SIZE;
  uint32_t last_word = (first_slot + SQ_GROUP_SIZE - 1) / lanes;
  uint64_t starts = sq_lane_starts(bits), high = starts << (bits - 1);
  uint64_t low = high - starts, target = starts * value;
  uint64_t matches = 0;
  for (uint32_t word_index = first_slot / lanes; word_index <= last_word; word_index++) {
    uint64_t word;
    memcpy(&word, tree->data + offset + (size_t)word_index * 8, 8);
    uint64_t difference = word ^ target;
    uint64_t equal = ~(((difference & low) + low) | difference) & high;
    uint64_t compact = _pext_u64(equal, high);
    uint32_t base = word_index * lanes;
    if (base >= first_slot) matches |= compact << (base - first_slot);
    else matches |= compact >> (first_slot - base);
  }
''' + text[b:]


def cache_probe(text, variant):
    if variant == 'cachefull':
        text = text.replace('  SQCursor *cursor;\n} QueryTreeCursor;', '''  SQCursor *cursor;
  SQUnpack unpack;
  uint16_t *symbols, *fields;
} QueryTreeCursor;

// Lazy full-column decoding: retain values for the complete execution, with
// no block eviction. Allocation, decoding, and deletion are all timed.
static uint16_t query_cached_id(QueryTreeCursor *cursor, SQNode node, bool field) {
  uint16_t **values = field ? &cursor->fields : &cursor->symbols;
  if (!*values) {
    uint32_t count = sq_tree_slot_count(node.tree);
    *values = ts_malloc((size_t)count * sizeof(uint16_t));
    cursor->unpack(node.tree->data + (field ? node.tree->layout.field : node.tree->layout.symbol),
                  0, count, field ? node.tree->layout.field_bits : node.tree->layout.symbol_bits, *values);
  }
  return (*values)[node.slot];
}''')
        text = text.replace('void sq_query_cursor_delete(SQQueryCursor *self) {', '''void sq_query_cursor_delete(SQQueryCursor *self) {
  ts_free(self->cursor.symbols);
  ts_free(self->cursor.fields);''')
        text = text.replace('  self->first_capture.valid = false;\n  query_tree_cursor_reset', '''  self->first_capture.valid = false;
  ts_free(self->cursor.symbols);
  ts_free(self->cursor.fields);
  self->cursor.symbols = self->cursor.fields = NULL;
  self->cursor.unpack = sq_unpack_select(SQ_UNPACK_KERNEL);
  query_tree_cursor_reset''')
        return cache_read_sites(text)
    window = int(variant.removeprefix('cache'))
    sets = 1 if window == 16 else 4
    text = text.replace('  SQCursor *cursor;\n} QueryTreeCursor;', f'''  SQCursor *cursor;
  SQUnpack unpack;
  struct {{ uint32_t tag; bool fields; uint16_t symbol[{window}], field[{window}]; }} blocks[{sets}];
}} QueryTreeCursor;

// Query-owned decoded blocks survive cursor navigation, backtracking, and
// next_capture/next_match calls. exec() invalidates them even for reused trees.
static uint16_t query_cached_id(QueryTreeCursor *cursor, SQNode node, bool field) {{
  uint32_t base = node.slot & ~{window - 1}u;
  unsigned set = (base / {window}) & {sets - 1}u;
  uint32_t count = sq_tree_slot_count(node.tree) - base;
  if (count > {window}) count = {window};
  if (cursor->blocks[set].tag != base + 1) {{
    cursor->blocks[set].tag = base + 1;
    cursor->blocks[set].fields = false;
    cursor->unpack(node.tree->data + node.tree->layout.symbol, base, count,
                   node.tree->layout.symbol_bits, cursor->blocks[set].symbol);
  }}
  if (field && !cursor->blocks[set].fields) {{
    cursor->unpack(node.tree->data + node.tree->layout.field, base, count,
                   node.tree->layout.field_bits, cursor->blocks[set].field);
    cursor->blocks[set].fields = true;
  }}
  return field ? cursor->blocks[set].field[node.slot - base] : cursor->blocks[set].symbol[node.slot - base];
}}''')
    text = text.replace('  self->first_capture.valid = false;\n  query_tree_cursor_reset', '''  self->first_capture.valid = false;
  memset(self->cursor.blocks, 0, sizeof(self->cursor.blocks));
  self->cursor.unpack = sq_unpack_select(SQ_UNPACK_KERNEL);
  query_tree_cursor_reset''')
    return cache_read_sites(text)


def cache_read_sites(text):
    text = text.replace('sq_query_cursor__current_status(const QueryTreeCursor *cursor,', 'sq_query_cursor__current_status(QueryTreeCursor *cursor,')
    text = text.replace('sq_decode_symbol(node.tree, sq_node_symbol_id(node));', 'sq_decode_symbol(node.tree, query_cached_id(cursor, node, false));')
    text = text.replace('sq_cursor_depth(cursor->cursor) ? sq_node_field_id(node) : 0;', 'sq_cursor_depth(cursor->cursor) ? query_cached_id(cursor, node, true) : 0;')
    text = text.replace('      uint32_t symbol = sq_node_symbol_id(node);', '      uint32_t symbol = query_cached_id(&self->cursor, node, false);')
    return text


def digest(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def edit(source, relative, transform, patches):
    p = source / relative
    old = p.read_text()
    new = transform(old)
    assert new != old, relative
    p.write_text(new)
    patches.extend(difflib.unified_diff(old.splitlines(True), new.splitlines(True),
                                      fromfile='a/' + relative, tofile='b/' + relative))


def query_probe(text, variant):
    marker = 'static uint32_t query_execution_find_symbols('
    if variant in ('hoist', 'lookup'):
        text = text.replace('uint32_t width = tree->layout.symbol_bits, lanes = 64 / width;',
                            'uint32_t width = tree->layout.symbol_bits, lanes = tree->layout.symbol_lanes;')
        text = text.replace('    uint32_t word_index = (high - 1) / lanes;',
                            '    uint32_t word_index = (high - 1) / lanes;\n    uint32_t low_word = low / lanes;')
        text = text.replace('if (word_index == low / lanes)', 'if (word_index == low_word)')
        if variant == 'lookup':
            text = text.replace('  filter->width = width;', '  filter->width = width;\n  for (unsigned bit = 0; bit < 64; bit++) filter->lane_for_bit[bit] = bit / width;')
            text = text.replace('word_index * lanes + bit / width', 'word_index * lanes + filter->lane_for_bit[bit]')
            text = text.replace('uint32_t width = tree->layout.symbol_bits, lanes = tree->layout.symbol_lanes;', 'uint32_t lanes = tree->layout.symbol_lanes;')
        return text
    if variant in ('autoavx', 'scalarfilter'):
        attribute = 'target("avx2")' if variant == 'autoavx' else 'optimize("no-tree-vectorize")'
        return text.replace(marker, '__attribute__(('+attribute+')) ' + marker)
    if variant == 'scanwidth':
        a = text.index(marker)
        b = text.index('\nstatic uint32_t query_execution_find_root', a)
        function = text[a:b].replace(marker, 'static inline __attribute__((always_inline)) uint32_t query_execution_find_symbols_width(')
        function = function.replace('uint32_t end) {', 'uint32_t end, uint32_t width) {', 1)
        function = function.replace('  uint32_t width = tree->layout.symbol_bits, lanes = 64 / width;', '  uint32_t lanes = 64 / width;')
        wrapper = '''static uint32_t query_execution_find_symbols(SQQueryCursor *cursor, const SQTree *tree,
                                             const QuerySymbolFilter *filter, uint32_t start,
                                             uint32_t end) {
  switch (tree->layout.symbol_bits) {
'''
        for bits in (5, 6, 8, 9, 10, 16):
            wrapper += f'    case {bits}: return query_execution_find_symbols_width(cursor, tree, filter, start, end, {bits});\n'
        wrapper += '''    default: return query_execution_find_symbols_width(cursor, tree, filter, start, end, tree->layout.symbol_bits);
  }
}
'''
        return text[:a]+function+'\n'+wrapper+text[b:]
    if variant == 'noscan':
        return text.replace('  filter->count = count;', '  filter->count = 0; // Negative control: scalar membership fallback.')
    if variant == 'clipped':
        a = text.index('      while (hits) {', text.index('static uint32_t query_execution_find_symbols('))
        b = text.index('\n      if (word_index ==', a)
        return text[:a] + '''      // Clip to the requested physical range before selecting a lane.
      uint32_t base = word_index * lanes;
      uint32_t first_bit = (low > base ? low - base : 0) * width;
      uint32_t last_bit = (high - base < lanes ? high - base : lanes) * width;
      hits &= UINT64_MAX << first_bit;
      if (last_bit < 64) hits &= (UINT64_C(1) << last_bit) - 1;
      if (hits) {
        unsigned bit = 63u - (unsigned)__builtin_clzll(hits);
        return slots - 1 - (base + bit / width);
      }
''' + text[b:]
    if variant == 'simd':
        marker = 'static uint32_t query_execution_find_symbols('
        text = text.replace(marker, '__attribute__((target("avx2"))) ' + marker)
        a = text.index('      for (uint32_t index = 0; index < filter->count;', text.index(marker))
        b = text.index('\n\n      while (hits)', a)
        return text[:a] + '''      uint32_t index = 0;
      // Four independent masked SWAR comparisons in parallel. Small sets
      // retain the scalar loop; no speculative reads past the packed column.
      if (filter->count >= 4) {
        __m256i low_bits = _mm256_set1_epi64x(filter->low_bits);
        __m256i combined = _mm256_setzero_si256();
        for (; index + 4 <= filter->count; index += 4) {
          __m256i difference = _mm256_and_si256(
            _mm256_xor_si256(_mm256_set1_epi64x(word),
              _mm256_loadu_si256((const __m256i *)(filter->values + index))),
            _mm256_loadu_si256((const __m256i *)(filter->masks + index)));
          __m256i misses = _mm256_or_si256(difference,
            _mm256_add_epi64(_mm256_and_si256(difference, low_bits), low_bits));
          combined = _mm256_or_si256(combined,
            _mm256_andnot_si256(misses, _mm256_set1_epi64x(filter->high_bits)));
        }
        __m128i pair = _mm_or_si128(_mm256_castsi256_si128(combined),
                                   _mm256_extracti128_si256(combined, 1));
        hits = (uint64_t)_mm_cvtsi128_si64(_mm_or_si128(pair, _mm_srli_si128(pair, 8)));
      }
      for (; index < filter->count; index++) {
        uint64_t difference = (word ^ filter->values[index]) & filter->masks[index];
        hits |= ~(((difference & filter->low_bits) + filter->low_bits) | difference) & filter->high_bits;
      }''' + text[b:]
    raise ValueError(variant)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', type=Path, default=ROOT/'build/kernel-probes')
    ap.add_argument('--variants', default=','.join(VARIANTS))
    ap.add_argument('--points', default='1,0')
    args = ap.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    manifest_path = out/'build-manifest.json'
    meta = json.loads(manifest_path.read_text()) if manifest_path.exists() else dict(
        revision=REVISION, cflags=FLAGS, compiler=subprocess.check_output(['cc', '--version'], text=True),
        rustc=subprocess.check_output(['rustc', '--version'], text=True), binaries={})
    assert meta['revision'] == REVISION
    rust = out/'rust'
    if not rust.exists():
        rust.mkdir()
        with tarfile.open(fileobj=io.BytesIO(subprocess.check_output(['git', 'archive', REVISION], cwd=ROOT))) as archive:
            archive.extractall(rust, filter='data')
    (rust/'crates/squatter-bench/src/bin').mkdir(exist_ok=True)
    shutil.copy2(ROOT/'crates/squatter-bench/src/bin/query-workload.rs', rust/'crates/squatter-bench/src/bin/query-workload.rs')
    (rust/'crates/squatter/build.rs').write_text('''fn main() {
    let path = std::env::var("SQ_PREBUILT_LIB").unwrap();
    println!("cargo:rerun-if-env-changed=SQ_PREBUILT_LIB");
    println!("cargo:rerun-if-changed={path}/libtree-sitter-squat.a");
    println!("cargo:rustc-link-search=native={path}");
    println!("cargo:rustc-link-lib=static=tree-sitter-squat");
}
''')
    meta['harness_sha256'] = digest(rust/'crates/squatter-bench/src/bin/query-workload.rs')
    original_harness = (rust/'crates/squatter-bench/src/bin/query-workload.rs').read_text()
    for point in map(int, args.points.split(',')):
        for variant in args.variants.split(','):
            assert variant in VARIANTS
            key = f'{variant}-p{point}'
            dest = out/'binaries'/key
            if key in meta['binaries']:
                assert digest(dest/'query-workload') == meta['binaries'][key]['binary_sha256']
                continue
            source = out/'sources'/key
            source.mkdir(parents=True, exist_ok=False)
            for sub in ('lib/include', 'lib/src', 'lib/squat'):
                shutil.copytree(rust/sub, source/sub)
            patches = []
            if variant == 'grouppext':
                edit(source, 'lib/squat/scan.c', group_pext, patches)
                shutil.copy2(ROOT/'lib/squat/experiments/group-equality-check.c', source/'lib/squat/experiments/group-equality-check.c')
            if variant.startswith('presence'):
                window = int(variant.removeprefix('presence'))
                def presence_cursor(s):
                    s = cache_probe(s, 'cache'+str(window))
                    s = s.replace('query_cached_id(cursor, node, false)', 'sq_node_symbol_id(node)')
                    s = s.replace('query_cached_id(cursor, node, true)', 'sq_node_field_id(node)')
                    s = s.replace('query_cached_id(&self->cursor, node, false)', 'sq_node_symbol_id(node)')
                    return s.replace('#include <wctype.h>', '#include <wctype.h>\n#include <immintrin.h>')
                edit(source, 'lib/squat/query.c', presence_cursor, patches)
                edit(source, 'lib/squat/query_plan.c', lambda s: presence_probe(s, window), patches)
            if variant in ('noscan', 'simd', 'clipped', 'autoavx', 'scalarfilter', 'scanwidth', 'hoist', 'lookup'):
                edit(source, 'lib/squat/query_plan.c', lambda s: query_probe(s, variant), patches)
            if variant == 'lookup':
                edit(source, 'lib/squat/query.c', lambda s: s.replace('  uint64_t values[8], masks[8];', '  uint64_t values[8], masks[8];\n  uint8_t lane_for_bit[64];'), patches)
            if variant == 'simd':
                edit(source, 'lib/squat/query.c', lambda s: s.replace('#include <wctype.h>', '#include <wctype.h>\n#include <immintrin.h>'), patches)
            if variant.startswith('cache') or variant == 'warmfull':
                def cache_transform(s):
                    s = cache_probe(s, 'cachefull' if variant == 'warmfull' else variant)
                    if variant == 'warmfull':
                        s = s.replace('  uint16_t *symbols, *fields;', '  uint16_t *symbols, *fields;\n  const SQTree *cached_tree;')
                        s = s.replace('''  ts_free(self->cursor.symbols);
  ts_free(self->cursor.fields);
  self->cursor.symbols = self->cursor.fields = NULL;''', '''  // Experiment-only: inputs retain immutable trees for the cursor lifetime.
  // A production cross-execution cache needs an explicit lifetime contract,
  // not merely pointer equality after a tree might have been destroyed.
  if (self->cursor.cached_tree != node.tree) {
    ts_free(self->cursor.symbols);
    ts_free(self->cursor.fields);
    self->cursor.symbols = self->cursor.fields = NULL;
    self->cursor.cached_tree = node.tree;
  }''')
                    return s
                edit(source, 'lib/squat/query.c', cache_transform, patches)
                edit(source, 'lib/squat/query_plan.c', lambda s: s.replace(
                    'sq_node_symbol_id(sq_position_node(tree, start))', 'query_cached_id(&self->cursor, sq_position_node(tree, start), false)').replace(
                    'sq_node_symbol_id(sq_position_node(tree, node))', 'query_cached_id(&self->cursor, sq_position_node(tree, node), false)').replace(
                    'sq_node_field_id(sq_position_node(tree, node))', 'query_cached_id(&self->cursor, sq_position_node(tree, node), true)'), patches)
            if variant == 'diagnostic':
                edit(source, 'lib/squat/query.c', lambda s: s.replace('void sq_query_delete(SQQuery *self) {', '''static unsigned long long scan_calls, scan_words;
__attribute__((destructor)) static void report_scan_calls(void) {
  fprintf(stderr, "SCAN calls=%llu words=%llu\\n", scan_calls, scan_words);
}
void sq_query_delete(SQQuery *self) {
  if (self) fprintf(stderr, "FILTER targets=%u comparisons=%u width=%u\\n",
                   self->scan_targets.size, self->scan_filter.count, self->scan_filter.width);'''), patches)
                edit(source, 'lib/squat/query_plan.c', lambda s: s.replace('  uint32_t width = tree->layout.symbol_bits, lanes = 64 / width;', '  scan_calls++;\n  uint32_t width = tree->layout.symbol_bits, lanes = 64 / width;').replace('      uint64_t word, hits = 0;', '      scan_words++;\n      uint64_t word, hits = 0;'), patches)
            if variant == 'special':
                def specialize(s):
                    s = s.replace('static inline void unpack_words(', 'static inline __attribute__((always_inline)) void unpack_words(')
                    old = '  unpack_words(column, first, count, bits, out, deposit_four);'
                    cases = '\n'.join(f'    case {b}: unpack_words(column, first, count, {b}, out, deposit_four); return;' for b in range(1, 17))
                    return s.replace(old, '  switch (bits) {\n' + cases + '\n  }')
                edit(source, 'lib/squat/unpack.c', specialize, patches)
            harness = (ROOT/'lib/squat/experiments/byte-rounding.c').read_text()
            harness = harness.replace('    prepare_query(in, language);', '    if (!getenv("SQ_WALK_ONLY")) prepare_query(in, language);')
            harness = harness.replace('    sq_query_cursor_delete(in->query_cursor);', '    if (in->query_cursor) sq_query_cursor_delete(in->query_cursor);')
            harness = harness.replace('  for (unsigned i = 0; i < 6; i++) {\n    unsigned op = end_to_end ? primary[i] : i;', '  const unsigned walks[] = {6, 7, 2};\n  bool walks_only = getenv("SQ_WALK_ONLY") != NULL;\n  for (unsigned i = 0; i < (walks_only ? 3u : 6u); i++) {\n    unsigned op = walks_only ? walks[i] : end_to_end ? primary[i] : i;')
            (source/'lib/squat/experiments/byte-rounding.c').write_text(harness)
            dest.mkdir(parents=True)
            (dest/'probe.patch').write_text(''.join(patches))
            flags = FLAGS + f' -DSQ_INCLUDE_POINTS={point}'
            if variant in ('swar', 'avx2'):
                flags += ' -DSQ_UNPACK_KERNEL=' + ('2' if variant == 'swar' else '4')
            command = ['make', '-C', str(source/'lib/squat'), '-j4', 'BUILD='+str(dest), 'CFLAGS='+flags, 'check', 'all', str(dest/'unpack-bench')]
            with (dest/'build.log').open('w') as log:
                subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)
                includes = ['-I'+str(source/p) for p in ('lib/include', 'lib/src', 'lib/squat/include')]
                if variant == 'grouppext':
                    subprocess.run(['cc', *flags.split(), '-std=c11', *includes, str(source/'lib/squat/experiments/group-equality-check.c'), str(dest/'libtree-sitter-squat.a'), str(dest/'runtime.o'), '-ldl', '-o', str(dest/'group-check')], stdout=log, stderr=subprocess.STDOUT, check=True)
                    subprocess.run([str(dest/'group-check')], stdout=log, stderr=subprocess.STDOUT, check=True)
                subprocess.run(['cc', *flags.split(), '-std=c11', *includes, str(source/'lib/squat/experiments/byte-rounding.c'), str(dest/'libtree-sitter-squat.a'), str(dest/'runtime.o'), '-ldl', '-o', str(dest/'walk')], stdout=log, stderr=subprocess.STDOUT, check=True)
                cargo = ['cargo', 'build', '--manifest-path', str(rust/'Cargo.toml'), '--locked', '--release', '-p', 'squatter-bench', '--bin', 'query-workload', '--target-dir', str(out/'target')]
                rust_harness = original_harness
                if variant.startswith('warm'):
                    rust_harness = rust_harness.replace('struct Input {\n', 'struct Input {\n    cursor: std::cell::RefCell<tree_sitter_squatter::QueryCursor>,\n')
                    rust_harness = rust_harness.replace('        inputs.push(Input {\n', '        inputs.push(Input {\n            cursor: std::cell::RefCell::new(tree_sitter_squatter::QueryCursor::new()),\n')
                    rust_harness = rust_harness.replace('        let mut cursor = tree_sitter_squatter::QueryCursor::new();', '        let mut cursor = input.cursor.borrow_mut();')
                (rust/'crates/squatter-bench/src/bin/query-workload.rs').write_text(rust_harness)
                if not point:
                    cargo.append('--no-default-features')
                subprocess.run(cargo, env={**os.environ, 'SQ_PREBUILT_LIB': str(dest), 'CFLAGS': FLAGS}, stdout=log, stderr=subprocess.STDOUT, check=True)
            shutil.copy2(out/'target/release/query-workload', dest/'query-workload')
            meta['binaries'][key] = dict(binary_sha256=digest(dest/'query-workload'), walk_sha256=digest(dest/'walk'), c_library_sha256=digest(dest/'libtree-sitter-squat.a'), patch_sha256=digest(dest/'probe.patch'), harness_sha256=digest(rust/'crates/squatter-bench/src/bin/query-workload.rs'), flags=flags, command=command, cargo=cargo)
            manifest_path.write_text(json.dumps(meta, indent=2)+'\n')
            print('built', key, flush=True)


if __name__ == '__main__':
    main()
