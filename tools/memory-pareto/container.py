#!/usr/bin/env python3
"""Build and run memory-pareto in the code-corpora grammars image, locally or over SSH."""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import hashlib
import subprocess
import sys
import tempfile
import tomllib

from run_corpus import sha256, write_json

ROOT = Path(__file__).resolve().parents[2]


def command(args, **kwargs):
    return subprocess.check_output(list(map(str, args)), text=True, **kwargs).strip()


def image_id(reference):
    if not re.fullmatch(r"(?:sha256:)?[0-9a-f]{64}|[^\s]+@sha256:[0-9a-f]{64}", reference):
        raise ValueError("use an immutable image digest or full local image ID")
    return command(["podman", "image", "inspect", "--format={{.Id}}", reference])


def classification(selected):
    """Use an explicit suffix map for the corpus languages and common support files."""
    path = Path(__file__).with_name("extensions.json")
    suffixes = json.loads(path.read_text())
    missing = set(suffixes.values()) - selected
    if missing:
        raise ValueError("extension map references unselected grammars: " + ", ".join(sorted(missing)))
    return dict(suffixes=suffixes, first_lines=[],
                inputs=[dict(path="tools/memory-pareto/extensions.json", sha256=sha256(path))])


def stage(corpus, target, repositories):
    """Snapshot selected source trees, including generated files, without Git metadata."""
    sha = command(["git", "-C", corpus, "rev-parse", "HEAD"])
    selected = tomllib.loads((corpus / "selected-grammars.toml").read_text())["repo"]
    rows = [r for r in tomllib.loads((corpus / "selected-repos.toml").read_text())["repo"]
            if not r.get("retired", False)]
    if repositories:
        unknown = set(repositories) - {r["name"] for r in rows}
        if unknown:
            raise ValueError("unknown selected repositories: " + ", ".join(sorted(unknown)))
        rows = [r for r in rows if r["name"] in repositories]
    tracked, missing, snapshots = [], [], []
    for split in ("training", "test"):
        (target / split).mkdir()
    for row in rows:
        relative = Path(row["split"]) / row["name"]
        source = corpus / relative
        if not (source / ".git").exists():
            missing.append(str(relative))
            continue
        actual = command(["git", "-C", source, "rev-parse", "HEAD"])
        if actual != row["sha"]:
            raise ValueError(f"corpus checkout differs from selected pin: {relative}")
        tracked.extend(str(relative / name) for name in
                       command(["git", "-C", source, "ls-files", "-z"]).split("\0") if name)
        snapshots.append(dict(path=str(relative), sha=actual,
            dirty=bool(command(["git", "-C", source, "status", "--porcelain", "--untracked-files=no"]))))
        # Preserve symlinks as symlinks: inventory records but never follows them.
        shutil.copytree(source, target / relative, symlinks=True, ignore=shutil.ignore_patterns(".git"))
    if not snapshots:
        raise ValueError("no selected corpus checkouts found")
    metadata = dict(code_corpora_sha=sha,
        code_corpora_dirty=bool(command(["git", "-C", corpus, "status", "--porcelain", "--untracked-files=no"])),
        definition_sha256={name: sha256(corpus / name) for name in
                           ("selected-repos.toml", "selected-grammars.toml", "containers/images.lock.toml")},
        selected_grammars=selected, repositories=snapshots, missing_repositories=missing,
        repository_filter=repositories, tracked_files=tracked,
        classification=classification({r["name"] for r in selected}))
    if sha != command(["git", "-C", corpus, "rev-parse", "HEAD"]):
        raise ValueError("code-corpora HEAD changed while staging")
    return metadata


def build_context(target):
    # The runtime crates inherit workspace metadata. Copy tracked source only,
    # plus the current tool files (including edits not yet committed).
    paths = command(["git", "-C", ROOT, "ls-files", "-z", "Cargo.toml", "lib", "crates/language"]).split("\0")
    for name in filter(None, paths):
        source = ROOT / name
        if source.is_file():
            destination = target / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
    tool = target / "tools/memory-pareto"
    shutil.copytree(Path(__file__).parent, tool,
                    ignore=shutil.ignore_patterns("target", "__pycache__", "results"))


def context_digest(context):
    digest = hashlib.sha256()
    for path in sorted(context.rglob("*")):
        if path.is_file():
            digest.update(str(path.relative_to(context)).encode() + b"\0")
            digest.update(bytes.fromhex(sha256(path)))
    return digest.hexdigest()


