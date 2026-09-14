#!/usr/bin/env python3
"""Validate uploaded storage variants and retain per-file timings and controls."""

import argparse
import importlib.util
import json
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "revisions", Path(__file__).with_name("summarize-revisions.py"))
revisions = importlib.util.module_from_spec(spec)
spec.loader.exec_module(revisions)


def read(path):
    return json.loads(path.read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path, help="contains bounded-results and large-results")
    parser.add_argument("--build-manifest", type=Path, required=True)
    parser.add_argument("--input-manifest", type=Path, required=True)
    parser.add_argument("--previous", default="v3")
    parser.add_argument("--current", default="v4")
    parser.add_argument("--slab-delta", type=int, default=-16)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    build = read(args.build_manifest)
    inputs = read(args.input_manifest)["samples"]
    manifests, runs, metadata = {}, {}, {}
    for sample in ["bounded", "large"]:
        folder = args.results / f"{sample}-results"
        manifest = read(folder / "manifest.json")
        manifests[sample] = manifest
        assert len(manifest["operations"]) == 8
        expected_inputs = {entry["path"]: entry for entry in inputs[sample]["inputs"]}
        for operation in manifest["operations"]:
            name = operation["name"]
            variant = operation["variant"]
            assert operation["status"] == "passed", name
            run = read(folder / f"{name}-run.json")
            assert not run["partial"] and run["failed"] == 0
            assert run["completed"] == run["planned"] == len(expected_inputs)
            assert run["failures"]["count"] == 0
            binary_hash = build["variants"][variant]["binary_sha256"]
            assert operation["binary_sha256"] == run["tool"]["binary_sha256"] == binary_hash
            rows = [json.loads(line) for line in (folder / f"{name}-files.jsonl").read_text().splitlines()]
            keyed = {(row["path"], row["benchmark"]): row for row in rows}
            assert len(keyed) == len(rows)
            # Parsing is timed during setup even when relationship validation
            # (the explicit cold-parse selector) is omitted on large inputs.
            benchmarks = set(run["benchmarks"]) | {run.get("parse_benchmark", "cold-parse")}
            assert keyed.keys() == {(path, bench) for path in expected_inputs for bench in benchmarks}
            for row in rows:
                assert row["repeats"] == manifest["repeat"] and row["failures"] == 0
                assert row["source_sha256"] == expected_inputs[row["path"]]["sha256"]
                assert row["ignored_differences"] == 0 or row["benchmark"].startswith("seek-")
            key = (sample, operation["mutated"], variant)
            assert key not in runs
            runs[key] = keyed
            metadata[key] = run

    records, summaries, caches = [], [], []
    for sample in ["bounded", "large"]:
        sizes = {entry["path"]: entry["bytes"] for entry in inputs[sample]["inputs"]}
        for mutated in [False, True]:
            for mode in ["points", "bytes"]:
                before_key = (sample, mutated, f"{args.previous}-{mode}")
                after_key = (sample, mutated, f"{args.current}-{mode}")
                before, after = runs[before_key], runs[after_key]
                assert before.keys() == after.keys()
                for field in ["registry", "grammar_sha256", "benchmarks", "machine", "point_positions"]:
                    assert metadata[before_key][field] == metadata[after_key][field], field
                compared = []
                for key, new in sorted(after.items()):
                    old = before[key]
                    identity = ["path", "grammar", "benchmark", "source_sha256", "tested_sha256",
                                "source_bytes", "nodes", "groups", "group_capacity",
                                "ignored_differences", "expected_field_differences"]
                    assert all(old[field] == new[field] for field in identity), (after_key, key)
                    assert new["slab_bytes"] - old["slab_bytes"] == args.slab_delta
                    row = {field: new[field] for field in identity}
                    row.update(sample=sample, mutated=mutated, mode=mode,
                               original_bytes=sizes[new["path"]], slab_bytes=new["slab_bytes"])
                    for label, source in [("previous", old), ("current", new)]:
                        row[label] = {
                            **source["squat"], "mainline_wall_ms": source["mainline"]["wall_ms"],
                            "paired_mainline_wall_ratio": source["ratios"]["wall_ms"],
                        }
                    row["wall_ratio"] = new["squat"]["wall_ms"] / old["squat"]["wall_ms"]
                    row["cpu_ratio"] = new["squat"]["cpu_ms"] / old["squat"]["cpu_ms"]
                    compared.append(row)
                records.extend(compared)
                subsets = {"all": compared}
                if sample == "large":
                    subsets["at_least_1_mib"] = [r for r in compared if r["original_bytes"] >= 1024 * 1024]
                for subset, selected in subsets.items():
                    for benchmark in sorted({r["benchmark"] for r in selected}):
                        group = [r for r in selected if r["benchmark"] == benchmark]
                        summaries.append(dict(sample=sample, mutated=mutated, mode=mode,
                                              subset=subset, benchmark=benchmark,
                                              **revisions.summarize(group)))
                    for version, results in [(args.previous, before), (args.current, after)]:
                        ratios, controls = [], []
                        for row in selected:
                            if row["benchmark"] != "walk-iterator":
                                continue
                            plain = results[(row["path"], "walk-iterator")]
                            cached = results[(row["path"], "walk-iterator-cached")]
                            ratios.append(cached["squat"]["wall_ms"] / plain["squat"]["wall_ms"])
                            controls.append(cached["mainline"]["wall_ms"] / plain["mainline"]["wall_ms"])
                        caches.append(dict(sample=sample, mutated=mutated, mode=mode,
                                           subset=subset, version=version, files=len(ratios),
                                           cached_over_plain=revisions.quantiles(ratios),
                                           mainline_control=revisions.quantiles(controls)))
    result = dict(schema=1, build=build, manifests=manifests,
                  runs={"/".join(map(str, key)): value for key, value in metadata.items()},
                  summaries=summaries, cache_comparisons=caches, records=records,
                  ratio_contract="current/previous per-file timing medians, then equal-file quantiles",
                  large_subset_contract="at least 1 MiB of original input, also for mutations")
    with args.output.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")
    for row in summaries:
        if row["sample"] == "large" and row["subset"] == "all":
            continue
        print(row["sample"], row["mode"], "mutated" if row["mutated"] else "original",
              row["benchmark"], f'{row["current_over_previous_wall"]["median"]:.3f}',
              "control", f'{row["mainline_current_over_previous_wall"]["median"]:.3f}')


if __name__ == "__main__":
    main()
