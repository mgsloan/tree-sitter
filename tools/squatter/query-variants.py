#!/usr/bin/env python3
"""Compare query layouts and shortcuts using an existing immutable corpus snapshot."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--group-size", type=int, choices=[16, 32, 64], action="append")
    parser.add_argument("--timeout", type=int, default=1200)
    args = parser.parse_args()
    root = args.run.resolve()
    original = json.loads((root / "container-run.json").read_text())
    manifest_path = root / "query-variants.json"
    sizes = list(dict.fromkeys(args.group_size or [16, 32, 64]))
    manifest = dict(group_sizes=sizes, planned=sum(6 if size == 16 else 2 for size in sizes),
                    source_sha256=original["source_sha256"], image=original["image"],
                    repeat=args.repeat, operations=[])
    # Never overwrite previous measurements, even if an earlier run was partial.
    with manifest_path.open("x") as output:
        json.dump(manifest, output, indent=2)
    for size in sizes:
        target = root / ("target" if size == 16 else f"target-{size}")
        flags = f"-DSQ_GROUP_SIZE={size}"
        with (root / f"query-build-{size}.log").open("w") as log:
            subprocess.run(["cargo", "build", "--manifest-path", str(root / "source/Cargo.toml"),
                            "-p", "squatter-bench", "--release", "--locked", "--target-dir", str(target)],
                           env={**os.environ, "CFLAGS": flags}, stdout=log, stderr=subprocess.STDOUT,
                           check=True, timeout=args.timeout)
        binary = target / "release/squatter-bench"
        with binary.open("rb") as source:
            binary_hash = hashlib.file_digest(source, "sha256").hexdigest()
        variants = [("optimized", [])]
        if size == 16:
            variants += [("unoptimized", ["--unoptimized-query"]), ("repacked", ["--repack"])]
        for variant, switches in variants:
            for mutated in [False, True]:
                name = f"query-{size}-{variant}-{'mutated' if mutated else 'original'}"
                command = ["podman", "run", "--rm", "--network=none", "--cap-drop=all",
                           "--security-opt=no-new-privileges", "--memory=8g", "--cpus=4", "--pids-limit=256",
                           "-v", f"{root}:/out:rw", "-e", f"SQUAT_TOOL_SHA={original['tool_sha']}",
                           "-e", f"SQUAT_SOURCE_SHA256={original['source_sha256']}",
                           "--entrypoint", "/lib64/ld-linux-x86-64.so.2", original["image"],
                           f"/out/{target.name}/release/squatter-bench", "--code-corpora", "/out/corpus",
                           "--registry", "/out/registry.json", "--all", "--repeat", str(args.repeat),
                           "--seed", str(original["seed"]), "--output-directory", "/out/bench-outputs",
                           "--output", name, "query-matches", "query-captures", *switches]
                if mutated:
                    command.append("--mutate")
                record = dict(name=name, group_size=size, cflags=flags, binary_sha256=binary_hash,
                              variant=variant, mutated=mutated, command=command)
                started = time.monotonic()
                try:
                    with (root / f"{name}.log").open("w") as log:
                        subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                                       check=True, timeout=args.timeout)
                    record["status"] = "passed"
                except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
                    record.update(status="failed", reason=str(error))
                record["seconds"] = time.monotonic() - started
                manifest["operations"].append(record)
                manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
                print(f"{name}: {record['status']}", flush=True)
    raise SystemExit(any(record["status"] != "passed" for record in manifest["operations"]))


if __name__ == "__main__":
    main()
