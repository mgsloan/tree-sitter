#!/usr/bin/env python3
"""Summarize real-query timings, retaining separate highlighting/tags transitions."""
import argparse
from collections import defaultdict
import json
import math
from pathlib import Path
import statistics as stats

POLICIES = {'exact': (8,16), 'r7': (7,13), 'r6': (6,13), 'r5': (5,13), 'r2': (2,13), 'r5_9': (5,9)}
PAIRS = {2: ('r5','r2'), 5: ('r6','r5'), 6: ('r7','r6'), 9: ('r5','r5_9'), 10: ('r5','r5_9')}


def geometric(values):
    return math.exp(stats.mean(math.log(v) for v in values))


def stored(width, policy):
    lo, hi = POLICIES[policy]
    return 8 if lo <= width <= 8 else 16 if width >= hi else width


def balanced(cases, value):
    groups = defaultdict(list)
    for case in cases:
        groups[case['grammar']].append(value(case))
    return geometric(geometric(values) for values in groups.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('runs', nargs='+', type=Path)
    parser.add_argument('--widths', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--keep-all', action='store_true')
    args = parser.parse_args()
    widths = json.loads(args.widths.read_text())
    jobs, blocks, excluded = {}, defaultdict(dict), {}
    for path in args.runs:
        for j in json.loads((path.parent/'jobs.json').read_text()):
            if j['kind']=='tags' and j['grammar_name'] in ('tsx','typescript') and '-full-tags' not in j['name']:
                continue  # Supplemental-only TS queries remain in raw data, not the main suite.
            assert j['grammar']['library_sha256'] == widths[j['grammar_name']]['library_sha256']
            jobs[j['name']] = j
        run = json.loads(path.read_text())
        excluded.update(run['excluded'])
        for row in run['records']:
            if row['job'] in jobs and row['status']=='passed':
                blocks[(str(path),row['round'],row['points'],row['job'])][row['variant']] = row['measured']
    excluded = {k: v for k, v in excluded.items() if k.partition("/")[2] in jobs}
    rejected, accepted = [], defaultdict(list)
    for key, block in blocks.items():
        if f'p{key[2]}/{key[3]}' in excluded or set(block) != set(POLICIES):
            continue
        base, control = block['exact'], block['r7']
        assert base['slab_bytes']==control['slab_bytes']
        assert len({json.dumps(m['squat']['counts'],sort_keys=True) for m in block.values()})==1
        assert base['mainline']['counts']==base['squat']['counts']
        ratio = stats.median(control['squat']['cpu_ms'])/stats.median(base['squat']['cpu_ms'])
        if not args.keep_all and not .85 <= ratio <= 1.15:
            rejected.append(dict(run=key[0],round=key[1],points=key[2],job=key[3],control_ratio=ratio))
        else:
            accepted[key[2],key[3]].append(block)
    per_job = []
    for (point,name), pairs in sorted(accepted.items()):
        job = jobs[name]
        assert len({json.dumps(p['exact']['squat']['counts'], sort_keys=True) for p in pairs}) == 1
        base = pairs[0]['exact']
        per_job.append(dict(points=point,job=name,grammar=job['grammar_name'],kind=job['kind'],
            size_class=job['size_class'],blocks=len(pairs), files=base['files'],source_bytes=base['source_bytes'],
            nodes=base['nodes'],patterns=base['patterns'],counts=base['squat']['counts'],
            mainline_ms=stats.median(stats.median(p['exact']['mainline']['cpu_ms']) for p in pairs),
            squat_ms=stats.median(stats.median(p['exact']['squat']['cpu_ms']) for p in pairs),
            ratios={v:stats.median(stats.median(p[v]['squat']['cpu_ms']) / stats.median(p['exact']['squat']['cpu_ms']) for p in pairs) for v in POLICIES}))
    overall = {}
    transitions = []
    for point in [1,0]:
        overall[str(point)] = {}
        for kind in ['highlights','tags']:
            selected=[c for c in per_job if c['points']==point and c['kind']==kind]
            if selected:
                overall[str(point)][kind]={v:balanced(selected,lambda c:c['ratios'][v]) for v in POLICIES}
    for width in [*range(2,8),*range(9,16)]:
        row=dict(required=width,stored=8 if width<8 else 16,results={})
        if width in PAIRS:
            before, after=PAIRS[width]
            row.update(before_variant=before,after_variant=after)
            for point in [1,0]:
                row['results'][str(point)]={}
                for kind in ['highlights','tags']:
                    cases=[]
                    for c in per_job:
                        if c['points']!=point or c['kind']!=kind:continue
                        actual=widths[c['grammar']]['widths']
                        changed={(stored(w,before),stored(w,after)) for w in actual if stored(w,before)!=stored(w,after)}
                        if changed!={(width,row['stored'])}:continue
                        pairs=accepted[point,c['job']]
                        ratio=stats.median(stats.median(p[after]['squat']['cpu_ms'])/stats.median(p[before]['squat']['cpu_ms']) for p in pairs)
                        cases.append(dict(grammar=c['grammar'],job=c['job'],ratio=ratio,blocks=len(pairs)))
                    if cases:
                        row['results'][str(point)][kind]=dict(ratio=balanced(cases,lambda c:c['ratio']),cases=cases)
        transitions.append(row)
    output=dict(control_rule='No rejection' if args.keep_all else 'Reject entire paired block when r7/exact is outside 0.85..1.15',
                rejected=rejected,excluded=excluded,per_job=per_job,overall=overall,transitions=transitions)
    args.output.write_text(json.dumps(output,indent=2)+'\n')
    print('Accepted',sum(c['blocks'] for c in per_job),'blocks; rejected',len(rejected),'excluded',len(excluded))
    print(json.dumps(overall,indent=2))


if __name__=='__main__':main()
