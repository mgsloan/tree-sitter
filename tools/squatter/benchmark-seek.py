#!/usr/bin/env python3
"""Pair two node.c implementations on identical packed corpus trees.

The baseline is compiled against the current slab layout; use this for seek-only
changes, not comparisons across encoding changes. No baseline checkout is edited.
"""
import argparse
import datetime
import hashlib
import json
import platform
import statistics
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def summarize(rows):
    result = {}
    for points in sorted({row['points'] for row in rows}):
        for mixed in (0, 1):
            data = [row for row in rows if row['points'] == points and row['mixed'] == mixed]
            ratios = [row['after_ns'] / row['before_ns'] for row in data]
            result[f"{'point' if points else 'byte'}-{'mixed' if mixed else 'empty'}"] = {
                'cases': len(data),
                'before_ns': sum(row['before_ns'] for row in data),
                'after_ns': sum(row['after_ns'] for row in data),
                'total_ratio': sum(row['after_ns'] for row in data) / sum(row['before_ns'] for row in data),
                'median_ratio': statistics.median(ratios),
            }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', default='220ee121c')
    parser.add_argument('--bundle', type=Path, default=ROOT / 'build/squat-corpus-10k')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--points', type=int, choices=(0, 1), default=1)
    parser.add_argument('--cpu', type=int, default=0)
    parser.add_argument('--rounds', type=int, default=32)
    parser.add_argument('--files-per-grammar', type=int, help='Omit to use the full 10,000-file manifest')
    parser.add_argument('--grammar', help='Restrict to one grammar, e.g. for sampling profiles')
    parser.add_argument('--profile', choices=('byte', 'point', 'before-byte', 'before-point'))
    parser.add_argument('--perf-event', default='cycles:u')
    args = parser.parse_args()
    if args.profile and 'point' in args.profile and not args.points:
        parser.error('point profiling requires --points 1')
    if args.rounds <= 0:
        parser.error('--rounds must be positive')
    if args.files_per_grammar is not None and args.files_per_grammar <= 0:
        parser.error('--files-per-grammar must be positive')

    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error('--output must be empty; use a new directory for each build and run')
    output.mkdir(parents=True, exist_ok=True)
    bundle = args.bundle.resolve()
    manifest_path = bundle / 'manifest.json'
    registry_path = bundle / 'local-registry.json'
    inputs = json.loads(manifest_path.read_text())['inputs']
    registry = json.loads(registry_path.read_text())['grammars']
    if args.grammar and args.grammar not in registry:
        parser.error(f'unknown grammar: {args.grammar}')
    flags = ['-O3', '-g', f'-DSQ_INCLUDE_POINTS={args.points}', '-std=c11', '-Wall', '-Wextra', '-Werror']
    includes = [f'-I{ROOT / path}' for path in ('lib/include', 'lib/src', 'lib/squat', 'lib/squat/include')]
    source = output / 'node-before.c'
    source.write_bytes(subprocess.check_output(['git', 'show', f'{args.baseline}:lib/squat/node.c'], cwd=ROOT))
    (output / 'node-after.c').write_bytes((ROOT / 'lib/squat/node.c').read_bytes())
    before = output / 'node-before.o'
    with (output / 'build.log').open('w') as log:
        def build(command):
            subprocess.run(command, cwd=ROOT, stdout=log, stderr=log, check=True)

        build(['cc', *flags, *includes, '-c', str(source), '-o', str(before)])
        symbols = subprocess.check_output(['nm', '--defined-only', str(before)], text=True)
        names = [line.split()[-1] for line in symbols.splitlines()
                 if len(line.split()) == 3 and line.split()[1].lower() == 't']
        mapping = output / 'symbols.txt'
        mapping.write_text(''.join(f'{name} before_{name}\n' for name in names))
        build(['objcopy', f'--redefine-syms={mapping}', str(before)])
        library = output / 'current/libtree-sitter-squat.a'
        runtime = output / 'current/runtime.o'
        build(['make', '-C', 'lib/squat', '-j4', f'BUILD={output / "current"}',
               f'CFLAGS={" ".join(flags)}', str(library), str(runtime)])
        binary = output / 'seek-bench'
        build(['cc', *flags, *includes, 'lib/squat/experiments/seek.c', str(before),
               str(library), str(runtime), '-ldl', '-o', str(binary)])

    metadata = {
        'started': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'compiler': subprocess.check_output(['cc', '--version'], text=True).splitlines()[0],
        'power_online': {str(path): path.read_text().strip() for path in Path('/sys/class/power_supply').glob('*/online')},
        'baseline': subprocess.check_output(['git', 'rev-parse', args.baseline], cwd=ROOT, text=True).strip(),
        'current_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'baseline_node_sha256': digest(source),
        'current_node_sha256': digest(ROOT / 'lib/squat/node.c'),
        'probe_sha256': digest(ROOT / 'lib/squat/experiments/seek.c'),
        'binary_sha256': digest(binary),
        'input_manifest_sha256': digest(manifest_path),
        'registry_sha256': digest(registry_path),
        'flags': flags,
        'cpu': args.cpu,
        'platform': platform.platform(),
        'cpu_info': subprocess.check_output(['lscpu'], text=True),
        'rounds': args.rounds,
        'queries_per_round': 128,
        'repeats': 5,
        'points': args.points,
        'operations': [],
    }
    all_rows = []
    started = time.monotonic()
    for grammar, entry in registry.items():
        if args.grammar and grammar != args.grammar:
            continue
        selected = [item for item in inputs if item['grammar'] == grammar]
        if args.files_per_grammar:
            selected = selected[:args.files_per_grammar]
        sources = output / f'{grammar}.list'
        paths = [(bundle / 'corpus' / item['path']).resolve() for item in selected]
        # The C probe emits these paths as JSON strings without escaping.
        assert all(not any(c in str(path) for c in '\\"\n\r\t') for path in paths)
        assert all(digest(path) == item['sha256'] for path, item in zip(paths, selected))
        assert digest(Path(entry['library'])) == entry['library_sha256']
        sources.write_text(''.join(str(path) + '\n' for path in paths))
        profile = 0 if not args.profile else ('byte', 'point', 'before-byte', 'before-point').index(args.profile) + 1
        command = ['taskset', '-c', str(args.cpu), str(binary), entry['library'], entry['symbol'],
                   str(sources), str(args.rounds), str(profile)]
        if profile:
            command = ['perf', 'record', '-q', '-e', args.perf_event, '-F', '1999',
                       '-o', str(output / f'{grammar}.perf'), '--', *command]
        with (output / f'{grammar}.jsonl').open('w') as result, (output / f'{grammar}.log').open('w') as log:
            subprocess.run(command, stdout=result, stderr=log, check=True)
        metadata['operations'].append({'grammar': grammar, 'files': len(selected), 'command': command})
        (output / 'manifest.json').write_text(json.dumps(metadata, indent=2) + '\n')
        if not profile:
            rows = [dict(json.loads(line), grammar=grammar)
                    for line in (output / f'{grammar}.jsonl').read_text().splitlines()]
            all_rows.extend(rows)
        print(grammar, len(selected), 'files done;', round(time.monotonic() - started, 1), 'seconds', flush=True)

    if all_rows:
        summary = {'all': summarize(all_rows), 'languages': {grammar: summarize(
            [row for row in all_rows if row['grammar'] == grammar])
            for grammar in sorted({row['grammar'] for row in all_rows})}}
        (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
        print(json.dumps(summary['all'], indent=2))

    metadata['finished'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    metadata['seconds'] = time.monotonic() - started
    (output / 'manifest.json').write_text(json.dumps(metadata, indent=2) + '\n')


if __name__ == '__main__':
    main()
