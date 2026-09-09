#!/usr/bin/env python3
"""Validate and summarize cached/uncached cursor runs on identical corpus inputs."""
import argparse
import hashlib
import json
from pathlib import Path

WORKLOADS = ["cursor-forward", "cursor-backward", "walk-forward", "walk-backward"]
PERCENTILES = [0, 50, 90, 95, 99, 100]


def read_rows(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def percentiles(values):
    ordered = sorted(values)
    result = []
    for percentile in PERCENTILES:
        position = (len(ordered) - 1) * percentile / 100
        lower = int(position)
        upper = min(lower + 1, len(ordered) - 1)
        result.append(ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower))
    return result


def summarize(files):
    result = dict(files=len(files), metrics={})
    for metric in ["wall_ms", "cpu_ms"]:
        ratios = [entry["metrics"][metric]["cached_over_uncached"] for entry in files]
        result["metrics"][metric] = dict(cached_over_uncached=percentiles(ratios),
                                        cached_wins=sum(value < 1 for value in ratios))
    return result


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source = json.loads((args.run / "container-run.json").read_text())
    binary_hash = hashlib.sha256((args.run / "target/release/squatter-bench").read_bytes()).hexdigest()
    inputs = {entry["path"]: entry for entry in source["inputs"]}
    variants = {}
    for name, mutated in [("baseline", False), ("mutated", True)]:
        prefix = args.run / "bench-outputs" / name
        manifest = json.loads(prefix.with_name(name + "-run.json").read_text())
        require(not manifest["partial"] and manifest["failed"] == 0
                and manifest["completed"] == manifest["planned"] == len(inputs),
                f"incomplete or failing run: {name}")
        require(manifest["arguments"]["mutate"] == mutated, f"mutation mode differs: {name}")
        require(manifest["tool"]["source_sha256"] == source["source_sha256"]
                and manifest["tool"]["binary_sha256"] == binary_hash,
                f"source or binary identity differs: {name}")
        rows = read_rows(prefix.with_name(name + "-files.jsonl"))
        index = {(row["path"], row["benchmark"]): row for row in rows}
        require(len(index) == len(rows), f"duplicate file/benchmark row: {name}")
        comparisons = {}
        for workload in WORKLOADS:
            files = []
            for path, source_input in inputs.items():
                plain = index.get((path, workload))
                cached = index.get((path, workload + "-cached"))
                require(plain is not None and cached is not None, f"missing pair: {name}/{path}/{workload}")
                require(plain["failures"] == cached["failures"] == 0
                        and plain["ignored_differences"] == cached["ignored_differences"] == 0,
                        f"failed comparison: {name}/{path}/{workload}")
                require(plain["source_sha256"] == cached["source_sha256"] == source_input["sha256"]
                        and plain["tested_sha256"] == cached["tested_sha256"]
                        and plain["grammar"] == cached["grammar"] == source_input["grammar"]
                        and plain["repeats"] == cached["repeats"] == manifest["arguments"]["repeat"],
                        f"input or repeat identity differs: {name}/{path}/{workload}")
                metrics = {}
                for metric in ["wall_ms", "cpu_ms"]:
                    uncached_time = plain["squat"][metric]
                    cached_time = cached["squat"][metric]
                    require(uncached_time > 0 and cached_time > 0, "nonpositive timing")
                    metrics[metric] = dict(uncached=uncached_time, cached=cached_time,
                                           cached_over_uncached=cached_time / uncached_time,
                                           uncached_over_mainline=plain["ratios"][metric],
                                           cached_over_mainline=cached["ratios"][metric])
                files.append(dict(path=path, grammar=plain["grammar"], tested_sha256=plain["tested_sha256"],
                                  nodes=plain["nodes"], original_bytes=source_input["bytes"],
                                  tested_bytes=plain["source_bytes"], metrics=metrics))
            summaries = {}
            for language in ["all", *sorted({entry["grammar"] for entry in files})]:
                selected = [entry for entry in files if language == "all" or entry["grammar"] == language]
                summaries[language] = summarize(selected)
            size_summaries = {}
            for label, large in [("under_1_mib", False), ("at_least_1_mib", True)]:
                selected = [entry for entry in files if (entry["original_bytes"] >= 1048576) == large]
                if selected:
                    size_summaries[label] = summarize(selected)
            comparisons[workload] = dict(summaries=summaries, size_summaries=size_summaries, files=files)
        variants[name] = dict(mutated=mutated, repeat=manifest["arguments"]["repeat"],
                              machine=manifest["machine"], counter_status=manifest["counter_status"],
                              comparisons=comparisons)
    result = dict(schema=1, source_sha256=source["source_sha256"], binary_sha256=binary_hash,
                  tool_sha=source["tool_sha"], code_corpora_sha=source["code_corpora_sha"],
                  image=source["image"], inputs=source["inputs"], grammars=source["grammars"],
                  percentiles=PERCENTILES, variants=variants,
                  caveats=[
                      "Cached/uncached ratios divide separate per-file timing medians in the same run; they are not paired-repeat ratios.",
                      "Each cursor variant separately retains paired-repeat ratios against mainline.",
                      "Native cursor workloads collect node identities without attributes; walk workloads collect full attributes.",
                      "Backward attribute walks use the common compatibility adapter, creating a fresh cursor per node; native backward navigation does not.",
                      "Both squat cursors use the same bulk attribute FFI call, unlike earlier walk benchmarks using individual node accessors.",
                      "Cursor allocation, movement, snapshot collection, and destruction are timed; tree construction is separate.",
                      "This is a bounded sample on one machine, not a universal performance claim.",
                  ])
    with args.output.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")
    for name, variant in variants.items():
        for workload, comparison in variant["comparisons"].items():
            summary = comparison["summaries"]["all"]["metrics"]["wall_ms"]
            print(f"{name} {workload}: median cached/uncached {summary['cached_over_uncached'][1]:.3f}; "
                  f"{summary['cached_wins']}/{len(inputs)} files faster")


if __name__ == "__main__":
    main()
