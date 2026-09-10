#!/usr/bin/env python3
"""Benchmark an uploaded binary/corpus bundle without building on the target."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("--cpu", type=int, default=0)
    parser.add_argument("--repeat", type=int, default=9)
    parser.add_argument("--variants", nargs="+", default=[
        "ids", "all", "scalar", "swar", "avx2", "group32", "group64"])
    parser.add_argument("--samples", nargs="+", default=["bounded", "large"])
    parser.add_argument("--large-variants", nargs="+")
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--output-name", default="cloud-results")
    parser.add_argument("--benchmarks", nargs="+")
    parser.add_argument("--unpack-sizes", type=int, nargs="*", default=[16, 32, 64])
    args = parser.parse_args()
    bundle = args.bundle.resolve()
    output = bundle / args.output_name
    output.mkdir(exist_ok=args.resume)
    manifest_path = output / "manifest.json"
    manifest = {
        "schema": 1,
        "platform": platform.uname()._asdict(),
        "cpu": subprocess.check_output(["lscpu", "--json"], text=True),
        "libc": subprocess.check_output(["ldd", "--version"], text=True),
        "affinity": [args.cpu],
        "repeat": args.repeat,
        "variants": args.variants,
        "samples": args.samples,
        "large_variants": args.large_variants,
        "local_build_manifest_sha256": digest(bundle / "local-build-manifest.json"),
        "operations": [],
    }
    if args.resume:
        previous = json.loads(manifest_path.read_text())
        if previous["repeat"] != args.repeat or previous["affinity"] != [args.cpu]:
            parser.error("resume must preserve repeat count and CPU affinity")
        manifest["operations"] = previous["operations"]
        manifest["interrupted_operations"] = previous.get("interrupted_operations", [])

    def save():
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")

    def run(name, command, **details):
        previous = next((op for op in manifest["operations"] if op["name"] == name), None)
        if previous:
            if previous["status"] == "passed":
                if previous["command"] != command or previous["binary_sha256"] != details["binary_sha256"]:
                    parser.error(f"resume changed command or binary for {name}")
                print(name, "already passed", flush=True)
                return
            parser.error(f"unfinished operation {name}; preserve its artifacts before resuming")
        operation = {"name": name, "command": command, **details,
                     "status": "running", "proc_stat_before": Path("/proc/stat").read_text()}
        manifest["operations"].append(operation)
        save()
        started = time.monotonic()
        with (output / (name + ".log")).open("w") as log:
            result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT)
        operation.update(status="passed" if result.returncode == 0 else "failed",
                         seconds=time.monotonic() - started,
                         proc_stat_after=Path("/proc/stat").read_text())
        save()
        print(name, operation["status"], flush=True)
        if result.returncode:
            raise SystemExit(result.returncode)

    # Invoke the guest loader explicitly: the uploaded executable's ELF
    # interpreter may name a build-host Nix store path absent on this machine.
    prefix = ["taskset", "-c", str(args.cpu), "/lib64/ld-linux-x86-64.so.2"]
    for size in args.unpack_sizes:
        binary = bundle / f"unpack-{size}"
        run(f"unpack-{size}", prefix + [str(binary)], binary_sha256=digest(binary))

    workloads = args.benchmarks or ["walk-forward", "walk-iterator", "walk-iterator-cached",
                                   "cursor-forward", "iterator-forward", "iterator-forward-cached", "cold-parse"]
    for sample in args.samples:
        # Reverse variant order for the second sample to expose temporal drift.
        variants = args.variants if sample == args.samples[0] else args.variants[::-1]
        if sample == "large" and args.large_variants:
            variants = args.large_variants
        selected_workloads = workloads
        if sample == "large":
            # Walks compare all nodes/attributes. Avoid repeating unrelated,
            # expensive relationship checks in each large-file timing pass.
            selected_workloads = [name for name in workloads if name != "cold-parse"]
        for index, variant in enumerate(variants):
            binary = bundle / "binaries" / variant / "squatter-bench"
            for mutated in ([False, True] if index % 2 == 0 else [True, False]):
                name = f"{variant}-{sample}-" + ("mutated" if mutated else "original")
                command = prefix + [str(binary), "--code-corpora", str(bundle / sample / "corpus"),
                    "--registry", str(bundle / sample / "registry.json"), "--all",
                    "--repeat", str(args.repeat), "--output-directory", str(output),
                    "--output", name, *selected_workloads]
                if mutated:
                    command.append("--mutate")
                run(name, command, variant=variant, sample=sample, mutated=mutated,
                    binary_sha256=digest(binary))
    save()


if __name__ == "__main__":
    main()
