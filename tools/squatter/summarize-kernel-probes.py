#!/usr/bin/env python3
"""Summarize paired kernel probes, balancing files within grammars.

Reject a whole paired block if its identical-code control differs by >15% in
any timed operation. Keep raw records and report rejected blocks; --keep-all
provides the unfiltered sensitivity analysis. Negative percentages are faster.
"""
import argparse
from collections import defaultdict
import json
import math
from pathlib import Path
import statistics as st


def gm(values):
    return math.exp(st.mean(math.log(x) for x in values))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('runs', nargs='+', type=Path)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--keep-all', action='store_true')
    a = p.parse_args()
    ratios, absolute = defaultdict(list), defaultdict(list)
    jobs, rejected = {}, []
    blocks_seen = 0
    for path in a.runs:
        run = json.loads(path.read_text())
        jobs.update({j['name']: j for j in run['jobs']})
        blocks = defaultdict(dict)
        for r in run['records']:
            assert r['returncode'] == 0, (path, r['job'], r['variant'])
            blocks[(r['points'], r['job'], r['round'])][r['variant']] = r['measured']
        for (point, job, rnd), block in blocks.items():
            if 'exact' not in block or 'control' not in block:
                continue
            blocks_seen += 1
            operations = ['query'] if run['kind'] == 'query' else list(block['exact']['modes'])
            def timing(m, op):
                return st.median(m['squat']['cpu_ms']) if op == 'query' else st.median(m['modes'][op]['us']) / 1000
            def check(m, op):
                return m['squat']['counts'] if op == 'query' else m['modes'][op]['checksum']
            controls = {op: timing(block['control'], op)/timing(block['exact'], op) for op in operations}
            bad = any(not .85 <= ratio <= 1.15 for ratio in controls.values())
            if bad:
                rejected.append(dict(run=str(path), point=point, job=job, round=rnd, controls=controls))
                if not a.keep_all:
                    continue
            for variant, measured in block.items():
                assert measured['slab_bytes'] == block['exact']['slab_bytes']
                for op in operations:
                    assert check(measured, op) == check(block['exact'], op)
                    key = (point, job, variant, op)
                    absolute[key].append(timing(measured, op))
                    ratios[key].append(timing(measured, op)/timing(block['exact'], op))
    rows = [dict(points=p, job=j, variant=v, operation=o, grammar=jobs[j]['grammar_name'],
                 kind=jobs[j].get('kind', 'walk'), samples=len(vals), ms=st.median(absolute[(p,j,v,o)]),
                 ratio=st.median(vals), percent=100*(st.median(vals)-1)) for (p,j,v,o),vals in sorted(ratios.items())]
    groups = defaultdict(lambda: defaultdict(list))
    for r in rows:
        groups[(r['points'],r['kind'],r['variant'],r['operation'])][r['grammar']].append(r['ratio'])
    aggregate = [dict(points=p, kind=k, variant=v, operation=o, percent=100*(gm(gm(values) for values in grammars.values())-1))
                 for (p,k,v,o),grammars in sorted(groups.items())]
    a.output.write_text(json.dumps(dict(blocks_seen=blocks_seen, rejected=rejected, keep_all=a.keep_all,
                                       rows=rows, aggregate=aggregate),indent=2)+'\n')


if __name__ == '__main__':
    main()