def remote_command(arguments):
    if not arguments.remote_checkout:
        raise ValueError("--host requires --remote-checkout (an existing checkout on that host)")
    args = ["python3", str(Path(arguments.remote_checkout) / "tools/memory-pareto/container.py"),
            "--code-corpora", str(arguments.code_corpora), "--directory", str(arguments.directory),
            "--jobs", str(arguments.jobs), "--max-file-bytes", str(arguments.max_file_bytes),
            "--skip-tsx-tail", str(arguments.skip_tsx_tail)]
    for name in ("image", "build_image", "grammars_image"):
        if getattr(arguments, name):
            args += ["--" + name.replace("_", "-"), getattr(arguments, name)]
    for repository in arguments.repo:
        args += ["--repo", repository]
    return ["ssh", arguments.host, shlex.join(args)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--code-corpora", type=Path, required=True,
                        help="code-corpora checkout on the execution host")
    parser.add_argument("--directory", type=Path, required=True, help="fresh result directory on the execution host")
    parser.add_argument("--host", help="SSH destination; both checkouts and images must exist remotely")
    parser.add_argument("--remote-checkout", help="path to this tree-sitter checkout on the SSH host")
    parser.add_argument("--image", help="reuse a previously built memory-pareto image by immutable ID")
    parser.add_argument("--build-image", help="immutable build image; defaults to code-corpora image lock")
    parser.add_argument("--grammars-image", help="immutable grammars image; defaults to code-corpora image lock")
    parser.add_argument("--repo", action="append", default=[], help="limit to a selected repository; repeatable")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--max-file-bytes", type=int, default=4*1024*1024)
    parser.add_argument("--skip-tsx-tail", type=int, default=3000)
    arguments = parser.parse_args()
    if arguments.jobs < 1 or arguments.max_file_bytes < 0 or arguments.skip_tsx_tail < 0:
        parser.error("jobs must be positive; cutoffs must be nonnegative")
    if arguments.host:
        return subprocess.call(remote_command(arguments))
    corpus = arguments.code_corpora.expanduser().resolve(strict=True)
    directory = arguments.directory.expanduser().resolve()
    directory.mkdir(parents=True, exist_ok=True)
    if any(directory.iterdir()):
        parser.error("choose a fresh, empty result directory")
    lock = tomllib.loads((corpus / "containers/images.lock.toml").read_text())
    # Both images must already be pulled/loaded on the execution host.
    grammar_image = image_id(arguments.grammars_image or lock["grammars"]["local_image_id"])
    with tempfile.TemporaryDirectory(prefix="memory-pareto-") as temporary:
        temporary = Path(temporary)
        if arguments.image:
            runtime_image = image_id(arguments.image)
            labels = json.loads(command(["podman", "image", "inspect", "--format={{json .Config.Labels}}", runtime_image]))
            if labels.get("memory-pareto.grammars-image") != grammar_image:
                raise ValueError("reused image has a different grammars base")
            build_image = labels["memory-pareto.build-image"]
        else:
            build_image = image_id(arguments.build_image or lock["build"]["local_image_id"])
            context = temporary / "build"
            context.mkdir()
            build_context(context)
            subprocess.run(["podman", "build", "--pull=never", "--iidfile", str(temporary / "image-id"),
                "--build-arg", "BUILD_IMAGE=" + build_image, "--build-arg", "GRAMMARS_IMAGE=" + grammar_image,
                "--build-arg", "TOOL_REVISION=" + command(["git", "-C", ROOT, "rev-parse", "HEAD"]),
                "--build-arg", "TOOL_SOURCE_SHA256=" + context_digest(context),
                "-f", str(context / "tools/memory-pareto/Containerfile"), str(context)], check=True)
            runtime_image = (temporary / "image-id").read_text().strip()
        labels = json.loads(command(["podman", "image", "inspect", "--format={{json .Config.Labels}}", runtime_image]))
        source = temporary / "input"
        source.mkdir()
        metadata = stage(corpus, source, arguments.repo)
        metadata.update(grammars_image=grammar_image, build_image=build_image, runtime_image=runtime_image,
            tool_revision=labels["memory-pareto.tool-revision"],
            tool_source_sha256=labels["memory-pareto.tool-source-sha256"])
        write_json(source / "metadata.json", metadata)
        # Only staged corpus inputs and the dedicated output directory are mounted.
        mounts = [str(source), str(directory)]
        if any(":" in p or "," in p for p in mounts):
            raise ValueError("mount paths must not contain ':' or ','")
        args = ["podman", "run", "--rm", "--init", "--pull=never", "--network=none",
            "--http-proxy=false", "--userns=keep-id", f"--user={os.getuid()}:{os.getgid()}",
            "--cap-drop=ALL", "--security-opt=no-new-privileges", "--read-only",
            "--pids-limit=512", "--memory=8g", "--memory-swap=8g", "--cpus=" + str(arguments.jobs),
            "--tmpfs=/tmp:rw,nosuid,nodev,noexec,size=256m,mode=1777",
            "-v", f"{source}:/input:ro,Z", "-v", f"{directory}:/out:rw,Z",
            runtime_image, "all", "--jobs", str(arguments.jobs),
            "--max-file-bytes", str(arguments.max_file_bytes), "--skip-tsx-tail", str(arguments.skip_tsx_tail)]
        print("Runtime image:", runtime_image, "\nResults:", directory, flush=True)
        # Forward signals and keep the staged input alive until reports flush.
        process = subprocess.Popen(args, start_new_session=True)
        def forward(signum, _frame):
            if process.poll() is None:
                process.send_signal(signum)
        previous = {number: signal.signal(number, forward) for number in (signal.SIGINT, signal.SIGTERM)}
        try:
            return process.wait()
        finally:
            for number, handler in previous.items():
                signal.signal(number, handler)


if __name__ == "__main__":
    sys.exit(main())
