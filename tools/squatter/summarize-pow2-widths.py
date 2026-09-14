#!/usr/bin/env python3
"""Report each observed source-to-target width transition independently.

Input summaries come from summarize-kernel-probes.py --allow-layout-changes.
Balance jobs within grammars, then grammars within each transition.
"""
import argparse
from collections import defaultdict
import json
import math
from pathlib import Path
import re
import statistics


def geometric(values):
    return math.exp(statistics.mean(math.log(value) for value in values))


def balanced(rows):
    groups = defaultdict(list)
    for grammar, ratio in rows:
        groups[grammar].append(ratio)
    return 100*(geometric(geometric(values) for values in groups.values())-1) if groups else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('bundle', type=Path)
    args = parser.parse_args()
    bundle = args.bundle
    widths = json.loads((bundle/'grammar-widths.json').read_text())
    query = json.loads((bundle/'query-summary.json').read_text())
    walk = json.loads((bundle/'walk-summary.json').read_text())
    raw = json.loads((bundle/'walks.json').read_text())
    jobs = {job['name']: job for job in raw['jobs']}
    layout = {(row['points'], row['job'], row['variant']): row['measured'] for row in raw['records'] if not row['returncode']}
    transitions = defaultdict(set)
    for (point, job, variant), measured in layout.items():
        grammar = jobs[job]['grammar_name']
        original = widths[grammar]
        changed = re.fullmatch(r'(field|symbol|super)(\d+)', variant)
        for column in ['field', 'symbol', 'super']:
            expected = original[column+'_bits']
            if changed and changed[1] == column and expected:
                expected = max(expected, int(changed[2]))
            key = 'supertype_bits' if column == 'super' else column+'_bits'
            assert measured[key] == expected, (job, variant, key, measured[key], expected)
        if changed:
            column, target = changed[1], int(changed[2])
            source = original[column+'_bits']
            assert 0 < source < target
            transitions[(column, source, target)].add(grammar)
    result = []
    for point in [1, 0]:
        for (column, source, target), grammars in sorted(transitions.items()):
            variant = column+str(target)
            row = dict(points=point, column=column, source=source, target=target, grammars=sorted(grammars))
            for kind, operation, summary in [('highlights', 'query', query), ('tags', 'query', query),
                                             ('walk', 'cursor_walk', walk), ('walk', 'uncached', walk),
                                             ('walk', 'cached', walk), ('walk', 'membership_walk', walk)]:
                key = kind if kind != 'walk' else operation
                row[key] = balanced((r['grammar'], r['ratio']) for r in summary['rows']
                                    if r['points'] == point and r['variant'] == variant and r['grammar'] in grammars
                                    and r['kind'] == kind and r['operation'] == operation)
            row['slab'] = balanced((jobs[job]['grammar_name'], measured['slab_bytes']/layout[(point, job, 'exact')]['slab_bytes'])
                                   for (pt, job, v), measured in layout.items()
                                   if pt == point and v == variant and jobs[job]['grammar_name'] in grammars)
            result.append(row)
    (bundle/'transition-tables.json').write_text(json.dumps(result, indent=2)+'\n')
    lines = []
    def percent(value):
        return '—' if value is None else f'{value:+.1f}%'
    for point in [1, 0]:
        lines += ['## '+('Points enabled' if point else 'Byte-only'), '']
        for column in ['field', 'symbol', 'super']:
            lines += ['### '+column, '', '| Transition | Highlight | Tags | Cursor | Uncached | Cached | Membership | Slab | Grammars |',
                      '|---|---:|---:|---:|---:|---:|---:|---:|---|']
            for row in result:
                if row['points'] == point and row['column'] == column:
                    lines.append(f"| {row['source']}→{row['target']} | "+' | '.join(percent(row[key]) for key in
                        ['highlights', 'tags', 'cursor_walk', 'uncached', 'cached', 'membership_walk', 'slab'])+' | '+', '.join(row['grammars'])+' |')
            lines.append('')
    (bundle/'transition-tables.md').write_text('\n'.join(lines)+'\n')


if __name__ == '__main__':
    main()
