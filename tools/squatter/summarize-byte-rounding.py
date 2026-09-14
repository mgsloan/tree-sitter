#!/usr/bin/env python3
"""Summarize paired byte-rounding runs (elapsed ratios below one are faster)."""
import argparse
import json
import math
import statistics as stats
from collections import defaultdict
from pathlib import Path

def geometric(values):
    return math.exp(stats.mean(math.log(x) for x in values))

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('runs', nargs='+', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--audit-output', type=Path)
    parser.add_argument('--keep-all', action='store_true', help='Sensitivity check without control-based block rejection')
    args = parser.parse_args()
    rows = []
    for path in args.runs:
        for r in json.loads(path.read_text())['records']:
            rows.append(dict(r, run=str(path)))
    # Reject an entire paired block when its identical-layout control reveals
    # a large timing disturbance. Keep the original raw files for audit.
    blocks = defaultdict(dict)
    for row in rows:
        blocks[(row['run'], row['round'], row['points'], row['case'])][row['variant']] = row['measured']
    rejected = {}
    for key, block in blocks.items():
        if 'exact' not in block or 'r7' not in block: continue
        base, control = block['exact'], block['r7']
        assert (base['symbol_bits'], base['field_bits'], base['retained_bytes']) == (control['symbol_bits'], control['field_bits'], control['retained_bytes'])
        ratios = {m: stats.median(control['modes'][m]['us']) / stats.median(base['modes'][m]['us'])
                  for m in base['modes']}
        if sum(not .9 <= r <= 1.1 for r in ratios.values()) >= 2 or any(not .75 <= r <= 4/3 for r in ratios.values()):
            rejected[key] = ratios
    if args.keep_all: rejected.clear()
    rows = [r for r in rows if (r['run'], r['round'], r['points'], r['case']) not in rejected]
    accepted = defaultdict(int)
    for key in blocks:
        if key not in rejected: accepted[(key[2], key[3])] += 1
    if args.audit_output:
        args.audit_output.write_text(json.dumps(dict(
            rule="No rejection (--keep-all)" if args.keep_all else "Reject whole block: r7/exact outside 0.9..1.1 for >=2 modes, or outside 0.75..4/3 for any mode",
            rejected=[dict(run=k[0], round=k[1], points=k[2], case=k[3], control_ratios=v) for k,v in rejected.items()],
            accepted=[dict(points=k[0], case=k[1], blocks=v) for k,v in sorted(accepted.items())]), indent=2)+'\n')
    print(f'Accepted {len(blocks)-len(rejected)}/{len(blocks)} paired blocks')
    measured = defaultdict(list)
    for row in rows:
        measured[(row['points'], row['case'], row['variant'])].append(row['measured'])
    output = {}
    for point in sorted({r['points'] for r in rows}):
        result = {}
        for variant in sorted({r['variant'] for r in rows}):
            if variant == 'exact': continue
            cases = []
            for p, case, v in measured:
                if p != point or v != variant or (p, case, 'exact') not in measured: continue
                base = measured[(p, case, 'exact')]; after = measured[(p, case, variant)]
                assert len({(a['nodes'], a['retained_bytes'], a['compact_bytes']) for a in after}) == 1
                assert after[0]['nodes'] == base[0]['nodes']
                for mode in after[0]['modes']:
                    assert {a['modes'][mode]['checksum'] for a in after} == {a['modes'][mode]['checksum'] for a in base}
                paired = [b for k,b in blocks.items() if k not in rejected and k[2:]==(p,case)
                          and variant in b and 'exact' in b]
                if not paired: continue
                ratios = {m: stats.median(stats.median(b[variant]['modes'][m]['us']) /
                                         stats.median(b['exact']['modes'][m]['us']) for b in paired)
                          for m in after[0]['modes']}
                cases.append(dict(case=case, before=[base[0]['symbol_bits'], base[0]['field_bits']],
                                  after=[after[0]['symbol_bits'], after[0]['field_bits']],
                                  ratios=ratios, retained_ratio=after[0]['retained_bytes']/base[0]['retained_bytes'],
                                  compact_ratio=after[0]['compact_bytes']/base[0]['compact_bytes']))
            if not cases: continue
            groups = defaultdict(list)
            for c in cases: groups[c['case'].split('-')[0]].append(c)
            balanced = {m: geometric(geometric(c['ratios'][m] for c in group) for group in groups.values())
                        for m in cases[0]['ratios']}
            result[variant] = dict(cases=cases, grammar_geomean=balanced,
                                  grammar_mean_retained_ratio=stats.mean(stats.mean(c['retained_ratio'] for c in g) for g in groups.values()), geomean={m: geometric(c['ratios'][m] for c in cases) for m in cases[0]['ratios']},
                                  mean_retained_ratio=stats.mean(c['retained_ratio'] for c in cases))
        output['p'+str(point)] = result
    if args.output: args.output.write_text(json.dumps(output, indent=2)+'\n')
    for points, variants in output.items():
        modes = list(next(iter(variants.values()))['grammar_geomean'])
        print(points, 'variant', *modes, 'retained')
        for variant, result in variants.items():
            print(f'{variant:8}', ' '.join(f'{result["grammar_geomean"][m]:6.3f}' for m in modes), f'{result["grammar_mean_retained_ratio"]:7.3f}')

if __name__ == '__main__': main()
