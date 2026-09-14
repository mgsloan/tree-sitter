#!/usr/bin/env python3
"""Select clean parses and translate the recorded queries for the Rust harness.

Run from the repository root after prepare-pow2-corpus.py and baseline validation.
Lua dot matches newlines; (?s) preserves that behavior in the regex equivalents.
Only display-priority directives are removed.
"""
import json,re,subprocess,hashlib
from pathlib import Path
b=Path('build/pow2-widths');jobs=json.loads((b/'jobs.json').read_text());initial=b/'local-validation.json'
if initial.exists() and not (b/'initial-validation.json').exists():
 (b/'initial-validation.json').write_text(initial.read_text())
extensions=dict(ini='ini',thrift='thrift',bibtex='bib',scss='scss',openscad='scad',**{'xml-xml':'xml'},glsl='glsl',nu='nu',systemverilog='sv',java='java')
files=Path('build/pow2-widths/corpus-files.txt').read_text().splitlines();audit=[]
for grammar,ext in extensions.items():
 related=[j for j in jobs if j['grammar_name']==grammar];j=related[0];paths=[]
 for p in files:
  if p.endswith('.'+ext):
   p=Path(p).resolve()
   try:size=p.stat().st_size
   except OSError:continue
   if 1024<=size<=2*1024*1024:paths.append((size,str(p)))
 paths=sorted(set(paths));paths=paths[-100:]
 cmd=[str(b/'parse-check'),j['grammar']['library'],j['grammar']['symbol']]+[p for _,p in paths]
 r=subprocess.run(cmd,capture_output=True,text=True,timeout=120);assert r.returncode==0,(grammar,r.stderr)
 flags=list(map(int,r.stdout.split()));good=[p for p,f in zip(paths,flags) if not f]
 print(grammar,len(good),'/',len(paths),flush=True)
 audit.append(dict(grammar=grammar,candidates=[dict(path=p,size=s,has_errors=f) for (s,p),f in zip(paths,flags)],clean=len(good)))
 if not good:
  jobs=[j for j in jobs if j['grammar_name']!=grammar];continue
 small=list(dict.fromkeys(good[i*len(good)//4][1] for i in [1,2,3]));large=[good[-1][1]]
 for j in related:
  j['sources']=[dict(path=p,sha256=hashlib.sha256(Path(p).read_bytes()).hexdigest()) for p in (small if j['size_class']=='small' else large)]
  q=Path(j['query']['path']);text=q.read_text()
  if '#lua-match?' in text or '#set!' in text:
   changed=re.sub(r'\(#lua-match\?\s+(@[\w.]+)\s+"([^"]*)"\)',lambda m:f'(#match? {m[1]} "(?s){m[2]}")',text)
   changed=re.sub(r'\(#set!\s+"priority"\s+\d+\)','',changed)
   out=b/'queries'/(grammar+'-'+j['kind']+'.scm');out.write_text(changed)
   j['query'].update(original_path=str(q),original_sha256=hashlib.sha256(q.read_bytes()).hexdigest(),adaptation='Equivalent Lua-pattern-to-regex predicates; omit display priority directives.',path=str(out.resolve()),sha256=hashlib.sha256(out.read_bytes()).hexdigest())
for j in jobs:(b/'jobs'/(j['name']+'.json')).write_text(json.dumps(j,indent=2)+'\n')
(b/'jobs.json').write_text(json.dumps(jobs,indent=2)+'\n');(b/'selection-audit.json').write_text(json.dumps(audit,indent=2)+'\n')
