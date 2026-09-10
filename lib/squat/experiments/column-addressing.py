#!/usr/bin/env python3
"""Run from the repository root after building both column-addressing binaries.

Uses CPU 2 and the saved iterator corpus. Raw results go to
build/squat-addressing/results.json; each completed file is flushed.
"""

import csv
import hashlib
import io
import json
import subprocess
from pathlib import Path


def main():
    root = Path.cwd()
    output = root / "build/squat-addressing/results.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((root / "build/squat-iterator/absolute-build-manifest.json").read_text())
    bounded = manifest["samples"]["bounded"]["inputs"]
    selected = []
    for grammar in sorted({row["grammar"] for row in bounded}):
        largest = max((row for row in bounded if row["grammar"] == grammar),
                      key=lambda row: row["bytes"])
        selected.append(("bounded", largest))
    selected.extend(("large", row) for row in manifest["samples"]["large"]["inputs"]
                    if row["bytes"] >= 1048576)

    results = []
    for sample, row in selected:
        corpus = root / ("build/squat-cursors" if sample == "bounded"
                         else "build/squat-cursors-large")
        registry = json.loads((corpus / "registry.json").read_text())
        grammar = row.get("grammar") or registry["suffixes"][Path(row["path"]).suffix[1:]]
        entry = registry["grammars"][grammar]
        source = corpus / "corpus" / row["path"]
        assert hashlib.sha256(source.read_bytes()).hexdigest() == row["sha256"]
        for points in [True, False]:
            binary = root / "build/squat-memory" / ("points" if points else "bytes") / "column-addressing"
            command = ["taskset", "-c", "2", str(binary),
                       str(corpus / "grammars" / Path(entry["library"]).name),
                       entry["symbol"], str(source)]
            run = subprocess.run(command, check=True, capture_output=True, text=True)
            measurements = list(csv.DictReader(io.StringIO(run.stdout)))
            results.append({
                "sample": sample, "path": row["path"], "source_sha256": row["sha256"],
                "grammar": grammar, "points": points, "command": command,
                "sizes": run.stderr.strip(), "rows": measurements,
            })
            print(sample, grammar, points, row["path"], flush=True)
            output.write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
