#!/usr/bin/env python3
"""Exercise publication history and file selection in disposable repositories."""

from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name('publish.py')


class PublicationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='squatter-publish-test-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / 'repository'
        self.root.mkdir()
        self.git('init', '-b', 'main')
        self.git('config', 'user.name', 'Publication test')
        self.git('config', 'user.email', 'publication@example.invalid')
        self.git('config', 'commit.gpgsign', 'false')
        self.write('core/keep', 'original\n')
        self.write('core/remove', 'remove me\n')
        self.write('core/old name', 'rename me\n')
        self.write('core/executable', '#!/bin/sh\n')
        (self.root / 'core/executable').chmod(0o755)
        (self.root / 'core/link').symlink_to('keep')
        self.write('experimental/private', 'not published\n')
        self.base = self.commit('Initial development')
        self.git('branch', 'pub')
        self.write('tools/publish.py', SCRIPT.read_text())
        self.write('tools/publish.toml', '[paths]\n"core" = "squatter"\n')
        self.source = self.commit('Add publisher')

    def git(self, *arguments):
        return subprocess.check_output(['git', *arguments], cwd=self.root,
                                       stderr=subprocess.PIPE).decode().strip()

    def write(self, relative, contents):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)

    def commit(self, message):
        self.git('add', '-A')
        self.git('commit', '-m', message)
        return self.git('rev-parse', 'HEAD')

    def run_publisher(self, *arguments, success=True):
        result = subprocess.run(
            [sys.executable, str(self.root / 'tools/publish.py'), *arguments],
            cwd=self.root, capture_output=True, text=True,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result.stdout + result.stderr

    def publish(self):
        self.run_publisher('publish')
        return self.git('rev-parse', 'pub')

    def test_history_deletions_moves_modes_and_repeated_publication(self):
        initial = self.publish()
        self.assertEqual(self.git('show', '-s', '--format=%P', initial),
                         f'{self.base} {self.source}')
        self.assertEqual(self.git('ls-tree', '--name-only', initial), 'squatter')
        self.assertEqual(self.git('branch', '--format=%(refname:short)'), 'main\npub')
        self.assertTrue(self.git('ls-tree', initial, 'squatter/executable').startswith('100755'))
        self.assertTrue(self.git('ls-tree', initial, 'squatter/link').startswith('120000'))

        self.git('rm', 'core/remove')
        self.git('mv', 'core/old name', 'core/new\tname')
        self.write('core/added', 'new\n')
        self.write('experimental/another', 'still excluded\n')
        self.write('tools/publish.toml', '[paths]\n"core" = "tree-squatter"\n')
        source = self.commit('Move published files')
        self.write('core/keep', 'uncommitted edit\n')
        second = self.publish()
        self.assertEqual(self.git('show', '-s', '--format=%P', second), f'{initial} {source}')
        self.assertEqual(self.git('ls-tree', '--name-only', second), 'tree-squatter')
        self.assertEqual(self.git('show', f'{second}:tree-squatter/keep'), 'original')
        files = self.git('ls-tree', '-r', '--name-only', second)
        self.assertNotIn('remove', files)
        self.assertNotIn('old name', files)
        self.assertIn('added', files)
        self.assertEqual(self.git('show', f'{second}:tree-squatter/new\tname'), 'rename me')
        self.assertEqual(self.git('rev-parse', 'main'), source)
        self.assertIn('unchanged', self.run_publisher('publish'))
        self.assertEqual(self.git('rev-parse', 'pub'), second)

    def test_check_leaves_refs_and_index_unchanged(self):
        self.write('core/staged', 'staged\n')
        self.git('add', 'core/staged')
        index = self.git('write-tree')
        references = self.git('show-ref')
        first = self.run_publisher('check')
        self.assertEqual(first, self.run_publisher('check'))
        self.assertEqual(self.git('show-ref'), references)
        self.assertEqual(self.git('write-tree'), index)

    def test_missing_sources_collisions_and_invalid_destinations(self):
        configurations = [
            ('[paths]\n"missing" = "squatter"\n', 'missing publication sources'),
            ('[paths]\n"core" = "squatter"\n"core/keep" = "squatter/keep"\n',
             'duplicate publication destination'),
            ('[paths]\n"core" = "squatter"\n"experimental/private" = "squatter"\n',
             'file/directory collision'),
            ('[paths]\n"core" = "../escape"\n', 'invalid publication path'),
        ]
        for configuration, error in configurations:
            with self.subTest(error=error):
                self.write('tools/publish.toml', configuration)
                self.commit('Change mapping')
                self.assertIn(error, self.run_publisher('check', success=False))

    def test_independent_target_changes_are_rejected(self):
        self.publish()
        self.write('core/keep', 'newer source\n')
        self.commit('Advance development')
        target = self.git('rev-parse', 'pub')
        tree = self.git('rev-parse', 'pub^{tree}')
        changed = self.git('commit-tree', tree, '-p', target, '-m', 'Independent target commit')
        self.git('update-ref', 'refs/heads/pub', changed, target)
        self.assertIn('unpublished changes', self.run_publisher('check', success=False))
        self.assertIn('unpublished changes', self.run_publisher('publish', success=False))
        self.assertEqual(self.git('rev-parse', 'pub'), changed)

    def test_root_templates_and_removing_a_mapping(self):
        self.write('public/README.md', 'public documentation\n')
        self.write('tools/publish.toml', '[paths]\n"core" = "squatter"\n"public" = "."\n')
        self.commit('Add root templates')
        self.publish()
        self.assertEqual(self.git('show', 'pub:README.md'), 'public documentation')
        self.write('tools/publish.toml', '[paths]\n"public" = "."\n')
        self.commit('Remove published component')
        self.publish()
        self.assertEqual(self.git('ls-tree', '--name-only', 'pub'), 'README.md')

    def test_source_branch_is_not_replaced(self):
        self.assertIn('source and target commits must differ',
                      self.run_publisher('publish', '--target', 'main', success=False))
        self.assertEqual(self.git('rev-parse', 'main'), self.source)
        self.assertEqual(self.git('rev-parse', 'pub'), self.base)

    def test_publication_updates_clean_worktree_and_refuses_dirty_one(self):
        self.publish()
        checkout = self.root.parent / 'pub'
        self.git('worktree', 'add', str(checkout), 'pub')
        self.write('core/keep', 'updated\n')
        self.commit('Change core')
        (checkout / 'squatter/keep').write_text('local edit\n')
        previous = self.git('rev-parse', 'pub')
        self.assertIn('uncommitted files', self.run_publisher('publish', success=False))
        self.assertEqual(self.git('rev-parse', 'pub'), previous)
        (checkout / 'squatter/keep').write_text('original\n')
        self.run_publisher('publish')
        self.assertEqual((checkout / 'squatter/keep').read_text(), 'updated\n')

    def test_exporter_version_and_source_ancestry_are_checked(self):
        self.publish()
        self.write('tools/publish.py', SCRIPT.read_text() + '\n# uncommitted change\n')
        self.assertIn('source revision', self.run_publisher('publish', success=False))
        shutil.copyfile(SCRIPT, self.root / 'tools/publish.py')
        tree = self.git('rev-parse', 'main^{tree}')
        unrelated = self.git('commit-tree', tree, '-m', 'Unrelated source')
        self.assertIn('does not descend',
                      self.run_publisher('publish', '--source', unrelated, success=False))


if __name__ == '__main__':
    unittest.main()
