#!/usr/bin/env python3
"""Check CLI paths, seek policy, partial outputs, and deterministic sampling."""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path, help="completed run.py output with CSS grammar")
    args = parser.parse_args()
    run = args.run.resolve()
    metadata = json.loads((run / "container-run.json").read_text())
    common = ["podman", "run", "--rm", "--network=none", "--cap-drop=all",
              "--security-opt=no-new-privileges", "--memory=4g", "--pids-limit=256",
              "--entrypoint", "/lib64/ld-linux-x86-64.so.2",
              "-v", f"{ROOT}:/work:ro", "-v", f"{run}:/out:rw", metadata["image"]]
    benchmark = ["/out/target/release/squatter-bench", "--code-corpora", "/out/corpus",
                 "--registry", "/out/registry.json", "--output-directory", "/out/controls",
                 "--repeat", "3", "seek-byte", "/work/lib/squat/tests/fixtures/hidden-seek.css"]
    subprocess.run(common + benchmark + ["--output", "ignored"], check=True, timeout=60)
    ignored = json.loads((run / "controls/ignored-run.json").read_text())
    assert ignored["completed"] == 1 and ignored["failed"] == 0
    assert ignored["ignored_seek_differences"] > 0
    strict = subprocess.run(common + benchmark + ["--output", "strict", "--strict-seeks",
                            "--short-circuit"], timeout=60)
    manifest = json.loads((run / "controls/strict-run.json").read_text())
    assert strict.returncode != 0 and manifest["partial"]
    assert manifest["failed"] == 1 and manifest["completed"] == 0
    summaries = [json.loads(line) for line in
                 (run / "controls/strict-aggregate.jsonl").read_text().splitlines()]
    assert summaries and all(row["partial"] for row in summaries)
    sample = json.loads((run / "samplings/run.json").read_text())
    subprocess.run(common + ["/out/target/release/corpus-analysis", "sample",
        "--code-corpora", "/out/corpus", "--registry", "/out/registry.json",
        "--output", "/out/control-samplings", "--seed", str(sample["seed"]),
        "--per-bucket", str(sample["per_bucket"])], check=True, timeout=120)
    for name in sample["counts"]:
        assert (run / "samplings" / name).read_bytes() == (run / "control-samplings" / name).read_bytes()
    print("ok: absolute input paths, ignored/strict seeks, partial output, identical sample lists")


if __name__ == "__main__":
    main()
