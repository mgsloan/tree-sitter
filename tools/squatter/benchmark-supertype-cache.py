#!/usr/bin/env python3
"""Paired process-CPU benchmarks of supertype dictionary implementations."""
import argparse
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASE = "ab9143b2fa3aff3fbde2062eb85988be7c6c7042"
OPERATIONS = ("context_cold", "pack_cold", "load_cold", "pack_warm", "load_warm",
              "pack_context", "membership_walk")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(command, **kwargs):
    return subprocess.run(command, check=True, text=True, capture_output=True, **kwargs)


def prepare(output, baseline_snapshot=None):
    paths = run(["git", "ls-tree", "-r", "--name-only", BASE,
                 "lib/src", "lib/include", "lib/squat"], cwd=ROOT).stdout.splitlines()
    paths = [p for p in paths if not p.startswith("lib/squat/") or
             "/include/" in p or len(Path(p).parts) == 3]
    for variant in ("baseline", "current"):
        checkout = output / variant
        if variant == "baseline" and baseline_snapshot:
            shutil.copytree(baseline_snapshot / "lib", checkout / "lib", dirs_exist_ok=True)
        names = paths + (["lib/squat/supertypes.c"] if variant == "current" else [])
        if variant == "baseline" and baseline_snapshot:
            names = []
        for name in names:
            path = checkout / name
            path.parent.mkdir(parents=True, exist_ok=True)
            content = ((ROOT / name).read_bytes() if variant == "current" else
                       subprocess.check_output(["git", "show", f"{BASE}:{name}"], cwd=ROOT))
            path.write_bytes(content)
        if variant == "baseline" and not baseline_snapshot:
            patch = ROOT / "lib/squat/experiments/supertype-cache-baseline.patch"
            run(["patch", "-p1", "--input", str(patch)], cwd=checkout)
        build = run(["make", "-j4", "../../build/squat/libtree-sitter-squat.a",
                     "../../build/squat/runtime.o", "CFLAGS=-O3 -g -fno-omit-frame-pointer"],
                    cwd=checkout / "lib/squat")
        (output / f"{variant}-build.log").write_text(build.stdout + build.stderr)
        experiments = checkout / "lib/squat/experiments"
        experiments.mkdir(exist_ok=True)
        for name in ("supertype-cache.c", "memory.c"):
            (experiments / name).write_bytes((ROOT / "lib/squat/experiments" / name).read_bytes())
        for kind in ("time", "memory"):
            command = ["cc", "-std=c11", "-O3", "-g", "-fno-omit-frame-pointer",
                       "-I" + str(checkout / "lib/include"), "-I" + str(checkout / "lib/src"),
                       str(experiments / "supertype-cache.c"),
                       str(checkout / "build/squat/libtree-sitter-squat.a"),
                       str(checkout / "build/squat/runtime.o"), "-ldl", "-o",
                       str(output / f"{variant}-{kind}")]
            if kind == "memory":
                command += ["-DBENCH_MEMORY",
                            "-Wl,--wrap=malloc,--wrap=calloc,--wrap=realloc,--wrap=aligned_alloc,--wrap=free"]
            run(command)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--cpu", type=int, default=0)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--prepare", action="store_true")
    parser.add_argument("--baseline-snapshot", type=Path,
                        help="Use an existing source snapshot instead of the tree-local baseline")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if args.prepare:
        prepare(output, args.baseline_snapshot)
    inputs = json.loads(args.inputs.read_text())
    for item in inputs:
        for kind in ("source", "library"):
            assert digest(Path(item[kind])) == item[kind + "_sha256"]
    result = {"baseline_commit": BASE, "baseline_note": "HEAD plus adaptive 16-bit indexes; tree-local dictionaries",
              "baseline_patch_sha256": digest(ROOT / "lib/squat/experiments/supertype-cache-baseline.patch"),
              "compiler": run(["cc", "--version"]).stdout.splitlines()[0],
              "flags": "-O3 -g -fno-omit-frame-pointer; no LTO; points; group=16; alignment=8",
              "cpu": args.cpu, "lscpu": json.loads(run(["lscpu", "--json"]).stdout),
              "system": platform.platform(), "started": time.time(), "inputs": inputs,
              "variants": {}, "timings": [], "memory": []}
    if args.baseline_snapshot:
        result["baseline_commit"] = None
        result["baseline_note"] = "Source snapshot: " + str(args.baseline_snapshot.resolve())
        result.pop("baseline_patch_sha256")
    for variant in ("baseline", "current"):
        checkout = output / variant
        result["variants"][variant] = {
            "binaries": {kind: digest(output / f"{variant}-{kind}") for kind in ("time", "memory")},
            "sources": {str(p.relative_to(checkout)): digest(p) for p in sorted((checkout / "lib").rglob("*")) if p.is_file()}}
    raw = output / "results.json"
    def save():
        raw.write_text(json.dumps(result, indent=2) + "\n")
    for round_number in range(args.rounds):
        for case in inputs:
            for variant in (("baseline", "current") if round_number % 2 == 0 else ("current", "baseline")):
                command = ["taskset", "-c", str(args.cpu), str(output / f"{variant}-time"),
                           case["library"], case["symbol"], case["source"]]
                measured = json.loads(run(command).stdout)
                result["timings"].append(dict(case=case["name"], variant=variant,
                                              round=round_number, measured=measured))
                save()
            print(f"round {round_number + 1}: {case['name']}", flush=True)
    for case in inputs:
        for variant in ("baseline", "current"):
            for repack in (False, True):
                env = os.environ.copy()
                if repack: env["SQ_REPACK"] = "1"
                else: env.pop("SQ_REPACK", None)
                command = ["taskset", "-c", str(args.cpu), str(output / f"{variant}-memory"),
                           case["library"], case["symbol"], case["source"]]
                measured = json.loads(run(command, env=env).stdout)
                result["memory"].append(dict(case=case["name"], variant=variant,
                                             repack=repack, measured=measured))
                save()
    summary = []
    for case in inputs:
        rows = [r for r in result["timings"] if r["case"] == case["name"]]
        for key in ("nodes", "membership_checksum", "groups"):
            assert len({r["measured"][key] for r in rows}) == 1, (case["name"], key)
        entry = {"case": case["name"], "operations": {}}
        for op in OPERATIONS:
            medians = {v: [statistics.median(r["measured"][op]["samples_us"])
                           for r in rows if r["variant"] == v] for v in ("baseline", "current")}
            entry["operations"][op] = {v + "_us": statistics.median(values) for v, values in medians.items()}
            entry["operations"][op]["ratios_by_round"] = [c / b for b, c in zip(medians["baseline"], medians["current"])]
        summary.append(entry)
    result["finished"] = time.time()
    result["summary"] = summary
    save()
    print(raw)


if __name__ == "__main__":
    main()
