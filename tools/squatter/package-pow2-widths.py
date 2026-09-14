#!/usr/bin/env python3
"""Package validated power-of-two jobs and immutable binaries for GCP.

Run from the repository root. Copy only when tar creation has completed.
"""
import json,hashlib,shutil,tarfile
from pathlib import Path
b=Path('build/pow2-widths');d=Path('build/pow2-widths-cloud');remote='/home/mgsloan/pow2-widths-20260914'
for sub in ['jobs','inputs','grammars','queries']:(d/sub).mkdir(parents=True,exist_ok=True)
jobs=json.loads((b/'jobs.json').read_text());widths=json.loads((b/'grammar-widths.json').read_text());selected={};walks={}
for j in jobs:
 for item,folder in [(j['grammar'],'grammars'),(j['query'],'queries')]+[(s,'inputs') for s in j['sources']]:
  key='library' if folder=='grammars' else 'path';p=Path(item[key]);h=hashlib.sha256(p.read_bytes()).hexdigest();assert h==item['library_sha256' if folder=='grammars' else 'sha256']
  name=h+p.suffix;shutil.copy2(p,d/folder/name);item[key]=remote+'/'+folder+'/'+name
 w=widths[j['grammar_name']];vs=['exact','control']
 for col in ['field','symbol','super']:
  bits=w[col+'_bits']
  for target in [2,4,8,16]:
   if 0<bits<target:vs.append(col+str(target))
 selected[j['name']]=vs
 (d/'jobs'/(j['name']+'.json')).write_text(json.dumps(j,indent=2)+'\n')
 case=j.get('case',j['name'].replace('-highlights','').replace('-tags',''));walk={k:v for k,v in j.items() if k not in ['kind','query']};walk['name']=case
 walks[case]=walk;selected[case]=vs
variants=sorted(set(v for vs in selected.values() for v in vs));m=json.loads((b/'build-manifest.json').read_text())
for v in variants:
 for pt in [1,0]:assert f'{v}-p{pt}' in m['binaries'],v
for name,data in [('jobs',jobs),('walk-jobs',list(walks.values())),('job-variants',selected),('grammar-widths',widths)]: (d/(name+'.json')).write_text(json.dumps(data,indent=2)+'\n')
shutil.copy2(b/'build-manifest.json',d/'build-manifest.json');shutil.copy2('tools/squatter/benchmark-kernel-probes.py',d)
script=f'''#!/bin/bash
set -euo pipefail
cd {remote}
python3 benchmark-kernel-probes.py . --kind query --variants {','.join(variants)} --job-variants job-variants.json --points 1,0 --rounds 3 --repeat 5 --tag queries
python3 benchmark-kernel-probes.py . --kind walk --variants {','.join(variants)} --job-variants job-variants.json --points 1,0 --rounds 3 --repeat 5 --tag walks
touch complete
'''
(d/'run.sh').write_text(script)
with tarfile.open('build/pow2-widths-cloud.tar.gz','w:gz') as t:
 for p in d.rglob('*'):
  if p.is_file():t.add(p,arcname='pow2-widths-20260914/'+str(p.relative_to(d)))
 for v in variants:
  for pt in [1,0]:
   for name in ['query-workload','walk','probe.patch']:t.add(b/'binaries'/f'{v}-p{pt}'/name,arcname=f'pow2-widths-20260914/binaries/{v}-p{pt}/{name}')
print(len(jobs),'query jobs',len(walks),'walk jobs',variants)
