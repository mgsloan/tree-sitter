#!/usr/bin/env python3
"""Stage reproducible corpus inputs, then execute squat tools in offline Podman."""
import argparse
import collections
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import tomllib

ROOT = Path(__file__).resolve().parents[2]
def run(command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


def sha256(path):
    with open(path, "rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def revision(path):
    result = run(["git", "-C", str(path), "rev-parse", "HEAD"], capture_output=True, text=True)
    return result.stdout.strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--code-corpora", type=Path, default=ROOT / "../../code-corpora")
    parser.add_argument("--matrix", type=Path, default=ROOT / "tools/squatter/matrix.toml")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repo", action="append")
    parser.add_argument("--grammar", action="append")
    parser.add_argument("--image")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--per-bucket", type=int, default=4)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--count", type=int)
    parser.add_argument("--max-file-bytes", type=int, default=4 * 1048576)
    parser.add_argument("--timeout", type=int, default=1200)
    parser.add_argument("--skip-layouts", action="store_true")
    parser.add_argument("--skip-mutated", action="store_true")
    parser.add_argument("--skip-sampling", action="store_true")
    parser.add_argument("--skip-benchmarks", action="store_true")
    parser.add_argument("--checks-only", action="store_true",
                        help="run the shared correctness oracle once instead of benchmarking")
    parser.add_argument("--unoptimized-query", action="store_true")
    parser.add_argument("--benchmark", action="append", help="benchmark selector; repeatable")
    parser.add_argument("--pressure-profile", action="append",
                        help="matrix pressure profile; repeatable (default: isolated)")
    parser.add_argument("--pressure-bytes", type=int)
    parser.add_argument("--benchmark-cpu", type=int)
    parser.add_argument("--pressure-cpu", type=int)
    args = parser.parse_args()
    matrix_path = args.matrix.resolve()
    matrix_bytes = matrix_path.read_bytes()
    matrix = tomllib.loads(matrix_bytes.decode())
    corpus = args.code_corpora.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    (output / "matrix.toml").write_bytes(matrix_bytes)
    snapshot = output / "source"
    snapshot.mkdir()
    source_hash = hashlib.sha256()
    paths = run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z",
                 "Cargo.toml", "Cargo.lock", "LICENSE", ".cargo", "lib", "crates",
                 "tools/memory-pareto", "tools/squatter"], cwd=ROOT, capture_output=True).stdout.split(b"\0")
    for raw_path in sorted(set(paths)):
        if not raw_path:
            continue
        relative = os.fsdecode(raw_path)
        path = ROOT / relative
        if not path.is_file() or path.is_symlink():
            continue
        destination = snapshot / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
        source_hash.update(len(raw_path).to_bytes(8, "little"))
        source_hash.update(raw_path)
        source_hash.update(bytes.fromhex(sha256(destination)))
    inputs = output / "corpus"
    inputs.mkdir()
    grammar_outputs = output / "grammars"
    grammar_outputs.mkdir()
    lock = tomllib.loads((corpus / "containers/images.lock.toml").read_text())
    image = args.image or lock["build"]["local_image_id"]
    if subprocess.run(["podman", "image", "exists", image]).returncode:
        raise SystemExit(f"build image is not cached: {image}; select an installed image with --image")
    selected = {entry["name"]: entry for entry in tomllib.loads((corpus / "selected-grammars.toml").read_text())["repo"]}
    grammar_names = args.grammar or matrix["selection"]["grammars"]
    repositories = args.repo or matrix["selection"]["repositories"]
    suffixes = json.loads((ROOT / "crates/corpus-analysis/extensions.json").read_text())
    buckets = collections.defaultdict(list)
    coverage = collections.Counter()
    for split in ["train", "training", "test"]:
        if (corpus / split).is_symlink():
            continue
        for repository in repositories:
            directory = corpus / split / repository
            if directory.is_symlink() or not directory.is_dir():
                continue
            for parent, directories, files in os.walk(directory, followlinks=False):
                directories[:] = sorted(name for name in directories if name != ".git" and not (Path(parent) / name).is_symlink())
                for name in sorted(files):
                    path = Path(parent) / name
                    if path.is_symlink():
                        coverage["symlinks"] += 1
                        continue
                    grammar = suffixes.get(name, suffixes.get(path.suffix.lstrip(".")))
                    if grammar not in grammar_names:
                        coverage["unclassified_or_unselected_grammar"] += 1
                        continue
                    size = path.stat().st_size
                    if size > args.max_file_bytes:
                        coverage["oversized"] += 1
                        continue
                    bucket = "small" if size < 4096 else "normal" if size <= 102400 else "large" if 1048576 < size else None
                    if bucket is None:
                        coverage["intentional_size_gap"] += 1
                        continue
                    relative = path.relative_to(corpus).as_posix()
                    if "\n" in relative or "\r" in relative:
                        coverage["newline_path"] += 1
                        continue
                    rank = hashlib.sha256(f"{args.seed}\0{relative}".encode()).hexdigest()
                    entries = buckets[(split, grammar, bucket)]
                    entries.append((rank, relative, size))
                    entries.sort()
                    del entries[args.per_bucket:]
                    coverage["candidates"] += 1
    staged = []
    for (split, grammar, bucket), entries in sorted(buckets.items()):
        for _, relative, size in entries:
            destination = inputs / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(corpus / relative, destination)
            staged.append(dict(path=relative, split=split, grammar=grammar, bucket=bucket, bytes=size, sha256=sha256(destination)))
    if not staged:
        raise SystemExit("no files staged; inspect repository selection")
    provenance = dict(code_corpora_sha=revision(corpus), tool_sha=revision(ROOT), source_sha256=source_hash.hexdigest(),
                      matrix_sha256=hashlib.sha256(matrix_bytes).hexdigest(), image=image,
                      seed=args.seed, repositories=repositories, coverage=dict(coverage), inputs=staged,
                      tool_dirty=bool(run(["git", "-C", str(ROOT), "status", "--porcelain"], capture_output=True, text=True).stdout),
                      missing_repositories=[name for name in repositories if not any((corpus / split / name).is_dir() for split in ["train", "training", "test"])],
                      operations=[])
    manifest_path = output / "container-run.json"
    manifest_path.write_text(json.dumps(provenance, indent=2) + "\n")
    common = ["podman", "run", "--rm", "--network=none", "--cap-drop=all", "--security-opt=no-new-privileges",
              "--memory=8g", "--cpus=4", "--pids-limit=256", "--entrypoint", "sh",
              "-v", f"{snapshot}:/work:ro", "-v", f"{output}:/out:rw", "-e", f"SQUAT_TOOL_SHA={provenance['tool_sha']}", "-e", f"SQUAT_SOURCE_SHA256={provenance['source_sha256']}"]
    registry = dict(code_corpora_sha=provenance["code_corpora_sha"], suffixes=suffixes, grammars={})
    query_sources = collections.defaultdict(set)
    for reference in [corpus / "zed", corpus / "zed-extensions"]:
        for parent, directories, files in os.walk(reference, followlinks=False):
            directories[:] = [name for name in directories if name not in [".git", "node_modules", "target"]
                              and not (Path(parent) / name).is_symlink()]
            if "config.toml" not in files:
                continue
            config = tomllib.loads((Path(parent) / "config.toml").read_text())
            grammar = config.get("grammar")
            if grammar in grammar_names:
                query_sources[grammar].update(Path(parent) / name for name in files if name.endswith(".scm"))
    query_outputs = output / "queries"
    query_outputs.mkdir()
    for name in sorted({entry["grammar"] for entry in staged}):
        entry = selected[name]
        checkout = corpus / "grammars" / name
        source = "/grammar/" + entry.get("directory", "").strip("/") + "/src"
        command = r'''
set -eu
source=$1
name=$2
set -- "$source/parser.c"
if test -f "$source/scanner.c"; then set -- "$@" "$source/scanner.c"; fi
cc -shared -fPIC -O2 -I"$source" "$@" -o "/out/grammars/$name.so"
'''
        run(common + ["-v", f"{checkout}:/grammar:ro", image, "-c", command, "sh", source, name], timeout=args.timeout)
        registry["grammars"][name] = dict(library=f"/out/grammars/{name}.so",
            symbol="tree_sitter_" + entry.get("grammar", name).replace("-", "_"),
            library_sha256=sha256(grammar_outputs / f"{name}.so"), sha=entry["sha"])
        for directory in {checkout / "queries", checkout / entry.get("directory", "") / "queries"}:
            if directory.is_dir():
                query_sources[name].update(directory.rglob("*.scm"))
        specs = []
        for index, path in enumerate(sorted(query_sources[name])):
            destination = query_outputs / f"{name}-{index}.scm"
            shutil.copyfile(path, destination)
            specs.append(dict(name=path.relative_to(corpus).as_posix(),
                              path=f"/out/queries/{destination.name}", sha256=sha256(destination)))
        registry["grammars"][name]["queries"] = specs
        provenance.setdefault("grammars", {})[name] = dict(
            pin=entry["sha"], checkout_sha=revision(checkout),
            parser_sha256=sha256(checkout / entry.get("directory", "") / "src/parser.c"),
            library_sha256=registry["grammars"][name]["library_sha256"])
    (output / "registry.json").write_text(json.dumps(registry, indent=2) + "\n")
    run(["cargo", "build", "--release", "--locked", "--target-dir", str(output / "target"), "-p", "corpus-analysis", "-p", "squatter-bench"], cwd=snapshot)

    def execute(name, shell, *arguments):
        started = time.monotonic()
        log = output / f"{name}.log"
        print(f"running {name}; log: {log}", flush=True)
        try:
            with log.open("w") as destination:
                run(common + [image, "-c", shell, "sh", *arguments], stdout=destination,
                    stderr=subprocess.STDOUT, timeout=args.timeout)
            status = "passed"
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            status = str(error)
        provenance["operations"].append(dict(name=name, status=status, seconds=time.monotonic() - started))
        manifest_path.write_text(json.dumps(provenance, indent=2) + "\n")
        print(f"{name}: {status}", flush=True)

    # Invoke the container's loader so host-built Rust binaries need no host
    # interpreter mount. The chosen Ubuntu build image supplies compatible libc.
    loader = "/lib64/ld-linux-x86-64.so.2"
    if not args.skip_sampling:
        execute("sampling", f'''{loader} /out/target/release/corpus-analysis sample --code-corpora /out/corpus --registry /out/registry.json --output /out/samplings --seed "$1" --per-bucket "$2"''', str(args.seed), str(args.per_bucket))
    # POSIX sh has no array slice: shift before forwarding remaining arguments.
    program = "squatter-check" if args.checks_only else "squatter-bench"
    command = f'''repeat=$1; seed=$2; name=$3; shift 3; {loader} /out/target/release/{program} --code-corpora /out/corpus --registry /out/registry.json --all --repeat "$repeat" --seed "$seed" --output-directory /out/bench-outputs --output "$name" "$@"'''
    extra = ["--count", str(args.count)] if args.count is not None else []
    extra += args.benchmark or []
    if args.unoptimized_query:
        extra += ["--unoptimized-query"]
    profiles = args.pressure_profile or ["isolated"]
    if args.checks_only and profiles != ["isolated"]:
        parser.error("--checks-only supports only the isolated pressure profile")
    unknown_profiles = set(profiles) - set(matrix["pressure"])
    if unknown_profiles:
        parser.error("unknown pressure profiles: " + ", ".join(sorted(unknown_profiles)))
    if not args.skip_benchmarks:
        for profile_name in profiles:
            profile = matrix["pressure"][profile_name]
            pressure = ["--pressure", profile["mode"], "--pressure-label", profile_name]
            if "duty_percent" in profile:
                pressure += ["--pressure-duty-percent", str(profile["duty_percent"])]
            if args.pressure_bytes is not None:
                pressure += ["--pressure-bytes", str(args.pressure_bytes)]
            if args.benchmark_cpu is not None:
                pressure += ["--benchmark-cpu", str(args.benchmark_cpu)]
            if args.pressure_cpu is not None:
                pressure += ["--pressure-cpu", str(args.pressure_cpu)]
            prefix = "check-" if args.checks_only else ""
            name = f"{prefix}original-{profile_name}"
            execute(name, command, str(args.repeat), str(args.seed), name, *pressure, *extra)
            if not args.skip_mutated:
                name = f"{prefix}mutated-{profile_name}"
                execute(name, command, str(args.repeat), str(args.seed), name,
                        "--mutate", *pressure, *extra)
    if not args.skip_layouts:
        for layout in matrix["layout"]:
            group = layout["group_size"]
            alignment = layout["alignment"]
            layout_name = layout["name"]
            directory = f"/out/layout-{layout_name}"
            execute(f"build-layout-{layout_name}",
                '''make -C /work/lib/squat -j4 BUILD="$1" CFLAGS="-O3 -g -DSQ_GROUP_SIZE=$2 -DSQ_COLUMN_ALIGNMENT=$3" "$1/layout-bench" "$1/compare" check''', directory, str(group), str(alignment))
            for grammar in registry["grammars"]:
                files = ["/out/corpus/" + entry["path"] for entry in staged if entry["grammar"] == grammar]
                symbol = registry["grammars"][grammar]["symbol"]
                execute(f"layout-{layout_name}-{grammar}",
                    '''program=$1; grammar=$2; symbol=$3; shift 3; "$program/layout-bench" "/out/grammars/$grammar.so" "$symbol" "$@"''', directory, grammar, symbol, *files)
                small_files = ["/out/corpus/" + entry["path"] for entry in staged
                               if entry["grammar"] == grammar and entry["bytes"] < 4096]
                execute(f"check-layout-{layout_name}-{grammar}",
                    '''program=$1; grammar=$2; symbol=$3; shift 3; "$program/compare" "/out/grammars/$grammar.so" "$symbol" "$@"''', directory, grammar, symbol, *small_files)
        execute("scan-kernels", '''make -C /work/lib/squat -j4 BUILD=/out/scan CFLAGS="-O3 -g" /out/scan/scan-bench && /out/scan/scan-bench''')
    failed = [entry for entry in provenance["operations"] if entry["status"] != "passed"]
    print(f"{len(staged)} staged files; {len(failed)} failed operations; {manifest_path}")
    raise SystemExit(bool(failed))


if __name__ == "__main__":
    main()
