#!/usr/bin/env python3
"""Validate and summarize an uploaded iterator benchmark matrix."""
import argparse
import csv
import io
import json
from pathlib import Path


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def quantiles(values):
    values = sorted(values)
    result = {}
    for name, fraction in [("min", 0), ("p10", .1), ("median", .5), ("p90", .9), ("max", 1)]:
        position = fraction * (len(values) - 1)
        lower = int(position)
        upper = min(lower + 1, len(values) - 1)
        result[name] = values[lower] + (values[upper] - values[lower]) * (position - lower)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path)
    parser.add_argument("build_manifest", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--baseline", default="ids")
    parser.add_argument("--instance", type=Path)
    args = parser.parse_args()
    manifest = json.loads((args.results / "manifest.json").read_text())
    builds = json.loads(args.build_manifest.read_text())
    runs, metadata, micro = {}, {}, []
    for operation in manifest["operations"]:
        name = operation["name"]
        require(operation["status"] == "passed", f"incomplete operation: {name}")
        if name.startswith("unpack-"):
            lines = (args.results / (name + ".log")).read_text().splitlines()
            lines = [line for line in lines if not line.startswith("checksum:")]
            for row in csv.DictReader(io.StringIO("\n".join(lines))):
                micro.append(dict(group_size=int(row["group_size"]),
                                  unpack_slots=int(row.get("unpack_slots") or row["group_size"]),
                                  bits=int(row["bits"]),
                                  kernel=row["kernel"], ns_per_value=float(row["ns_per_value"])))
            continue
        run = json.loads((args.results / (name + "-run.json")).read_text())
        require(not run["partial"] and not run["failed"] and run["completed"] == run["planned"],
                f"incomplete benchmark: {name}")
        require(not run["failures"]["count"], f"comparison failures: {name}")
        require(operation["binary_sha256"] == builds["variants"][operation["variant"]]["binary_sha256"],
                f"uploaded binary differs: {name}")
        require(run["tool"]["binary_sha256"] == operation["binary_sha256"],
                f"measured binary differs: {name}")
        rows = [json.loads(line) for line in
                (args.results / (name + "-files.jsonl")).read_text().splitlines()]
        keyed = {(row["path"], row["benchmark"]): row for row in rows}
        require(len(keyed) == len(rows), f"duplicate rows: {name}")
        paths = {row["path"] for row in rows}
        # Parsing is prerequisite setup and is recorded even when its additional
        # relationship-validation workload was not selected.
        benchmarks = set(run["benchmarks"]) | {"cold-parse"}
        require(set(keyed) == {(path, bench) for path in paths for bench in benchmarks},
                f"incomplete workload coverage: {name}")
        require(all(not row["failures"] and row["repeats"] == manifest["repeat"] for row in rows),
                f"failed or incomplete row: {name}")
        runs[(operation["sample"], operation["mutated"], operation["variant"])] = keyed
        metadata[name] = {key: value for key, value in run.items() if key != "inputs"}

    variants = manifest.get("variants", ["ids", "all", "scalar", "swar", "avx2", "group32", "group64"])
    samples = manifest.get("samples", ["bounded", "large"])
    expected_runs = set()
    for sample in samples:
        selected = variants
        if sample == "large" and manifest.get("large_variants"):
            selected = manifest["large_variants"]
        expected_runs.update((sample, mutated, variant)
                             for mutated in [False, True] for variant in selected)
    require(set(runs) == expected_runs, "incomplete experiment matrix")
    for sample, saved in builds["samples"].items():
        inputs = {entry["path"]: entry for entry in saved["inputs"]}
        original = runs[(sample, False, args.baseline)]
        require({path for path, _ in original} == inputs.keys(), f"different saved sample: {sample}")
        require(all(row["source_sha256"] == inputs[path]["sha256"]
                    for (path, _), row in original.items()), f"different saved bytes: {sample}")

    summaries = []
    for (sample, mutated, variant), rows in runs.items():
        baseline = runs[(sample, mutated, args.baseline)]
        require(rows.keys() == baseline.keys(), f"different input/workload coverage: {sample}/{variant}")
        for key, row in rows.items():
            fields = ["source_sha256", "tested_sha256", "grammar", "nodes"]
            if builds["variants"][variant]["group_size"] == builds["variants"][args.baseline]["group_size"]:
                fields += ["slab_bytes", "groups", "group_capacity"]
            require(all(row[field] == baseline[key][field] for field in fields),
                    f"different input: {sample}/{variant}/{key}")
        paths = sorted({path for path, _ in rows})
        # Original byte length determines membership even for mutated inputs.
        original = runs[(sample, False, variant)]
        size_benchmark = next(iter(original))[1]
        subsets = {"all": paths}
        large = [path for path in paths if original[(path, size_benchmark)]["source_bytes"] >= 1024**2]
        if large:
            subsets["at_least_1_mib"] = large
        comparisons = [
            ("iterator_navigation/cursor", rows, "iterator-forward", rows, "cursor-forward"),
            ("cached_navigation/plain", rows, "iterator-forward-cached", rows, "iterator-forward"),
            ("iterator_attributes/cursor", rows, "walk-iterator", rows, "walk-forward"),
            ("cached_attributes/plain", rows, "walk-iterator-cached", rows, "walk-iterator"),
            ("cached_attributes/cursor", rows, "walk-iterator-cached", rows, "walk-forward"),
            ("cached_attributes/default_cached", rows, "walk-iterator-cached", baseline, "walk-iterator-cached"),
        ]
        for subset, selected in subsets.items():
            for label, numerator, num_bench, denominator, den_bench in comparisons:
                if (selected[0], num_bench) not in numerator or (selected[0], den_bench) not in denominator:
                    continue
                pairs = [(numerator[(path, num_bench)], denominator[(path, den_bench)]) for path in selected]
                summary = dict(sample=sample, mutated=mutated, variant=variant, subset=subset,
                               comparison=label, files=len(selected))
                for metric in ["wall_ms", "cpu_ms"]:
                    summary[metric] = quantiles(a["squat"][metric] / b["squat"][metric] for a, b in pairs)
                summary["mainline_control"] = quantiles(
                    a["mainline"]["wall_ms"] / b["mainline"]["wall_ms"] for a, b in pairs)
                summary["summed_wall_ratio"] = (sum(a["squat"]["wall_ms"] for a, _ in pairs) /
                                                sum(b["squat"]["wall_ms"] for _, b in pairs))
                summaries.append(summary)
                print(sample, "mutated" if mutated else "original", variant, subset, label,
                      f'{summary["wall_ms"]["median"]:.3f}')
    records = []
    for (sample, mutated, variant), rows in runs.items():
        for row in rows.values():
            records.append(dict(sample=sample, mutated=mutated, variant=variant, **row))
    result = dict(schema=1, manifest=manifest, builds=builds, baseline=args.baseline, runs=metadata,
                  ratio_contract="Ratios of separate per-file medians, then equal-file quantiles; lower is faster.",
                  summaries=summaries, unpack=micro, records=records)
    if args.instance:
        result["cloud_instance"] = json.loads(args.instance.read_text())
    with args.output.open("x") as output:
        # One row per line keeps the complete artifact readable without huge indentation overhead.
        header = json.dumps({key: value for key, value in result.items() if key != "records"}, indent=2)
        output.write(header.removesuffix("\n}"))
        output.write(',\n  "records": [\n')
        output.write(',\n'.join('    ' + json.dumps(row, separators=(',', ':')) for row in records))
        output.write('\n  ]\n}\n')


if __name__ == "__main__":
    main()
