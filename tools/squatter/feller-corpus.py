import argparse
import collections
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import signal
import subprocess
import time
import tomllib


def inventory(output, corpus):
    output.mkdir(parents=True)
    (output / 'lists').mkdir()
    coverage = json.loads((corpus / 'language-coverage.json').read_text())
    grammars = tomllib.loads((corpus / 'selected-grammars.toml').read_text())['repo']
    by_symbol = collections.defaultdict(list)
    for grammar in grammars:
        grammar['parser'] = str(corpus / 'grammars' / grammar['name'] /
                                grammar.get('directory', '') / 'src/parser.c')
        grammar['files'] = 0
        parser = Path(grammar['parser'])
        if not parser.exists():
            grammar['eligibility'] = 'missing_parser'
        else:
            contents = parser.read_bytes()
            grammar['parser_sha256'] = hashlib.sha256(contents).hexdigest()
            version = re.search(rb'#define LANGUAGE_VERSION (\d+)', contents)
            external = re.search(rb'#define EXTERNAL_TOKEN_COUNT (\d+)', contents)
            entry = re.search(rb'const TSLanguage\s*\*\s*(tree_sitter_\w+)\s*\(void\)', contents)
            grammar['abi'] = int(version[1]) if version else None
            grammar['external_tokens'] = int(external[1]) if external else None
            grammar['entry'] = entry[1].decode() if entry else None
            grammar['eligibility'] = ('unsupported_abi' if grammar['abi'] != 15 else
                                      'external_scanner' if grammar['external_tokens'] else
                                      'eligible')
        by_symbol[grammar['grammar']].append(grammar)

    suffixes = collections.defaultdict(set)
    patterns = []
    unmapped = []
    for registration in coverage['registrations']:
        candidates = by_symbol[registration['grammar']]
        matching = [grammar for grammar in candidates if
                    registration['source'] in grammar['sources'] or
                    registration['path'] in grammar['sources'] or
                    (registration['source'] == 'zed' and
                     any(source.startswith('zed/') for source in grammar['sources']))]
        matching = matching or candidates
        if not matching and registration['grammar']:
            unmapped.append(registration)
        names = {grammar['name'] for grammar in matching}
        for suffix in set(registration['path_suffixes']) | set(
                coverage.get('suffix_aliases', {}).get(registration['name'], [])):
            suffixes[suffix].update(names)
        pattern = registration.get('first_line_pattern')
        if pattern and names:
            try:
                patterns.append((re.compile(pattern), names))
            except re.error as error:
                unmapped.append(dict(registration, pattern_error=str(error)))

    by_name = {grammar['name']: grammar for grammar in grammars}
    streams = {}
    counts = collections.Counter()
    for split in ['train', 'test']:
        base = corpus / split
        for parent, directories, files in os.walk(base):
            directories[:] = sorted(directory for directory in directories
                                    if directory != '.git' and not
                                    (Path(parent) / directory).is_symlink())
            for filename in sorted(files):
                path = Path(parent) / filename
                if filename == '.git' or path.is_symlink():
                    continue
                counts['files'] += 1
                relative = path.relative_to(base).as_posix()
                candidates = {relative, filename, filename.rsplit('.', 1)[-1]}
                candidates.update(relative[index + 1:] for index, character in
                                  enumerate(relative) if character == '.')
                matched = set().union(*(suffixes.get(candidate, ()) for candidate in candidates))
                if not matched:
                    try:
                        with path.open('rb') as source:
                            first_line = source.readline(4096).decode('utf-8', errors='replace')
                        for pattern, names in patterns:
                            if pattern.search(first_line):
                                matched.update(names)
                        if matched:
                            counts['first_line_matched'] += 1
                    except OSError:
                        counts['read_errors'] += 1
                if not matched:
                    counts['unmapped_files'] += 1
                    continue
                counts['mapped_files'] += 1
                for name in sorted(matched):
                    grammar = by_name[name]
                    grammar['files'] += 1
                    counts[grammar['eligibility'] + '_file_grammar_pairs'] += 1
                    if grammar['eligibility'] == 'eligible':
                        if name not in streams:
                            streams[name] = (output / 'lists' / (name + '.paths')).open('wb')
                        streams[name].write(os.fsencode(path) + b'\0')
            if counts['files'] and counts['files'] % 50000 < 10:
                print(dict(counts), flush=True)
    for stream in streams.values():
        stream.close()
    document = dict(corpus=str(corpus), counts=dict(counts), grammars=grammars,
                    unmapped_registrations=unmapped,
                    selection_sha256=hashlib.sha256((corpus / 'selected-grammars.toml').read_bytes()).hexdigest(),
                    coverage_sha256=hashlib.sha256((corpus / 'language-coverage.json').read_bytes()).hexdigest())
    (output / 'inventory.json').write_text(json.dumps(document, indent=2) + '\n')
    print(json.dumps(dict(counts)), flush=True)
    print('Eligible grammars with files:', sum(grammar['eligibility'] == 'eligible' and
                                              grammar['files'] > 0 for grammar in grammars), flush=True)


