#!/usr/bin/env python3
"""Validate and summarize previous/current runs from a revision-run.json manifest."""

import argparse
import json
from pathlib import Path


def read_json(path):
    return json.loads(path.read_text())


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def quantiles(values):
    ordered = sorted(values)

    def percentile(fraction):
        position = fraction * (len(ordered) - 1)
        lower = int(position)
        upper = min(lower + 1, len(ordered) - 1)
        return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)

    return dict(zip(["min", "p10", "median", "p90", "max"],
                    [percentile(value) for value in [0, .1, .5, .9, 1]]))


def summarize(rows):
    current = sum(row["current"]["wall_ms"] for row in rows)
    previous = sum(row["previous"]["wall_ms"] for row in rows)
    return {
        "files": len(rows),
        "nodes": sum(row["nodes"] for row in rows),
        "current_over_previous_wall": quantiles(row["wall_ratio"] for row in rows),
        "current_over_previous_cpu": quantiles(row["cpu_ratio"] for row in rows),
        "current_over_mainline_wall": quantiles(
            row["current"]["paired_mainline_wall_ratio"] for row in rows),
        "mainline_current_over_previous_wall": quantiles(
            row["current"]["mainline_wall_ms"] / row["previous"]["mainline_wall_ms"]
            for row in rows),
        "summed_current_wall_ms": current,
        "summed_previous_wall_ms": previous,
        "ratio_of_summed_wall_medians": current / previous,
        "files_over_5_percent_faster": sum(row["wall_ratio"] < .95 for row in rows),
        "files_over_5_percent_slower": sum(row["wall_ratio"] > 1.05 for row in rows),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    manifest = read_json(args.run / "revision-run.json")
    require(len(manifest["operations"]) == 4 * len(manifest["samples"]),
            "expected original/mutated runs for both revisions in every sample")
    runs = {}
    metadata = {}
    for operation in manifest["operations"]:
        name = operation["name"]
        require(operation["status"] == "passed", f"incomplete operation: {name}")
        prefix = args.run / "bench-outputs" / name
        run = read_json(Path(f"{prefix}-run.json"))
        require(not run["partial"] and run["failed"] == 0 and
                run["completed"] == run["planned"], f"incomplete run: {name}")
        require(run["failures"]["count"] == 0, f"comparison failure: {name}")
        for identity in ["source_sha256", "binary_sha256"]:
            require(run["tool"][identity] == manifest["variants"][operation["variant"]][identity],
                    f"{identity} differs: {name}")
        rows = [json.loads(line) for line in Path(f"{prefix}-files.jsonl").read_text().splitlines()]
        keyed = {(row["path"], row["benchmark"]): row for row in rows}
        require(len(keyed) == len(rows), f"duplicate records: {name}")
        inputs = {entry["path"]: entry for entry in manifest["samples"][operation["sample"]]["inputs"]}
        expected_files = inputs.keys()
        benchmarks = set(run["benchmarks"]) | {run.get("parse_benchmark", "cold-parse")}
        expected = {(path, benchmark) for path in expected_files for benchmark in benchmarks}
        require(keyed.keys() == expected, f"missing or unexpected records: {name}")
        for row in rows:
            require(row["source_sha256"] == inputs[row["path"]]["sha256"],
                    f"input identity differs: {name}: {row['path']}")
            require(row["failures"] == 0 and row["repeats"] == manifest["repeat"],
                    f"failed or incomplete record: {name}: {row['path']}")
            require(row["ignored_differences"] == 0 or row["benchmark"].startswith("seek-"),
                    f"ignored non-seek difference: {name}: {row['path']}")
        key = (operation["sample"], operation["mutated"], operation["variant"])
        require(key not in runs, f"duplicate operation: {key}")
        runs[key] = keyed
        metadata[name] = {key: value for key, value in run.items() if key != "inputs"}

    records = []
    summaries = []
    for sample in manifest["samples"]:
        original_sizes = {entry["path"]: entry["bytes"] for entry in manifest["samples"][sample]["inputs"]}
        for mutated in [False, True]:
            state = "mutated" if mutated else "original"
            old_run = metadata[f"{sample}-previous-{state}"]
            new_run = metadata[f"{sample}-current-{state}"]
            for field in ["registry", "grammar_sha256", "benchmarks", "arguments", "machine"]:
                old_value, new_value = old_run[field], new_run[field]
                if field == "arguments":
                    old_value = {key: value for key, value in old_value.items() if key != "output"}
                    new_value = {key: value for key, value in new_value.items() if key != "output"}
                require(old_value == new_value, f"different {field}: {sample}, {state}")
            previous = runs[(sample, mutated, "previous")]
            current = runs[(sample, mutated, "current")]
            require(previous.keys() == current.keys(), f"different coverage: {sample}, {mutated}")
            pair_rows = []
            for key in sorted(current):
                old, new = previous[key], current[key]
                fields = ["path", "grammar", "benchmark", "source_sha256", "tested_sha256",
                          "source_bytes", "nodes", "slab_bytes", "groups", "group_capacity"]
                require(all(old[field] == new[field] for field in fields),
                        f"different input or layout: {sample}, {mutated}, {key}")
                require(old["ignored_differences"] == new["ignored_differences"],
                        f"seek difference count changed: {sample}, {mutated}, {key}")
                row = {field: new[field] for field in fields}
                row.update(sample=sample, mutated=mutated,
                           original_bytes=original_sizes[new["path"]])
                for label, source in [("previous", old), ("current", new)]:
                    row[label] = {
                        "wall_ms": source["squat"]["wall_ms"],
                        "cpu_ms": source["squat"]["cpu_ms"],
                        "mainline_wall_ms": source["mainline"]["wall_ms"],
                        "paired_mainline_wall_ratio": source["ratios"]["wall_ms"],
                        "ignored_differences": source["ignored_differences"],
                    }
                # Repeats are paired against mainline within each run, but the
                # revision comparison is a ratio of separate per-file medians.
                row["wall_ratio"] = new["squat"]["wall_ms"] / old["squat"]["wall_ms"]
                row["cpu_ratio"] = new["squat"]["cpu_ms"] / old["squat"]["cpu_ms"]
                pair_rows.append(row)
            records.extend(pair_rows)
            subsets = {"all": pair_rows}
            large = [row for row in pair_rows if row["original_bytes"] >= 1024 * 1024]
            if large:
                subsets["at_least_1_mib"] = large
            for subset, selected in subsets.items():
                for benchmark in sorted({row["benchmark"] for row in selected}):
                    rows = [row for row in selected if row["benchmark"] == benchmark]
                    summaries.append(dict(sample=sample, mutated=mutated, subset=subset,
                                          benchmark=benchmark, **summarize(rows)))

    result = dict(schema=1, manifest=manifest, runs=metadata, summaries=summaries, records=records,
                  ratio_contract="current/previous per-file medians, then equal-file quantiles; lower is faster",
                  large_subset_contract="at least 1 MiB of original input, also for mutated runs")
    with args.output.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")
    for summary in summaries:
        print(summary["sample"], "mutated" if summary["mutated"] else "original",
              summary["subset"], summary["benchmark"], summary["files"],
              f'{summary["current_over_previous_wall"]["median"]:.3f}')


if __name__ == "__main__":
    main()
