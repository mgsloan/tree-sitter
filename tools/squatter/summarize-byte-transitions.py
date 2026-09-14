#!/usr/bin/env python3
"""Isolate individual width transitions from the paired GCP rounding artifact."""
import argparse
import json
import math
import statistics as stats
from collections import defaultdict
from pathlib import Path


def geometric(values):
    return math.exp(stats.mean(math.log(x) for x in values))


def summarize(artifact):
    rejected = {(Path(r['run']).name, r['round'], r['points'], r['case'])
                for r in artifact['audit']['rejected']}
    blocks = defaultdict(dict)
    for name, run in artifact['runs'].items():
        for row in run['records']:
            key = (name, row['round'], row['points'], row['case'])
            if key not in rejected:
                blocks[key][row['variant']] = row['measured']
    # Each pair holds the other cutoff fixed. Restrict to inputs whose only
    # changed column width is the requested transition.
    pairs = {2: ('r5', 'r2'), 5: ('r6', 'r5'), 6: ('r7', 'r6'),
             9: ('r5', 'r5_9'), 10: ('r5', 'r5_9')}
    result = []
    for width in [*range(2, 8), *range(9, 16)]:
        stored = 8 if width < 8 else 16
        entry = dict(required=width, stored=stored,
                     asymptotic_column_ratio=(64 // width) / (64 // stored), points={})
        if width not in pairs:
            result.append(entry)
            continue
        before, after = pairs[width]
        entry['before_variant'], entry['after_variant'] = before, after
        for point in [1, 0]:
            cases = defaultdict(list)
            for key, block in blocks.items():
                if key[2] != point or before not in block or after not in block:
                    continue
                a, b = block[before], block[after]
                changed = {(a[f], b[f]) for f in ('symbol_bits', 'field_bits') if a[f] != b[f]}
                if changed != {(width, stored)}:
                    continue
                assert a['nodes'] == b['nodes']
                for mode in a['modes']:
                    assert a['modes'][mode]['checksum'] == b['modes'][mode]['checksum']
                cases[key[3]].append(dict(
                    ratios={m: stats.median(b['modes'][m]['us']) / stats.median(a['modes'][m]['us'])
                            for m in a['modes']},
                    memory=b['retained_bytes'] / a['retained_bytes']))
            groups = defaultdict(list)
            detail = []
            for name, rows in sorted(cases.items()):
                assert len(rows) == 3, (width, point, name, len(rows))
                case = dict(case=name, blocks=len(rows),
                            ratios={m: stats.median(r['ratios'][m] for r in rows) for m in rows[0]['ratios']},
                            retained_ratio=rows[0]['memory'])
                detail.append(case)
                groups[name.split('-')[0]].append(case)
            assert detail
            entry['points'][str(point)] = dict(cases=detail, grammars=sorted(groups),
                ratios={m: geometric(geometric(c['ratios'][m] for c in g) for g in groups.values())
                        for m in detail[0]['ratios']},
                retained_ratio=stats.mean(stats.mean(c['retained_ratio'] for c in g) for g in groups.values()))
        result.append(entry)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('artifact', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = summarize(json.loads(args.artifact.read_text()))
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    for entry in result:
        print(f'{entry["required"]}→{entry["stored"]}', entry['points'])


if __name__ == '__main__':
    main()
