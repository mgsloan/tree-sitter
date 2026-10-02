#!/usr/bin/env python3
"""Exercise the updater in disposable repositories, including stopped builds."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


UPDATER = Path(__file__).with_name("update.py").resolve()


class UpdateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.git("init", "-b", "upstream")
        self.git("config", "user.name", "Updater Test")
        self.git("config", "user.email", "updater@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "rerere.enabled", "true")
        self.commit("lib/shared", "base\n")
        self.base = self.git("rev-parse", "HEAD")
        self.manifest = self.repo / "manifest.json"

    def git(self, *args, cwd=None):
        return subprocess.check_output(
            ["git", *args], cwd=cwd or self.repo, env=self.env,
            text=True, stderr=subprocess.STDOUT,
        ).strip()

    def commit(self, path, content):
        file = self.repo / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(content)
        self.git("add", path)
        self.git("commit", "-m", f"Update {path}")
        return self.git("rev-parse", "HEAD")

    def fix(self, name="fix", path="lib/fix", content="fix\n", base=None):
        base = base or self.base
        self.git("switch", "-c", name, base)
        tip = self.commit(path, content)
        self.git("switch", "upstream")
        return dict(id=name, ref=name, base=base), tip

    def configure(self, fixes, checks=None):
        self.manifest.write_text(json.dumps(dict(
            version=1, upstream="upstream", target="aggregate", fixes=fixes,
            checks=checks or [[sys.executable, "-c", "pass"]],
        )))

    def update(self, *args, succeeds=True):
        result = subprocess.run(
            [sys.executable, str(UPDATER), "--manifest", str(self.manifest), "--no-fetch", *args],
            cwd=self.repo, env=self.env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        self.assertEqual(result.returncode == 0, succeeds, result.stdout)
        return result.stdout

    def test_build_preserves_sources_and_excludes_history_before_explicit_base(self):
        self.git("switch", "-c", "tree-squatter")
        patch_base = self.commit("squatter", "unrelated\n")
        entry, source = self.fix(base=patch_base)
        other, other_source = self.fix(name="other", path="lib/other")
        self.configure([entry, other])
        self.update()
        self.assertEqual(self.git("rev-parse", "fix"), source)
        self.assertEqual(self.git("rev-parse", "other"), other_source)
        self.assertEqual(self.git("show", "aggregate:lib/fix"), "fix")
        self.assertNotIn("squatter", self.git("ls-tree", "--name-only", "aggregate").splitlines())
        self.assertEqual(len(self.git("rev-list", "--merges", "aggregate").splitlines()), 2)

        self.commit("lib/new-upstream", "new\n")
        previous = self.git("rev-parse", "aggregate")
        self.update()
        self.assertEqual(self.git("show", "aggregate:lib/new-upstream"), "new")
        self.assertEqual(self.git("rev-parse", "refs/bugfixes/previous"), previous)
        self.assertEqual(self.git("rev-parse", "fix"), source)

    def test_upstream_equivalent_patch_is_dropped(self):
        entry, source = self.fix()
        self.commit("lib/fix", "fix\n")
        self.configure([entry])
        self.update()
        self.assertEqual(self.git("rev-parse", "aggregate"), self.git("rev-parse", "upstream"))
        self.assertEqual(self.git("rev-parse", "fix"), source)

    def test_failed_check_keeps_old_branch_and_can_resume(self):
        entry, _ = self.fix()
        self.git("branch", "aggregate", self.base)
        gate = self.repo / ".git" / "check-passes"
        self.configure([entry], [[sys.executable, "-c",
                                 f"from pathlib import Path; raise SystemExit(not Path({str(gate)!r}).exists())"]])
        self.update(succeeds=False)
        self.assertEqual(self.git("rev-parse", "aggregate"), self.base)
        self.assertTrue((self.repo / ".git/bugfixes/state.json").exists())
        gate.touch()
        self.update("--continue")
        self.assertEqual(self.git("show", "aggregate:lib/fix"), "fix")

    def test_rebase_conflict_can_resume_with_staged_resolution(self):
        entry, source = self.fix(path="lib/shared", content="fix\n")
        self.commit("lib/shared", "upstream\n")
        self.git("branch", "aggregate", "upstream")
        previous = self.git("rev-parse", "aggregate")
        self.configure([entry])
        self.update(succeeds=False)
        self.assertEqual(self.git("rev-parse", "aggregate"), previous)
        worktree = self.repo / ".git/bugfixes/worktree"
        (worktree / "lib/shared").write_text("upstream and fix\n")
        self.git("add", "lib/shared", cwd=worktree)
        self.update("--continue")
        self.assertEqual(self.git("show", "aggregate:lib/shared"), "upstream and fix")
        self.assertEqual(self.git("rev-parse", "fix"), source)

    def test_merge_conflict_can_resume_with_staged_resolution(self):
        first, _ = self.fix(name="first", path="lib/shared", content="first\n")
        second, _ = self.fix(name="second", path="lib/shared", content="second\n")
        self.configure([first, second])
        self.update(succeeds=False)
        state = json.loads((self.repo / ".git/bugfixes/state.json").read_text())
        self.assertEqual(state["phase"], "merge")
        worktree = self.repo / ".git/bugfixes/worktree"
        (worktree / "lib/shared").write_text("first and second\n")
        self.git("add", "lib/shared", cwd=worktree)
        self.update("--continue")
        self.assertEqual(self.git("show", "aggregate:lib/shared"), "first and second")

    def test_destination_checked_out_is_rejected(self):
        entry, _ = self.fix()
        self.configure([entry])
        self.git("switch", "-c", "aggregate", self.base)
        self.assertIn("checked out", self.update(succeeds=False))
        self.assertEqual(self.git("rev-parse", "aggregate"), self.base)

    def test_destination_changed_during_checks_is_preserved(self):
        entry, _ = self.fix()
        self.git("branch", "aggregate", self.base)
        replacement = self.commit("lib/new-upstream", "new\n")
        self.configure([entry], [["git", "update-ref", "refs/heads/aggregate", replacement]])
        self.update(succeeds=False)
        self.assertEqual(self.git("rev-parse", "aggregate"), replacement)


if __name__ == "__main__":
    unittest.main()
