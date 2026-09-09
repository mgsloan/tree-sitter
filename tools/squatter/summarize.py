#!/usr/bin/env python3
"""Summarize completed layout logs without hiding per-file or provenance data."""
import argparse
import csv
import io
import json
from pathlib import Path
import statistics


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    manifest = json.loads((args.run / "container-run.json").read_text())
    failures = [operation for operation in manifest["operations"]
                if operation["status"] != "passed"]
    if failures:
        raise SystemExit(f"run contains failed operations: {failures}")
    variants = {}
    records = []
    for group, alignment in [(16, 8), (32, 8), (64, 8), (16, 64)]:
        variant = f"{group}-{alignment}"
        rows = []
        for grammar in sorted({entry["grammar"] for entry in manifest["inputs"]}):
            inputs = [entry for entry in manifest["inputs"] if entry["grammar"] == grammar]
            path = args.run / f"layout-{variant}-{grammar}.log"
            grammar_rows = list(csv.DictReader(io.StringIO(path.read_text())))
            if len(grammar_rows) != len(inputs):
                raise SystemExit(f"incomplete layout log: {path}")
            for source, raw in zip(inputs, grammar_rows):
                row = {key: float(value) if key == "median_pack_ms" else int(value)
                       for key, value in raw.items()}
                row.update(path=source["path"], grammar=grammar)
                rows.append(row)
        nodes = sum(row["nodes"] for row in rows)
        size = sum(row["slab_bytes"] for row in rows)
        variants[variant] = dict(files=len(rows), nodes=nodes, slab_bytes=size,
            bytes_per_node=size / nodes,
            median_file_bytes_per_node=statistics.median(row["slab_bytes"] / row["nodes"] for row in rows),
            occupancy=nodes / sum(row["slots"] for row in rows),
            summed_median_pack_ms=sum(row["median_pack_ms"] for row in rows))
        if variant == "16-8":
            variants[variant]["modeled_column_bytes"] = {
                name: sum(row[name] for row in rows) for name in [
                    "grammar_bytes", "sparse_grammar_bytes", "super_bytes", "var_super_bytes",
                    "separate_symbol_field_bytes", "interleaved_symbol_field_bytes"]}
            # Historical version-2 logs include the now-removed exception section.
            if "field_exception_bytes" in rows[0]:
                variants[variant]["field_exception_bytes"] = sum(row["field_exception_bytes"] for row in rows)
        records.extend(rows)
    scan = (args.run / "scan-kernels.log").read_text().splitlines()
    # make output precedes CSV. The header is emitted by the executable itself.
    start = next(index for index, line in enumerate(scan) if line.startswith("width,"))
    kernels = list(csv.DictReader(scan[start:]))
    result = dict(schema=1, source_sha256=manifest["source_sha256"],
        tool_sha=manifest["tool_sha"], image=manifest["image"],
        code_corpora_sha=manifest["code_corpora_sha"], grammars=manifest.get("grammars"),
        inputs=manifest["inputs"], variants=variants, kernels=kernels, records=records,
        caveats=["Convenience sample is bounded and not language-balanced by node count.",
                 "Packing times are seven-repeat medians; scheduling and thermals add noise.",
                 "Sparse/VarBits/interleaved columns are byte models, not measured access paths."])
    with args.output.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")
    print(json.dumps(variants, indent=2))


if __name__ == "__main__":
    main()
