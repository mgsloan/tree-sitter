#!/usr/bin/env python3
"""Publish the selected contents of dev as merge commits on organized."""

import argparse
import os
from pathlib import Path, PurePosixPath
import subprocess
import tempfile
import tomllib


class PublicationError(Exception):
    pass


def git(*arguments, input=None, environment=None, directory=None, allowed=(0,)):
    result = subprocess.run(
        ['git', *arguments], input=input, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, env=environment, cwd=directory,
    )
    if result.returncode not in allowed:
        raise PublicationError(result.stderr.decode().strip() or f'git {arguments[0]} failed')
    return result.stdout


def revision(reference):
    return git('rev-parse', '--verify', f'{reference}^{{commit}}').decode().strip()


def branch_tip(branch):
    return git('rev-parse', '--verify', '--quiet', f'refs/heads/{branch}',
               allowed=(0, 1)).decode().strip()


def is_ancestor(ancestor, descendant):
    result = subprocess.run(
        ['git', 'merge-base', '--is-ancestor', ancestor, descendant],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    if result.returncode not in (0, 1):
        raise PublicationError(result.stderr.decode().strip())
    return result.returncode == 0


def checked_out(branch):
    for record in git('worktree', 'list', '--porcelain', '-z').split(b'\0\0'):
        fields = dict(field.split(b' ', 1) for field in record.split(b'\0') if b' ' in field)
        if fields.get(b'branch') == os.fsencode(f'refs/heads/{branch}'):
            return os.fsdecode(fields[b'worktree'])
    return None


def advance_branch(branch, commit, previous):
    directory = checked_out(branch)
    if directory:
        if git('status', '--porcelain', directory=directory):
            raise PublicationError(f'{branch} has uncommitted files in {directory}')
        if branch_tip(branch) != previous:
            raise PublicationError(f'{branch} moved; prepare again')
        git('merge', '--ff-only', commit, directory=directory)
    else:
        git('update-ref', f'refs/heads/{branch}', commit, previous or '0' * len(commit))


def validate_path(path, root=False):
    if root and path == '.':
        return
    if (not isinstance(path, str) or not path or PurePosixPath(path).is_absolute()
            or any(part in ('', '.', '..', '.git') for part in path.split('/'))):
        raise PublicationError(f'invalid publication path: {path!r}')


def exported_tree(source):
    # The exporter and its inputs must come from the same source revision.
    if git('show', f'{source}:tools/publish.py') != Path(__file__).read_bytes():
        raise PublicationError('run the tools/publish.py committed at the source revision')
    configuration = tomllib.loads(git('show', f'{source}:tools/publish.toml').decode())
    mappings = configuration['paths']
    if not isinstance(mappings, dict) or not mappings:
        raise PublicationError('the publication path mapping is empty')
    for original, destination in mappings.items():
        validate_path(original)
        validate_path(destination, root=True)

    entries = {}
    matched = set()
    for record in git('ls-tree', '-rz', '--full-tree', source).split(b'\0'):
        if not record:
            continue
        metadata, path = record.split(b'\t', 1)
        mode, kind, object_id = metadata.split()
        path = os.fsdecode(path)
        for original, destination in mappings.items():
            if path != original and not path.startswith(original + '/'):
                continue
            matched.add(original)
            suffix = path[len(original):].lstrip('/')
            published = '/'.join(part for part in (destination, suffix) if part and part != '.')
            validate_path(published)
            if kind != b'blob':
                raise PublicationError(f'unsupported Git entry: {path}')
            if published in entries:
                raise PublicationError(f'duplicate publication destination: {published}')
            entries[published] = mode + b' ' + object_id + b'\t' + os.fsencode(published) + b'\0'

    missing = mappings.keys() - matched
    if missing:
        raise PublicationError(f'missing publication sources: {", ".join(sorted(missing))}')
    for path in entries:
        if any(str(parent) in entries for parent in PurePosixPath(path).parents):
            raise PublicationError(f'publication file/directory collision: {path}')

    with tempfile.TemporaryDirectory(prefix='squatter-publish-') as directory:
        environment = {**os.environ, 'GIT_INDEX_FILE': str(Path(directory) / 'index')}
        git('read-tree', '--empty', environment=environment)
        git('update-index', '-z', '--index-info', input=b''.join(entries.values()),
            environment=environment)
        return git('write-tree', environment=environment).decode().strip()


def publication(commit):
    tree, *parents = git('show', '-s', '--format=%T %P', commit).decode().split()
    trailers = git('show', '-s', '--format=%B', commit)
    trailers = git('interpret-trailers', '--parse', input=trailers).decode().splitlines()
    trailers = dict(line.split(': ', 1) for line in trailers if ': ' in line)
    if (len(parents) != 2 or trailers.get('Published-Source') != parents[1]
            or trailers.get('Published-Tree') != tree):
        return None
    return tree, parents[0], parents[1]


def validate_target(target, source):
    previous = publication(target)
    if previous:
        if not is_ancestor(previous[2], source):
            raise PublicationError('source does not descend from the previous publication source')
    elif not is_ancestor(target, source):
        raise PublicationError('target has unpublished changes; incorporate its history into dev first')


def prepare(arguments):
    source = revision(arguments.source)
    target = branch_tip(arguments.target)
    if not target:
        raise PublicationError(f'create the initial branch with: git branch {arguments.target} <base>')
    validate_target(target, source)
    tree = exported_tree(source)
    print(f'Source: {source}\nTarget: {target}\nTree:   {tree}', flush=True)
    if tree == git('rev-parse', f'{target}^{{tree}}').decode().strip():
        print('Published contents are unchanged.')
        return
    statistics = git('diff', '--stat=80,40,20', '--find-renames', target, tree).decode()
    print(statistics, end='', flush=True)
    if arguments.action == 'check':
        return
    if source == target:
        raise PublicationError('commit development work before preparing the first merge')
    candidate = branch_tip(arguments.candidate)
    if candidate and publication(candidate) == (tree, target, source):
        print(f'Already prepared {arguments.candidate}: {candidate}')
        return
    if candidate and not is_ancestor(candidate, target):
        raise PublicationError(f'{arguments.candidate} has an unpublished candidate; use another --candidate')
    message = (
        f'Publish {arguments.source} to {arguments.target}\n\n'
        f'Published-Source: {source}\nPublished-Tree: {tree}\n'
    )
    commit = git('commit-tree', tree, '-p', target, '-p', source,
                 input=message.encode()).decode().strip()
    advance_branch(arguments.candidate, commit, candidate)
    print(f'Prepared {arguments.candidate}: {commit}')


def publish(arguments):
    candidate = branch_tip(arguments.candidate)
    if not candidate:
        raise PublicationError(f'no candidate on {arguments.candidate}; run prepare first')
    target = branch_tip(arguments.target)
    if candidate == target:
        print('Candidate is already published.')
        return
    prepared = publication(candidate)
    if not prepared:
        raise PublicationError('candidate is not a publication merge')
    tree, parent, source = prepared
    if parent != target:
        raise PublicationError('target moved since preparation; prepare a new candidate')
    validate_target(target, source)
    if tree != exported_tree(source):
        raise PublicationError('candidate differs from the exported source tree')
    advance_branch(arguments.target, candidate, target)
    print(f'Published {arguments.target}: {candidate}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['check', 'prepare', 'publish'])
    parser.add_argument('--source', default='dev', help='committed source revision (default: dev)')
    parser.add_argument('--target', default='organized', help='output branch (default: organized)')
    parser.add_argument('--candidate', help='review branch (default: publish/<target>)')
    arguments = parser.parse_args()
    arguments.candidate = arguments.candidate or f'publish/{arguments.target}'
    try:
        os.chdir(os.fsdecode(git('rev-parse', '--show-toplevel').rstrip(b'\n')))
        for branch in (arguments.target, arguments.candidate):
            git('check-ref-format', f'refs/heads/{branch}')
        if arguments.target == arguments.candidate:
            raise PublicationError('target and candidate branches must differ')
        if arguments.action == 'publish':
            publish(arguments)
        else:
            prepare(arguments)
    except (PublicationError, OSError, ValueError, KeyError) as error:
        parser.exit(1, f'error: {error}\n')


if __name__ == '__main__':
    main()
