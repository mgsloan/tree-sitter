#!/usr/bin/env python3
"""Time compact packing with uploaded layout probes and existing corpus bytes."""

import argparse
import csv
import hashlib
import io
import json
from pathlib import Path
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    bundle = args.bundle.resolve()
    manifest = json.loads(args.manifest.read_text())
    args.output.mkdir()
    rows = []
    for index, entry in enumerate(manifest["inputs"]):
        source = bundle / entry["sample"] / "corpus" / entry["path"]
        assert digest(source) == entry["source_sha256"]
        grammar = bundle.parent / "shared" / "grammars" / (entry["grammar"] + ".so")
        variants = list(manifest["variants"])
        if index % 2:
            variants.reverse()
        for variant in variants:
            binary = bundle / "packing" / variant
            assert digest(binary) == manifest["variants"][variant]["binary_sha256"]
            command = ["taskset", "-c", "0", "/lib64/ld-linux-x86-64.so.2", str(binary),
                       str(grammar), "tree_sitter_" + entry["grammar"], str(source)]
            result = subprocess.run(command, text=True, capture_output=True, check=True)
            values = list(csv.DictReader(io.StringIO(result.stdout)))
            assert len(values) == 1
            values = {key: float(value) if key == "median_pack_ms" else int(value)
                      for key, value in values[0].items()}
            row = dict(**entry, variant=variant, grammar_sha256=digest(grammar),
                       command=command, values=values)
            rows.append(row)
            (args.output / "results.json").write_text(json.dumps(
                dict(manifest=manifest, completed=len(rows), planned=len(manifest["inputs"]) *
                     len(manifest["variants"]), rows=rows), indent=2) + "\n")
        print(f"{index + 1}/{len(manifest['inputs'])}: {entry['path']}", flush=True)


if __name__ == "__main__":
    main()
