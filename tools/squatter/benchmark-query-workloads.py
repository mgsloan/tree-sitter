#!/usr/bin/env python3
"""Run uploaded real highlighting/tags query jobs with fixed rounding binaries.

A bundle contains jobs.json, build-manifest.json, and binaries/POLICY-pN/query-workload.
Jobs name hashed grammar/query/source files, with paths valid on the benchmark host.
"""
import argparse
import hashlib
import json
import platform
import random
import subprocess
import time
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('bundle', type=Path)
    parser.add_argument('--variants', default='exact,r7,r6,r5,r2,r5_9')
    parser.add_argument('--points', default='1,0')
    parser.add_argument('--rounds', type=int, default=3)
    parser.add_argument('--repeat', type=int, default=5)
    parser.add_argument('--cpu', type=int, default=0)
    parser.add_argument('--loader')
    parser.add_argument('--jobs', default='')
    parser.add_argument('--tag', default='cloud-queries')
    args = parser.parse_args()
    bundle = args.bundle.resolve()
    variants = args.variants.split(',')
    assert 'exact' in variants, 'exact baseline is required'
    points = list(map(int, args.points.split(',')))
    all_jobs = json.loads((bundle/'jobs.json').read_text())
    jobs = [j for j in all_jobs if not args.jobs or j['name'] in args.jobs.split(',')]
    assert jobs and args.rounds > 0 and args.repeat > 0
    output = bundle/(args.tag+'.json')
    assert not output.exists(), 'choose a fresh tag'
    binary_hashes = {f'{v}-p{p}': digest(bundle/'binaries'/f'{v}-p{p}'/'query-workload')
                     for p in points for v in variants}
    build = json.loads((bundle/'build-manifest.json').read_text())
    for key, value in binary_hashes.items():
        assert value == build['binaries'][key]['binary_sha256'], key
    records, excluded = [], {}
    result = dict(started=time.time(), cpu=args.cpu, rounds=args.rounds, repeat=args.repeat,
                  platform=platform.uname()._asdict(),
                  cpu_description=subprocess.check_output(['lscpu','--json'],text=True),
                  libc=subprocess.check_output(['ldd','--version'],text=True),
                  jobs_sha256=digest(bundle/'jobs.json'), build_manifest_sha256=digest(bundle/'build-manifest.json'),
                  binaries=binary_hashes, records=records, excluded=excluded)
    save(output, result)
    for round_number in range(args.rounds):
        for point in points:
            for job in jobs:
                key = f'p{point}/{job["name"]}'
                if key in excluded:
                    continue
                index = all_jobs.index(job)
                order = variants.copy()
                random.Random(2767+round_number*1000+point*100+index).shuffle(order)
                # First establish a correct exact-width reference for this input.
                if round_number == 0:
                    order.remove('exact'); order.insert(0,'exact')
                job_path = bundle/'jobs'/(job['name']+'.json')
                assert json.loads(job_path.read_text()) == job
                for variant in order:
                    binary = bundle/'binaries'/f'{variant}-p{point}'/'query-workload'
                    command = ['taskset','-c',str(args.cpu)] + ([args.loader] if args.loader else [])
                    command += [str(binary),str(job_path),'--repeat',str(args.repeat)]
                    if variant == 'exact': command.append('--time-mainline')
                    if round_number > 0: command.append('--skip-validation')
                    start = time.time()
                    row = dict(round=round_number, points=point, job=job['name'], variant=variant,
                               command=command, started=start)
                    try:
                        run = subprocess.run(command,text=True,capture_output=True,timeout=300)
                        row.update(returncode=run.returncode, seconds=time.time()-start)
                        if run.returncode:
                            row.update(status='failed', stderr=run.stderr, stdout=run.stdout)
                        else:
                            measured = json.loads(run.stdout)
                            assert measured['points'] == bool(point)
                            row.update(status='passed', measured=measured)
                    except subprocess.TimeoutExpired:
                        row.update(status='failed', seconds=time.time()-start, stderr='300-second process timeout')
                    records.append(row)
                    if row['status'] == 'failed':
                        # Exclude this workload/configuration from every policy comparison,
                        # retaining earlier records and the complete failure diagnostic.
                        excluded[key] = dict(variant=variant, round=round_number, reason=row['stderr'])
                        save(output, result)
                        print('EXCLUDED',key,variant,row['stderr'][:500],flush=True)
                        break
                    save(output, result)
                print(f'round {round_number+1}: p{point} {job["name"]}',flush=True)
    result['finished'] = time.time()
    save(output, result)


if __name__ == '__main__':
    main()
