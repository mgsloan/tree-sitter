#!/usr/bin/env python3
"""Rebuild the bugfix integration branch without rewriting source branches."""

import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys


def run(command, cwd, *, capture=False, check=True, env=None):
    result = subprocess.run(
        command, cwd=cwd, check=check, text=True,
        stdout=subprocess.PIPE if capture else None, env=env,
    )
    return result.stdout.strip() if capture else result.returncode


def git(repo, *args, **kwargs):
    return run(["git", *args], repo, **kwargs)


def resolve(repo, ref):
    return git(repo, "rev-parse", "--verify", f"{ref}^{{commit}}", capture=True)


def ancestor(repo, base, tip):
    return git(repo, "merge-base", "--is-ancestor", base, tip, check=False) == 0


def target_available(repo, target):
    if f"branch refs/heads/{target}" in git(repo, "worktree", "list", "--porcelain", capture=True).splitlines():
        raise ValueError(f"{target} is checked out in a worktree; switch that worktree to another branch first")


def save(path, state):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, indent=2) + "\n")
    temporary.replace(path)


def prepare(repo, manifest, args, state_dir):
    target = manifest["target"]
    git(repo, "check-ref-format", "--branch", target, capture=True)
    target_available(repo, target)
    if not args.no_fetch:
        for remote in manifest.get("fetch_remotes", []):
            git(repo, "fetch", remote)
    upstream = resolve(repo, manifest["upstream"])
    entries = []
    sources = [manifest["maintenance"]] if manifest.get("maintenance") else []
    sources += [entry for entry in manifest["fixes"] if entry.get("enabled", True)]
    for entry in sources:
        name = entry["id"]
        if not re.fullmatch(r"[a-z0-9-]+", name) or any(e["id"] == name for e in entries):
            raise ValueError(f"Invalid or duplicate fix id: {name}")
        source = resolve(repo, entry["ref"])
        base = resolve(repo, entry["base"])
        if not ancestor(repo, base, source):
            raise ValueError(f"{name}: base is not an ancestor of {entry['ref']}")
        if git(repo, "rev-list", "--min-parents=2", f"{base}..{source}", capture=True):
            raise ValueError(f"{name}: source range contains merges; extract the fix onto a clean branch")
        paths = git(repo, "diff", "--name-only", base, source, capture=True).splitlines()
        allowed = entry.get("allowed_paths", ["lib/", "crates/cli/src/tests/"])
        if any(not any(path.startswith(prefix) for prefix in allowed) for path in paths):
            raise ValueError(f"{name}: source range changes files outside {allowed}")
        entries.append(dict(id=name, source=source, base=base,
                            branch=f"bugfixes/rebased/{name}"))

    # Only reuse a clean worktree owned by this script.
    worktree = state_dir / "worktree"
    if worktree.exists():
        if git(worktree, "status", "--porcelain", capture=True):
            raise ValueError(f"Build worktree is dirty: {worktree}")
        git(worktree, "switch", "--detach", upstream)
    else:
        git(repo, "worktree", "add", "--detach", str(worktree), upstream)

    fixtures = Path(args.fixtures).resolve() if args.fixtures else repo / "test/fixtures/grammars"
    destination = worktree / "test/fixtures/grammars"
    destination.mkdir(parents=True, exist_ok=True)
    if fixtures.is_dir() and fixtures.resolve() != destination.resolve():
        for fixture in fixtures.iterdir():
            link = destination / fixture.name
            if fixture.is_dir() and not link.exists():
                link.symlink_to(fixture.resolve(), target_is_directory=True)

    old = git(repo, "rev-parse", "--verify", "--quiet", f"refs/heads/{target}", capture=True, check=False)
    state = dict(target=target, old=old, upstream=upstream, entries=entries,
                 worktree=str(worktree), phase="rebase", index=0, active=False,
                 checks=manifest["checks"], candidate="bugfixes/integration-candidate",
                 target_dir=str(Path(os.environ.get("CARGO_TARGET_DIR", repo / "target")).resolve()))
    save(state_dir / "state.json", state)
    return state


