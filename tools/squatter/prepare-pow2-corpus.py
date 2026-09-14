#!/usr/bin/env python3
"""Build additional local corpus grammars and deterministic width-coverage jobs."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT.parent.parent/'code-corpora'
OUT = ROOT/'build/pow2-widths'
SELECTION = {'ini': ('ini', 'queries/highlights.scm'),
             'thrift': ('thrift', 'queries/highlights.scm'),
             'bibtex': ('bib', 'queries/highlights.scm'),
             'scss': ('scss', 'queries/highlights.scm'),
             'openscad': ('scad', 'queries/highlights.scm'),
             'xml/xml': ('xml', '../queries/xml/highlights.scm'),
             'glsl': ('glsl', 'queries/highlights.scm'),
             'nu': ('nu', 'queries/nu/highlights.scm'),
             'systemverilog': ('sv', 'queries/highlights.scm'),
             'java': ('java', 'queries/highlights.scm')}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    for name in ['grammars', 'jobs', 'queries']:
        (OUT/name).mkdir(exist_ok=True)
    inventory_path = OUT/'inventory.json'
    if not inventory_path.exists():
        inventory = []
        paths = subprocess.check_output(['rg', '--files', str(CORPUS/'grammars')], text=True)
        for filename in paths.splitlines():
            parser = Path(filename)
            if parser.name != 'parser.c':
                continue
            text = parser.read_text(errors='replace')
            row = {key: int(value) for key, value in re.findall(
                r'^#define (LANGUAGE_VERSION|SYMBOL_COUNT|ALIAS_COUNT|FIELD_COUNT|SUPERTYPE_COUNT) (\d+)', text, re.M)}
            if 'SYMBOL_COUNT' not in row:
                continue
            row.update(path=str(parser.resolve()), name=str(parser.parent.parent.relative_to(CORPUS/'grammars')),
                       field_bits=row.get('FIELD_COUNT', 0).bit_length(),
                       symbol_bits=(row['SYMBOL_COUNT']+row.get('ALIAS_COUNT', 0)+1).bit_length(),
                       super_count=len(re.findall(r'\.supertype\s*=\s*true', text)))
            inventory.append(row)
        inventory_path.write_text(json.dumps(inventory, indent=2)+'\n')
    inventory = json.loads(inventory_path.read_text())
    if not (OUT/'corpus-files.txt').exists():
        (OUT/'corpus-files.txt').write_bytes(subprocess.check_output(
            ['rg', '--files', str(CORPUS/'train'), str(CORPUS/'test')]))
    extensions = {'.'+item[0] for item in SELECTION.values()}
    files = [Path(p).resolve() for p in (OUT/'corpus-files.txt').read_text().splitlines() if Path(p).suffix in extensions]
    jobs = json.loads((ROOT/'build/highlight-rounding/jobs.json').read_text())
    jobs += json.loads((ROOT/'build/highlight-rounding/full-tags-jobs.json').read_text())
    provenance = []
    for name, (extension, query_path) in SELECTION.items():
        row = next(r for r in inventory if r['name'] == name)
        parser = Path(row['path'])
        grammar = parser.parent.parent
        label = name.replace('/', '-')
        library = OUT/'grammars'/f'{label}.so'
        source = parser.read_text()
        symbol = re.search(r'const TSLanguage \*(tree_sitter_\w+)\(', source).group(1)
        inputs = [parser]
        scanner = next(iter(sorted(parser.parent.glob('scanner.*'))), None)
        if scanner:
            inputs.append(scanner)
        objects = []
        commands = []
        for item in inputs:
            obj = OUT/'grammars'/f'{label}-{item.stem}.o'
            cmd = ['c++' if item.suffix in ('.cc', '.cpp') else 'cc', '-O2', '-fPIC', '-I'+str(parser.parent), '-c', str(item), '-o', str(obj)]
            subprocess.run(cmd, check=True, capture_output=True)
            objects.append(obj)
            commands.append(cmd)
        cmd = ['c++', '-shared', *map(str, objects), '-o', str(library)]
        subprocess.run(cmd, check=True, capture_output=True)
        commands.append(cmd)
        candidates = []
        for path in files:
            if path.suffix != '.'+extension:
                continue
            try:
                size = path.stat().st_size
            except OSError:
                continue
            if 1024 <= size <= 2*1024*1024:
                candidates.append((size, str(path)))
        candidates.sort()
        assert candidates, name
        # A batch spread across the size distribution, plus the largest file.
        selected = [candidates[i*len(candidates)//4][1] for i in [1, 2, 3]]
        selected = list(dict.fromkeys(selected))
        query = (grammar/query_path).resolve()
        kinds = [('highlights', query)]
        tags = grammar/'queries/tags.scm'
        if tags.exists():
            kinds.append(('tags', tags))
        for size_class, paths in [('small', selected), ('large', [candidates[-1][1]])]:
            for kind, query in kinds:
                job = dict(name=f'{label}-{size_class}-{kind}', kind=kind, grammar_name=label,
                           grammar=dict(library=str(library), symbol=symbol, library_sha256=digest(library)),
                           query=dict(name=f'{label}-{kind}', path=str(query), sha256=digest(query)),
                           sources=[dict(path=p, sha256=digest(Path(p))) for p in paths],
                           case=f'{label}-{size_class}', size_class=size_class)
                jobs.append(job)
        provenance.append(dict(grammar=name, metadata=row, commands=commands,
                               inputs=[dict(path=str(p), sha256=digest(p)) for p in inputs],
                               candidates=len(candidates), largest_bytes=candidates[-1][0]))
        print('prepared', name, flush=True)
    for job in jobs:
        (OUT/'jobs'/(job['name']+'.json')).write_text(json.dumps(job, indent=2)+'\n')
    (OUT/'jobs.json').write_text(json.dumps(jobs, indent=2)+'\n')
    (OUT/'corpus-provenance.json').write_text(json.dumps(provenance, indent=2)+'\n')


if __name__ == '__main__':
    main()
