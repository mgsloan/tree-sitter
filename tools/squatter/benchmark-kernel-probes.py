#!/usr/bin/env python3
"""Paired real-query/full-walk kernel probes on a prepared benchmark host.

The bundle reuses hashed inputs and full highlighting/tags jobs from the query
workload experiment. Query timers include text predicates; parsing and query
compilation are outside the measured region. Walks visit every visible node.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import subprocess
import time


def digest(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def save(path, result):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(result, indent=2)+'\n')
    temporary.replace(path)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('bundle', type=Path)
    p.add_argument('--kind', choices=['query', 'walk'], required=True)
    p.add_argument('--variants', required=True)
    p.add_argument('--points', default='1')
    p.add_argument('--rounds', type=int, default=3)
    p.add_argument('--round-start', type=int, default=0)
    p.add_argument('--repeat', type=int, default=5)
    p.add_argument('--jobs', default='')
    p.add_argument('--tag', required=True)
    p.add_argument('--loader', default='/lib64/ld-linux-x86-64.so.2')
    p.add_argument('--cpu', type=int, default=0)
    a = p.parse_args()
    assert a.rounds > 0 and a.repeat > 0
    b = a.bundle.resolve()
    output = b/(a.tag+'.json')
    assert not output.exists()
    jobs = json.loads((b/('jobs.json' if a.kind == 'query' else 'walk-jobs.json')).read_text())
    if a.jobs:
        jobs = [j for j in jobs if j['name'] in a.jobs.split(',')]
    assert jobs
    variants = a.variants.split(',')
    assert 'exact' in variants
    points = list(map(int, a.points.split(',')))
    build = json.loads((b/'build-manifest.json').read_text())
    binary_name = 'query-workload' if a.kind == 'query' else 'walk'
    hashes = {f'{v}-p{pt}': digest(b/'binaries'/f'{v}-p{pt}'/binary_name) for v in variants for pt in points}
    for key, value in hashes.items():
        assert value == build['binaries'][key]['binary_sha256' if a.kind == 'query' else 'walk_sha256']
    for j in jobs:
        assert digest(Path(j['grammar']['library'])) == j['grammar']['library_sha256']
        for s in j['sources']:
            assert digest(Path(s['path'])) == s['sha256']
    result = dict(kind=a.kind, started=time.time(), cpu=a.cpu, cpu_description=subprocess.check_output(['lscpu','--json'], text=True),
                  build_manifest_sha256=digest(b/'build-manifest.json'), binaries=hashes, jobs=jobs, records=[])
    for r in range(a.round_start, a.round_start+a.rounds):
        for point in points:
            for ji, j in enumerate(jobs):
                order = variants.copy()
                random.Random(5119+r*1000+point*100+ji).shuffle(order)
                checks = []
                for v in order:
                    binary = b/'binaries'/f'{v}-p{point}'/binary_name
                    cmd = ['taskset','-c',str(a.cpu),a.loader,str(binary)]
                    if a.kind == 'query':
                        cmd += [str(b/'jobs'/(j['name']+'.json')),'--repeat',str(a.repeat)]
                        if r > 0:
                            cmd.append('--skip-validation')
                    else:
                        cmd += [j['grammar']['library'],j['grammar']['symbol'],str(a.repeat),*[s['path'] for s in j['sources']]]
                    run = subprocess.run(cmd, capture_output=True, text=True, timeout=300,
                                         env={**os.environ, 'SQ_WALK_ONLY': '1'})
                    row = dict(round=r, points=point, job=j['name'], variant=v, command=cmd, returncode=run.returncode, stderr=run.stderr)
                    if not run.returncode:
                        row['measured'] = m = json.loads(run.stdout)
                        if a.kind == 'query':
                            assert m['points'] == bool(point)
                        checks.append(m['squat']['counts'] if a.kind == 'query' else {k: z['checksum'] for k,z in m['modes'].items()})
                    else:
                        row['stdout'] = run.stdout
                    result['records'].append(row)
                    save(output, result)
                    assert not run.returncode, (j['name'],v,run.stderr)
                assert all(c == checks[0] for c in checks), (j['name'],checks)
                print(a.kind,r,point,j['name'],flush=True)
    result['finished'] = time.time()
    save(output, result)


if __name__ == '__main__':
    main()
