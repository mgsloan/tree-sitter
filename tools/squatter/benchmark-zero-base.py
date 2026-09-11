#!/usr/bin/env python3
"""Measure format-preserving base choices on the saved representative corpus."""
import argparse
import csv
import datetime
import hashlib
import json
import platform
import statistics
import subprocess
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cpu", type=int, default=0)
    parser.add_argument("--repeats", type=int, default=16)
    parser.add_argument("--reverse", action="store_true")
    parser.add_argument("--kind", choices=["zero", "end255"], default="zero")
    args = parser.parse_args()
    if args.kind == "end255":
        columns = ["end_column"]
        kernels = ["scalar-node", "scalar-group", "sse2-four", "sse2-sixteen", "avx2-eight", "avx2-sixteen"]
        variants = ["baseline", "existing-complement", "fixed-subtract", "fixed-complement"]
        existing = "already_255"
    else:
        columns = ["span", "start_column", "end_column"]
        kernels = ["scalar-node", "scalar-group", "sse2", "avx2"]
        variants = ["baseline", "existing-skip", "zero-add", "zero-skip"]
        existing = "already_zero"
    assert args.repeats > 0 and args.repeats % 8 == 0
    args.output.mkdir(parents=True, exist_ok=False)
    inputs = json.loads(args.inputs.read_text())["inputs"]
    if args.reverse:
        inputs.reverse()
    result = {
        "started": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "host": platform.uname()._asdict(),
        "cpu": json.loads(subprocess.check_output(["lscpu", "--json"], text=True)),
        "affinity": args.cpu,
        "binary_sha256": sha(args.binary),
        "inputs_sha256": sha(args.inputs),
        "runner_sha256": sha(Path(__file__)),
        "repeats": args.repeats,
        "reverse": args.reverse,
        "kind": args.kind,
        "files": [],
    }
    for index, item in enumerate(inputs):
        registry = json.loads((args.bundle / item["sample"] / "registry.json").read_text())
        grammar = registry["grammars"][item["grammar"]]
        source = args.bundle / item["sample"] / "corpus" / item["path"]
        assert sha(source) == item["source_sha256"]
        library = Path(grammar["library"])
        assert sha(library) == grammar["library_sha256"]
        command = ["taskset", "-c", str(args.cpu), "/lib64/ld-linux-x86-64.so.2",
                   str(args.binary.resolve()), str(library), grammar["symbol"],
                   str(source), str(args.repeats)]
        raw = args.output / f"{index:02d}-{item['grammar']}.csv"
        with raw.open("w") as stdout, raw.with_suffix(".log").open("w") as stderr:
            subprocess.run(command, stdout=stdout, stderr=stderr, check=True)
        rows = list(csv.DictReader(raw.open()))
        groups = {}
        for row in rows:
            key = (row["column"], row["kernel"], row["variant"])
            groups.setdefault(key, []).append(row)
        assert len(groups) == len(columns) * len(kernels) * len(variants)
        medians = {}
        stats = {}
        for (column, kernel, variant), values in groups.items():
            assert len(values) == args.repeats
            assert {int(v["repeat"]) for v in values} == set(range(args.repeats))
            assert len({v["checksum"] for v in values}) == 1
            medians[(column, kernel, variant)] = statistics.median(
                float(v["seconds"]) / int(v["batches"]) for v in values)
            stats[column] = {key: int(values[0][key]) for key in
                             ["groups", "nodes", "eligible", existing]}
        measurements = []
        for column in columns:
            for kernel in kernels:
                baseline = medians[(column, kernel, "baseline")]
                # All variants must decode the same live values and checksum.
                assert len({groups[(column, kernel, v)][0]["checksum"] for v in variants}) == 1
                for variant in variants:
                    value = medians[(column, kernel, variant)]
                    measurements.append({"column": column, "kernel": kernel,
                                         "variant": variant, "ratio": value / baseline,
                                         "ns_per_live_value": value * 1e9 / stats[column]["nodes"]})
        result["files"].append({**item, "grammar_sha256": sha(library), "command": command,
                                "raw": raw.name, "raw_sha256": sha(raw), "stats": stats,
                                "measurements": measurements})
        (args.output / "results.json").write_text(json.dumps(result, indent=2) + "\n")
        print(f"{index + 1}/{len(inputs)} {item['sample']} {item['grammar']}: passed", flush=True)
    summaries = []
    for sample in ["bounded", "large"]:
        files = [f for f in result["files"] if f["sample"] == sample]
        for column in columns:
            for kernel in kernels:
                for variant in variants[1:]:
                    ratios = [m["ratio"] for f in files for m in f["measurements"]
                              if (m["column"], m["kernel"], m["variant"]) == (column, kernel, variant)]
                    summaries.append({"sample": sample, "column": column, "kernel": kernel,
                                      "variant": variant, "median_ratio": statistics.median(ratios),
                                      "min_ratio": min(ratios), "max_ratio": max(ratios)})
    result["summary"] = summaries
    result["finished"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    (args.output / "results.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
