#!/usr/bin/env python3
"""Freeze committed sources and benchmark byte rounding without changing the runtime.

Examples:
  python tools/squatter/benchmark-byte-rounding.py --prepare --output build/byte-rounding
  python tools/squatter/benchmark-byte-rounding.py --output build/byte-rounding --rounds 3
"""
import argparse
import difflib
import hashlib
import io
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
import tarfile
import time

ROOT = Path(__file__).resolve().parents[2]
VARIANTS = {'exact': (8, 16), 'r7': (7, 13), 'r6': (6, 13), 'r5': (5, 13),
            'r4': (4, 13), 'r2': (2, 13), 'r10': (8, 10), 'r9': (8, 9),
            'r6_9': (6, 9), 'r6_10': (6, 10), 'r5_9': (5, 9)}

def run(cmd, **kwargs):
    result = subprocess.run(list(map(str, cmd)), text=True, capture_output=True, **kwargs)
    if result.returncode: print(result.stderr, flush=True)
    result.check_returncode()
    return result

def digest(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()

def save(path, obj):
    path.write_text(json.dumps(obj, indent=2) + '\n')

def prepare(out, points, variants, revision):
    revision = run(['git', 'rev-parse', revision], cwd=ROOT).stdout.strip()
    source = out / 'source'
    source.mkdir(parents=True, exist_ok=False)
    data = subprocess.check_output(['git', 'archive', revision, 'lib/src', 'lib/include', 'lib/squat'], cwd=ROOT)
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        archive.extractall(source, filter='data')
    changes = []
    p = source / 'lib/squat/internal.h'; old = p.read_text()
    new = old.replace('#define SQ_VERSION', '''#ifndef SQ_ROUND8_MIN
#define SQ_ROUND8_MIN 8
#endif
#ifndef SQ_ROUND16_MIN
#define SQ_ROUND16_MIN 16
#endif
_Static_assert(SQ_ROUND8_MIN >= 2 && SQ_ROUND8_MIN <= 8, "round8 cutoff");
_Static_assert(SQ_ROUND16_MIN >= 9 && SQ_ROUND16_MIN <= 16, "round16 cutoff");
#define SQ_VERSION''').replace('UINT32_C(0x53510070)',
    '(UINT32_C(0xA0000000) | ((uint32_t)SQ_ROUND8_MIN << 16) | ((uint32_t)SQ_ROUND16_MIN << 21))')
    assert 'UINT32_C(0x53510070)' in old
    new = new.replace('uint8_t sq_width(uint32_t max);', 'uint8_t sq_width(uint32_t max);\nuint8_t sq_storage_width(uint32_t max);')
    changes.extend(difflib.unified_diff(old.splitlines(True), new.splitlines(True),
                   fromfile='a/lib/squat/internal.h', tofile='b/lib/squat/internal.h'))
    p.write_text(new)
    p = source / 'lib/squat/slab.c'; old = p.read_text()
    new = old.replace('bool sq_layout(', '''uint8_t sq_storage_width(uint32_t max) {
  uint8_t bits = sq_width(max);
  if (bits <= 8 && bits >= SQ_ROUND8_MIN) return 8;
  if (bits > 8 && bits >= SQ_ROUND16_MIN) return 16;
  return bits;
}

bool sq_layout(''').replace('layout->symbol_bits = sq_width(', 'layout->symbol_bits = sq_storage_width(').replace('layout->field_bits = sq_width(', 'layout->field_bits = sq_storage_width(')
    changes.extend(difflib.unified_diff(old.splitlines(True), new.splitlines(True),
                   fromfile='a/lib/squat/slab.c', tofile='b/lib/squat/slab.c'))
    p.write_text(new)
    p = source / 'lib/squat/query_plan.c'; old = p.read_text()
    new = old.replace('uint32_t width = sq_width(', 'uint32_t width = sq_storage_width(')
    changes.extend(difflib.unified_diff(old.splitlines(True), new.splitlines(True),
                   fromfile='a/lib/squat/query_plan.c', tofile='b/lib/squat/query_plan.c'))
    p.write_text(new)
    patch = ROOT / 'lib/squat/experiments/byte-rounding.patch'
    patch.write_text(''.join(changes))
    shutil.copy2(ROOT / 'lib/squat/experiments/byte-rounding.c', source / 'lib/squat/experiments/byte-rounding.c')
    registry = json.loads((ROOT / 'build/squat-corpus-10k/registry.json').read_text())
    candidates = {}
    for directory in ('build/squat-corpus-reviewed/corpus', 'build/squat-cursors-large/corpus'):
        for p in (ROOT / directory).rglob('*'):
            if not p.is_file(): continue
            grammar = registry['suffixes'].get(p.suffix.lstrip('.'))
            if grammar not in registry['grammars']: continue
            size = p.stat().st_size
            if 1024 <= size <= 65536:
                candidates.setdefault(grammar, {})[digest(p)] = (size, str(p))
    cases = []
    for grammar, files in sorted(candidates.items()):
        files = sorted(files.values())
        chosen = [files[i][1] for i in sorted({0, len(files) // 2, len(files) - 1})]
        meta = registry['grammars'][grammar]
        cases.append(dict(name=grammar + '-small', grammar=grammar, size_class='small',
                          library=str(ROOT / 'build/squat-corpus-10k/grammars' / (grammar + '.so')), symbol=meta['symbol'], sources=chosen))
    for index, item in enumerate(json.loads((ROOT / 'build/squat-conversion-analysis/inputs.json').read_text())):
        if not Path(item['source']).exists(): continue
        cases.append(dict(name=item['grammar'] + '-large-' + str(index), grammar=item['grammar'], size_class='large',
                          library=str(ROOT / 'build/squat-corpus-10k/grammars' / (item['grammar'] + '.so')), symbol=item['symbol'], sources=[item['source']]))
    for case in cases:
        case['library_sha256'] = digest(case['library'])
        case['source_sha256'] = {p: digest(p) for p in case['sources']}
    manifest = dict(revision=revision, flags='-O3 -g -fno-omit-frame-pointer; no LTO; group16, alignment8',
                    variants=VARIANTS, cases=cases, source_hashes={str(p.relative_to(source)): digest(p)
                    for p in sorted(source.rglob('*')) if p.is_file()},
                    patch_sha256=digest(patch), compiler=run(['cc', '--version']).stdout,
                    cpu=run(['lscpu', '--json']).stdout, created=time.time())
    save(out / 'manifest.json', manifest)
    build(out, points, variants)

def build(out, points, variants):
    source = out / 'source'
    for point in points:
        for variant in variants:
            directory = out / f'{variant}-p{point}'; directory.mkdir(exist_ok=True)
            lo, hi = VARIANTS[variant]
            flags = f'-O3 -g -fno-omit-frame-pointer -DSQ_INCLUDE_POINTS={point} -DSQ_ROUND8_MIN={lo} -DSQ_ROUND16_MIN={hi}'
            # Runtime parser code is independent of the packed layout.
            common = out / 'runtime.o'
            if common.exists(): shutil.copy2(common, directory / 'runtime.o')
            # Query filters call sq_storage_width; the query object has no cutoff constants.
            shared_query = out / f'query-p{point}.o'
            if shared_query.exists(): shutil.copy2(shared_query, directory / 'query.o')
            cmd = ['make', '-C', source / 'lib/squat', '-j4', 'BUILD=' + str(directory),
                   'CFLAGS=' + flags, 'check', 'all']
            result = subprocess.run(list(map(str, cmd)), text=True, capture_output=True)
            (directory / 'build.log').write_text(result.stdout + result.stderr)
            result.check_returncode()
            if not common.exists(): shutil.copy2(directory / 'runtime.o', common)
            if not shared_query.exists(): shutil.copy2(directory / 'query.o', shared_query)
            includes = ['-I' + str(source / p) for p in ('lib/include', 'lib/src', 'lib/squat/include')]
            result = run(['cc', *flags.split(), '-std=c11', '-Wall', '-Wextra', '-Werror', *includes,
                          source / 'lib/squat/experiments/byte-rounding.c', directory / 'libtree-sitter-squat.a',
                          directory / 'runtime.o', '-ldl', '-o', directory / 'bench'])
            print('built', directory.name, flush=True)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=Path('build/byte-rounding'))
    parser.add_argument('--prepare', action='store_true')
    parser.add_argument('--revision', default='HEAD')
    parser.add_argument('--build', action='store_true')
    parser.add_argument('--points', default='1')
    parser.add_argument('--variants', default='exact,r7,r6,r5,r4,r2,r10,r9,r6_9,r6_10')
    parser.add_argument('--rounds', type=int, default=1)
    parser.add_argument('--repeats', type=int, default=5)
    parser.add_argument('--cpu', type=int, default=2)
    parser.add_argument('--cases', default='')
    parser.add_argument('--tag', default='sweep')
    parser.add_argument('--micro', action='store_true')
    parser.add_argument('--end-to-end', action='store_true', help='Include cursor/iterator walks and structural/field queries')
    args = parser.parse_args()
    if args.end_to_end: os.environ['SQ_END_TO_END'] = '1'
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=True)
    points = list(map(int, args.points.split(','))); variants = args.variants.split(',')
    if args.prepare: prepare(out, points, variants, args.revision); return
    if args.build: build(out, points, variants); return
    manifest = json.loads((out / 'manifest.json').read_text())
    cases = [c for c in manifest['cases'] if not args.cases or c['name'] in args.cases.split(',')]
    records = []; output = out / (args.tag + '.json')
    assert not output.exists(), 'use a fresh --tag'
    meta = dict(end_to_end='SQ_END_TO_END' in os.environ,
                harness_sha256=digest(out/'source/lib/squat/experiments/byte-rounding.c'),
                manifest_sha256=digest(out/'manifest.json'), cpu=args.cpu, started=time.time(), records=records,
                policies={v: VARIANTS[v] for v in variants},
                binaries={f'{v}-p{p}': digest(out/f'{v}-p{p}'/'bench') for p in points for v in variants})
    if args.micro:
        meta['micro'] = json.loads(run(['taskset', '-c', args.cpu, out/'exact-p1/bench', '--micro', args.repeats]).stdout)
        save(output, meta); return
    for round_number in range(args.rounds):
        for point in points:
            for index, case in enumerate(cases):
                order = variants.copy(); random.Random(1729 + round_number * 1000 + index).shuffle(order)
                for variant in order:
                    command = ['taskset', '-c', args.cpu, out / f'{variant}-p{point}' / 'bench',
                               case['library'], case['symbol'], args.repeats, *case['sources']]
                    frequency = Path(f'/sys/devices/system/cpu/cpu{args.cpu}/cpufreq/scaling_cur_freq')
                    before_khz = int(frequency.read_text()) if frequency.exists() else None
                    result = run(command)
                    after_khz = int(frequency.read_text()) if frequency.exists() else None
                    measured = json.loads(result.stdout)
                    records.append(dict(round=round_number, case=case['name'], variant=variant,
                                        points=point, before_khz=before_khz, after_khz=after_khz, measured=measured))
                    save(output, meta)
                print(f'round {round_number + 1}: p{point} {case["name"]}', flush=True)
    meta['finished'] = time.time(); save(output, meta)

if __name__ == '__main__': main()