def compare(output, repository, executable):
    (output / 'grammars').mkdir(exist_ok=True)
    inventory = json.loads((output / 'inventory.json').read_text())
    summary = dict(started=time.time(), complete=False, totals={}, grammars={})
    totals = collections.Counter()

    def save():
        summary['totals'] = dict(totals)
        temporary = output / 'summary.tmp'
        temporary.write_text(json.dumps(summary, indent=2) + '\n')
        temporary.replace(output / 'summary.json')

    def limits():
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_AS, (2 * 1024**3, 2 * 1024**3))

    grammars = sorted((grammar for grammar in inventory['grammars']
                       if grammar['eligibility'] == 'eligible' and grammar['files']),
                      key=lambda grammar: (grammar['name'] != 'c', grammar['files'], grammar['name']))
    with (output / 'failures.jsonl').open('w') as failures:
        for grammar in grammars:
            name = grammar['name']
            print('grammar', name, grammar['files'], flush=True)
            state = dict(files=grammar['files'], counts={}, status='building')
            summary['grammars'][name] = state
            save()
            parser = Path(grammar['parser'])
            source = parser.read_text()
            # Compile the same generated lexer for both parsers without its optional O0 pragma.
            source = re.sub(r'^\s*#pragma (?:GCC optimize\s*\("O0"\)|clang optimize off|optimize\("", off\))\s*$',
                            '', source, flags=re.MULTILINE)
            copied = output / 'grammars' / (name + '.c')
            library = output / 'grammars' / (name + '.so')
            copied.write_text(source)
            command = ['cc', '-O1', '-fPIC', '-shared', '-I' + str(parser.parent),
                       '-I' + str(parser.parent.parent),
                       '-I' + str(repository / 'lib/tree_feller/include/tree_feller'),
                       str(copied), '-o', str(library)]
            with (output / 'grammars' / (name + '.build.log')).open('w') as build_log:
                try:
                    built = subprocess.run(command, stdout=build_log, stderr=subprocess.STDOUT,
                                           timeout=180, check=False)
                except subprocess.TimeoutExpired:
                    built = None
            if not built or built.returncode:
                state['status'] = 'build_failed'
                totals['build_failed_file_grammar_pairs'] += grammar['files']
                save()
                continue
            state['library_sha256'] = hashlib.sha256(library.read_bytes()).hexdigest()
            paths = (output / 'lists' / (name + '.paths')).read_bytes().split(b'\0')[:-1]
            counts = collections.Counter()
            next_index = 0
            state['status'] = 'running'
            with (output / (name + '.jsonl')).open('w') as log:
                while next_index < len(paths):
                    ready = False
                    command = [str(executable), str(library),
                               grammar['entry'] or 'tree_sitter_' + grammar['grammar'].replace('-', '_'),
                               str(output / 'lists' / (name + '.paths')), str(next_index)]
                    with (output / (name + '.stderr')).open('a') as errors:
                        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=errors,
                                                   text=True, preexec_fn=limits)
                        for line in process.stdout:
                            row = json.loads(line)
                            if row['status'] == 'ready':
                                ready = True
                                continue
                            if not ready:
                                state['status'] = row['status']
                                state['detail'] = row['detail']
                                break
                            next_index = row['index'] + 1
                            log.write(line)
                            counts[row['status']] += 1
                            totals[row['status']] += 1
                            if row['status'] not in ['equal', 'mainline_syntax_error']:
                                row['grammar'] = name
                                row['path'] = os.fsdecode(paths[row['index']])
                                failures.write(json.dumps(row) + '\n')
                                failures.flush()
                            if next_index % 1000 == 0:
                                state['counts'] = dict(counts)
                                save()
                                print(name, next_index, dict(counts), flush=True)
                        code = process.wait()
                    if not ready:
                        if state['status'] == 'running':
                            state['status'] = 'grammar_process_failed'
                            state['detail'] = str(code)
                        category = ('unsupported_at_runtime_file_grammar_pairs'
                                    if state['status'] == 'grammar_rejected'
                                    else 'grammar_failed_file_grammar_pairs')
                        totals[category] += len(paths)
                        break
                    if code and next_index < len(paths):
                        status = 'timeout' if code == -signal.SIGALRM else 'process_failed'
                        row = dict(index=next_index, grammar=name, status=status, exit_code=code,
                                   path=os.fsdecode(paths[next_index]))
                        failures.write(json.dumps(row) + '\n')
                        failures.flush()
                        log.write(json.dumps(row) + '\n')
                        counts[status] += 1
                        totals[status] += 1
                        next_index += 1
                    elif next_index < len(paths):
                        raise RuntimeError((name, next_index, len(paths), code))
                if next_index == len(paths):
                    state['status'] = 'complete'
            state['counts'] = dict(counts)
            save()
            print(name, state, flush=True)
    summary['complete'] = True
    summary['finished'] = time.time()
    save()
    print('complete', dict(totals), flush=True)


