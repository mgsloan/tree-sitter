#!/usr/bin/env python3
"""Record query experiment provenance, coverage, and paired timing summaries."""
import argparse
import json
from pathlib import Path
import statistics


def rows(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source = json.loads((args.run / "container-run.json").read_text())
    experiments = json.loads((args.run / "query-variants.json").read_text())
    operations = experiments["operations"]
    if len(operations) != experiments.get("planned", 10) or any(record["status"] != "passed" for record in operations):
        raise SystemExit("query variants are incomplete or have failures")
    summaries, files = {}, {}
    query_sources = None
    machine = None
    grammars = None
    for record in operations:
        name = record["name"]
        prefix = args.run / "bench-outputs" / name
        run = json.loads(prefix.with_name(name + "-run.json").read_text())
        if run["partial"] or run["failures"]["count"] or run["completed"] != len(source["inputs"]):
            raise SystemExit(f"incomplete query run: {name}")
        if run["tool"]["binary_sha256"] != record["binary_sha256"] or run["tool"]["source_sha256"] != source["source_sha256"]:
            raise SystemExit(f"tool identity differs: {name}")
        current_queries = {grammar: [{key: value for key, value in query.items() if key != "compile_ms"}
                                    for query in queries] for grammar, queries in run["queries"].items()}
        if query_sources is not None and current_queries != query_sources:
            raise SystemExit(f"query identity differs: {name}")
        query_sources, machine = current_queries, run["machine"]
        grammars = {name: {key: grammar[key] for key in ["symbol", "sha", "library_sha256"]}
                    for name, grammar in run["registry"]["grammars"].items()}
        per_file = rows(prefix.with_name(name + "-files.jsonl"))
        files[name] = {(row["path"], row["benchmark"]): row for row in per_file}
        # The prerequisite parse row carries representation measurements.
        trees = [row for row in per_file if row["benchmark"] == run.get("parse_benchmark", "cold-parse")]
        summaries[name] = dict(group_size=record["group_size"], variant=record["variant"],
                              mutated=record["mutated"], cflags=record["cflags"],
                              binary_sha256=record["binary_sha256"], files=run["completed"],
                              nodes=sum(row["nodes"] for row in trees),
                              slab_bytes=sum(row["slab_bytes"] for row in trees),
                              groups=sum(row["groups"] for row in trees),
                              group_capacity=sum(row["group_capacity"] for row in trees),
                              aggregate=[dict(benchmark=row["benchmark"], files=row["files"],
                                              percentiles=row["percentiles"],
                                              wall_ms=row["statistics"]["wall_ms"],
                                              cpu_ms=row["statistics"]["cpu_ms"])
                                         for row in rows(prefix.with_name(name + "-aggregate.jsonl"))],
                              language_wall_ratios=[dict(language=row["language"], benchmark=row["benchmark"],
                                                         files=row["files"],
                                                         percentiles=row["statistics"]["wall_ms"]["paired_ratios"])
                                                    for row in rows(prefix.with_name(name + "-languages.jsonl"))])
    ablations = {}
    for name, variant in summaries.items():
        suffix = "mutated" if variant["mutated"] else "original"
        baseline = files.get(f"query-16-optimized-{suffix}")
        if baseline is None:
            continue
        metrics = {}
        for benchmark in ["query-matches", "query-captures"]:
            ratios = []
            for key, row in files[name].items():
                if key[1] != benchmark:
                    continue
                other = baseline[key]
                if row["tested_sha256"] != other["tested_sha256"]:
                    raise SystemExit(f"input bytes differ: {name}: {key[0]}")
                ratios.append(row["squat"]["wall_ms"] / other["squat"]["wall_ms"])
            metrics[benchmark] = dict(files=len(ratios), median=statistics.median(ratios),
                                      minimum=min(ratios), maximum=max(ratios))
        ablations[name] = metrics
    result = dict(schema=1, source_sha256=source["source_sha256"], tool_sha=source["tool_sha"],
                  code_corpora_sha=source["code_corpora_sha"], image=source["image"],
                  repeat=experiments["repeat"], machine=machine, inputs=source["inputs"],
                  grammars=grammars, queries=query_sources, variants=summaries,
                  relative_to_16_optimized=ablations,
                  caveats=["Bounded training/holdout sample, at most 100 KiB per input.",
                           "Timings include identical result snapshot collection and host text predicates.",
                           "Per-file backend ratios pair repeats; ablations compare separate run medians.",
                           "No performance counters were available in this container.",
                           "Group-size variants use experimental format flags; default remains 16.",
                           "Earlier large generated TS/JS cases exceeded timeout/snapshot budgets and are excluded."])
    with args.output.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")
    print(json.dumps(ablations, indent=2))


if __name__ == "__main__":
    main()