def rebuild(repo, state, state_path):
    worktree = Path(state["worktree"])
    environment = dict(os.environ, GIT_EDITOR="true", CARGO_TARGET_DIR=state["target_dir"])
    entries = state["entries"]
    while state["phase"] == "rebase" and state["index"] < len(entries):
        entry = entries[state["index"]]
        print(f"Rebasing {entry['id']} onto {state['upstream'][:12]}", flush=True)
        if not state["active"]:
            git(worktree, "switch", "-C", entry["branch"], entry["source"])
            state["active"] = True
            save(state_path, state)
            git(worktree, "rebase", "--onto", state["upstream"], entry["base"], env=environment)
        else:
            operation = Path(git(worktree, "rev-parse", "--git-path", "rebase-merge", capture=True))
            if not operation.is_absolute():
                operation = worktree / operation
            if operation.exists():
                git(worktree, "rebase", "--continue", env=environment)
        tip = resolve(repo, entry["branch"])
        if not ancestor(repo, state["upstream"], tip):
            raise ValueError("Rebase was aborted; preserve any resolutions, remove state.json, and start again")
        entry["rebased"] = tip
        state.update(index=state["index"] + 1, active=False)
        save(state_path, state)

    if state["phase"] == "rebase":
        git(worktree, "switch", "-C", state["candidate"], state["upstream"])
        state.update(phase="merge", index=0, active=False)
        save(state_path, state)

    while state["phase"] == "merge" and state["index"] < len(entries):
        entry = entries[state["index"]]
        print(f"Merging {entry['id']}", flush=True)
        if not state["active"]:
            state["active"] = True
            save(state_path, state)
            git(worktree, "merge", "--no-ff", "--no-edit", "-m",
                f"Merge bugfix: {entry['id']}", entry["rebased"], env=environment)
        elif git(worktree, "rev-parse", "--verify", "--quiet", "MERGE_HEAD", capture=True, check=False):
            git(worktree, "merge", "--continue", env=environment)
        if not ancestor(worktree, entry["rebased"], "HEAD"):
            raise ValueError("Merge was aborted; preserve any resolutions, remove state.json, and start again")
        state.update(index=state["index"] + 1, active=False)
        save(state_path, state)

    state["phase"] = "test"
    save(state_path, state)
    if git(worktree, "status", "--porcelain", capture=True):
        raise ValueError(f"Build worktree is dirty: {worktree}; commit conflict resolutions before continuing")
    for command in state["checks"]:
        print(f"Checking: {' '.join(command)}", flush=True)
        run(command, worktree, env=environment)
    tip = resolve(worktree, "HEAD")
    target_available(repo, state["target"])
    # Compare-and-swap prevents overwriting changes made during the build.
    zero = "0" * len(tip)
    if state["old"]:
        git(repo, "update-ref", "refs/bugfixes/previous", state["old"])
    git(repo, "update-ref", "-m", "Rebuild tested bugfix integration",
        f"refs/heads/{state['target']}", tip, state["old"] or zero)
    save(state_path.with_name("last-build.json"), dict(state, result=tip))
    state_path.unlink()
    print(f"Updated {state['target']} to {tip}\nBuild worktree: {worktree}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=Path(__file__).with_name("manifest.json"))
    parser.add_argument("--no-fetch", action="store_true", help="Use already-fetched refs")
    parser.add_argument("--continue", dest="resume", action="store_true", help="Resume a stopped build")
    parser.add_argument("--fixtures", help="Directory of prepared grammar fixtures to share with the build worktree")
    args = parser.parse_args()
    repo = Path(git(Path.cwd(), "rev-parse", "--show-toplevel", capture=True))
    common = Path(git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir", capture=True))
    state_dir = common / "bugfixes"
    state_dir.mkdir(exist_ok=True)
    state_path = state_dir / "state.json"
    with (state_dir / "lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError("Another bugfix build is running") from None
        if args.resume:
            state = json.loads(state_path.read_text())
        else:
            if state_path.exists():
                raise ValueError(f"A stopped build exists at {state_path}; use --continue")
            manifest = json.loads(args.manifest.read_text())
            if manifest.get("version") != 1 or not manifest.get("checks"):
                raise ValueError("Manifest needs version 1 and at least one validation command")
            state = prepare(repo, manifest, args, state_dir)
        rebuild(repo, state, state_path)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Build stopped: {error}", file=sys.stderr)
        print("The integration branch was not updated. Resolve and git add conflicts in the build\n"
              "worktree, then rerun with --continue. Original fix branches are unchanged.", file=sys.stderr)
        sys.exit(1)