def retry(output, executable):
    if not json.loads((output / 'summary.json').read_text())['complete']:
        raise SystemExit('Wait for the comparison to finish before retrying.')
    inventory = json.loads((output / 'inventory.json').read_text())
    grammars = {grammar['name']: grammar for grammar in inventory['grammars']}
    failures = [json.loads(line) for line in (output / 'failures.jsonl').read_text().splitlines()]
    failed = [failure for failure in failures if failure['status'] in ['process_failed', 'timeout']]

    def limits():
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_AS, (4 * 1024**3, 4 * 1024**3))

    counts = collections.Counter()
    with (output / 'retries.jsonl').open('w') as results:
        for index, failure in enumerate(failed):
            grammar = grammars[failure['grammar']]
            print('retry', index + 1, 'of', len(failed), failure['path'], flush=True)
            manifest = output / 'retry.paths'
            manifest.write_bytes(os.fsencode(failure['path']) + b'\0')
            command = [str(executable), str(output / 'grammars' / (grammar['name'] + '.so')),
                       grammar['entry'], str(manifest), '0']
            try:
                process = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                         text=True, preexec_fn=limits, timeout=110,
                                         env=dict(os.environ, CORPUS_TIMEOUT_SECONDS='45'))
                rows = [json.loads(line) for line in process.stdout.splitlines()]
                row = next((row for row in rows if row['status'] != 'ready'), None)
                if row is None:
                    row = dict(status='timeout' if process.returncode == -14 else 'process_failed',
                               exit_code=process.returncode, stderr=process.stderr)
            except subprocess.TimeoutExpired:
                row = dict(status='timeout', detail='parent timeout')
            row.update(grammar=grammar['name'], path=failure['path'], index=failure['index'])
            results.write(json.dumps(row) + '\n')
            results.flush()
            counts[row['status']] += 1
            print(row['status'], dict(counts), flush=True)
    print('complete', dict(counts), flush=True)


def main():
    parser = argparse.ArgumentParser(description='Compare feller and mainline preorder slabs over code-corpora.')
    parser.add_argument('phase', choices=['inventory', 'compare', 'retry'])
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--corpus', type=Path, default=Path('../../code-corpora'))
    parser.add_argument('--executable', type=Path, default=Path('build/squat/feller-corpus'))
    arguments = parser.parse_args()
    output = arguments.output.resolve()
    if arguments.phase == 'inventory':
        inventory(output, arguments.corpus.resolve())
    elif arguments.phase == 'compare':
        compare(output, Path(__file__).resolve().parents[2], arguments.executable.resolve())
    else:
        retry(output, arguments.executable.resolve())


if __name__ == '__main__':
    main()
