#!/usr/bin/env python3
"""Validate and summarize every supported result in one Squatter run directory."""

import argparse
import hashlib
import json
from pathlib import Path


def read(path):
    return json.loads(path.read_text())


def rows(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def quantiles(values):
    ordered = sorted(values)
    if not ordered:
        return None

    def at(fraction):
        position = fraction * (len(ordered) - 1)
        low = int(position)
        high = min(low + 1, len(ordered) - 1)
        return ordered[low] + (ordered[high] - ordered[low]) * (position - low)

    return dict(zip(("min", "p10", "median", "p90", "max"),
                    (at(value) for value in (0, .1, .5, .9, 1))))


def benchmark_results(directory):
    results = {}
    for manifest_path in sorted((directory / "bench-outputs").glob("*-run.json")):
        manifest = read(manifest_path)
        if manifest["partial"] or manifest["failed"] or manifest["failures"]["count"]:
            raise SystemExit(f"incomplete or failed benchmark: {manifest_path}")
        name = manifest_path.name.removesuffix("-run.json")
        records = rows(manifest_path.with_name(name + "-files.jsonl"))
        keyed = {(row["path"], row["benchmark"]): row for row in records}
        if len(keyed) != len(records):
            raise SystemExit(f"duplicate benchmark rows: {manifest_path}")
        profile = manifest["arguments"].get("pressure_label") or manifest["pressure"]["mode"]
        key = (manifest["arguments"]["mutate"], profile)
        if key in results:
            raise SystemExit(f"duplicate mutation/pressure condition: {manifest_path}")
        results[key] = (manifest, keyed)
    return results


def pressure_summary(results):
    records = []
    summaries = []
    baselines = {}
    for key, value in results.items():
        if value[0]["pressure"]["mode"] == "none":
            if key[0] in baselines:
                raise SystemExit(f"multiple isolated baselines for mutated={key[0]}")
            baselines[key[0]] = value
    for (mutated, profile), (manifest, pressured) in results.items():
        mode = manifest["pressure"]["mode"]
        if mode == "none":
            continue
        if mutated not in baselines:
            raise SystemExit(f"pressure profile {profile} has no matching isolated run")
        baseline_manifest, baseline = baselines[mutated]
        if baseline.keys() != pressured.keys():
            raise SystemExit(f"pressure mode {mode} changed benchmark coverage")
        for field in ("schema", "timing_contract", "feller_contract", "summary_contract", "parse_order",
                      "registry", "grammar_sha256", "benchmarks"):
            if baseline_manifest.get(field) != manifest.get(field):
                raise SystemExit(f"pressure mode {mode} changed {field}")
        condition = []
        for key in sorted(pressured):
            before = baseline[key]
            after = pressured[key]
            for field in ("path", "grammar", "benchmark", "source_sha256",
                          "tested_sha256", "nodes", "slab_bytes"):
                if before[field] != after[field]:
                    raise SystemExit(f"pressure comparison changed {field}: {key}")
            mainline = after["mainline"]["wall_ms"] / before["mainline"]["wall_ms"]
            squat = after["squat"]["wall_ms"] / before["squat"]["wall_ms"]
            record = dict(path=after["path"], grammar=after["grammar"],
                          benchmark=after["benchmark"], mutated=mutated,
                          pressure=profile, pressure_mode=mode,
                          mainline_slowdown=mainline, squat_slowdown=squat,
                          relative_slowdown=squat / mainline)
            first_feller, second_feller = before.get("feller"), after.get("feller")
            if (first_feller or {}).get("status") != (second_feller or {}).get("status"):
                raise SystemExit(f"pressure comparison changed feller coverage: {key}")
            if second_feller:
                record["feller_status"] = second_feller["status"]
                if second_feller["status"] == "ok":
                    feller = (second_feller["metrics"]["wall_ms"] /
                              first_feller["metrics"]["wall_ms"])
                    record.update(feller_slowdown=feller,
                                  feller_relative_slowdown=feller / mainline,
                                  feller_pack_relative_slowdown=feller / squat)
            records.append(record)
            condition.append(record)
        for benchmark in sorted({row["benchmark"] for row in condition}):
            selected = [row for row in condition if row["benchmark"] == benchmark]
            direct = [row for row in selected if row.get("feller_status") == "ok"]
            summaries.append(dict(
                mutated=mutated, pressure=profile, pressure_mode=mode,
                benchmark=benchmark, files=len(selected),
                mainline_slowdown=quantiles(row["mainline_slowdown"] for row in selected),
                squat_slowdown=quantiles(row["squat_slowdown"] for row in selected),
                relative_slowdown=quantiles(row["relative_slowdown"] for row in selected),
                feller_successful=dict(
                    files=len(direct),
                    mainline_slowdown=quantiles(row["mainline_slowdown"] for row in direct),
                    squat_slowdown=quantiles(row["squat_slowdown"] for row in direct),
                    relative_slowdown=quantiles(row["relative_slowdown"] for row in direct),
                    feller_slowdown=quantiles(row["feller_slowdown"] for row in direct),
                    feller_relative_slowdown=quantiles(row["feller_relative_slowdown"] for row in direct),
                    feller_pack_relative_slowdown=quantiles(row["feller_pack_relative_slowdown"] for row in direct),
                ),
            ))
    return records, summaries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--matrix", type=Path,
                        help="matrix used for the run (default: RUN/matrix.toml)")
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    manifest = read(arguments.run / "container-run.json")
    if manifest.get("partial", False):
        raise SystemExit("run is incomplete")
    failed = [operation for operation in manifest["operations"]
              if operation["status"] != "passed"]
    if failed:
        raise SystemExit(f"run contains failed operations: {failed}")
    matrix_path = arguments.matrix or arguments.run / "matrix.toml"
    matrix_bytes = matrix_path.read_bytes()
    if manifest.get("matrix_sha256") != hashlib.sha256(matrix_bytes).hexdigest():
        raise SystemExit(f"matrix does not match run manifest: {matrix_path}")
    benchmarks = benchmark_results(arguments.run)
    pressure_records, pressure_summaries = pressure_summary(benchmarks)
    result = dict(
        schema=3,
        provenance={key: manifest.get(key) for key in
                    ("source_sha256", "matrix_sha256", "tool_sha", "image",
                     "code_corpora_sha", "grammars")},
        inputs=manifest["inputs"],
        benchmark_runs={f"{'mutated' if key[0] else 'original'}/{key[1]}": value[0]
                        for key, value in benchmarks.items()},
        pressure_summaries=pressure_summaries,
        pressure_records=pressure_records,
        ratio_contract="pressure/isolated per-file medians; relative slowdown divides by mainline slowdown; feller pack relative slowdown divides by Squatter slowdown; skipped feller inputs are excluded",
    )
    with arguments.output.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")
    for row in pressure_summaries:
        print("mutated" if row["mutated"] else "original", row["pressure"],
              row["benchmark"], "relative", f"{row['relative_slowdown']['median']:.3f}")


if __name__ == "__main__":
    main()
