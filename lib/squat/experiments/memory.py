#!/usr/bin/env python3
"""Measure actual tree allocations on the saved, hashed iterator corpora.

Build memory-bench in build/squat-memory/{points,bytes} first. This runner can
also run inside the corpus container with --loader /lib64/ld-linux-x86-64.so.2.
"""

import argparse
import hashlib
import json
import platform
import subprocess
from pathlib import Path


def digest(data):
    return hashlib.sha256(data).hexdigest()


def mutate(source, path):
    """Match corpus_analysis::mutate(seed=42); validate against saved Rust hashes."""
    seed = (42).to_bytes(8, "little")
    for part in [b"mutations", path.encode()]:
        seed += len(part).to_bytes(8, "little") + part
    state = int.from_bytes(hashlib.sha256(seed).digest()[:8], "little")
    mask = (1 << 64) - 1

    def index(length):
        nonlocal state
        if not length:
            return 0
        state = (state + 0x9E3779B97F4A7C15) & mask
        value = ((state ^ (state >> 30)) * 0xBF58476D1CE4E5B9) & mask
        value = ((value ^ (value >> 27)) * 0x94D049BB133111EB) & mask
        return (value ^ (value >> 31)) % length

    result = bytearray(source)
    for _ in range(3):
        start = index(len(result) + 1)
        length = index(min(len(result) - start, 64) + 1)
        action = index(3)
        if action == 0:
            del result[start : start + length]
        elif action == 1:
            tokens = [b"}", b"\n", b'"', b"/*", b"\xff", b"()", b"<>"]
            result[start:start] = tokens[index(len(tokens))]
        else:
            moved = result[start : start + length]
            del result[start : start + length]
            destination = index(len(result) + 1)
            result[destination:destination] = moved
    return bytes(result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[3])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--loader", type=Path)
    parser.add_argument("--repeats", type=int, default=2)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error("--repeats must be positive")
    root = args.root.resolve()
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    inputs_directory = output.parent / "inputs"
    inputs_directory.mkdir(exist_ok=True)
    manifest_path = root / "build/squat-iterator/absolute-build-manifest.json"
    manifest = json.loads(manifest_path.read_text())
    binaries = {mode: root / f"build/squat-memory/{mode}/memory-bench"
                for mode in ["points", "bytes"]}
    metadata = {
        "platform": platform.platform(),
        "libc": platform.libc_ver(),
        "repeats": args.repeats,
        "seed": 42,
        "input_manifest_sha256": digest(manifest_path.read_bytes()),
        "binaries": {mode: digest(path.read_bytes()) for mode, path in binaries.items()},
        "sources": {str(path.relative_to(root)): digest(path.read_bytes())
                    for path in sorted((root / "lib/squat").rglob("*.c"))
                    if "tests" not in path.parts},
        "grammars": {},
    }
    # The previous Rust validation is an independent oracle for the Python
    # mutation port. Its presence is optional for reproductions of this runner.
    validation_path = root / "build/squat-optional-points/mutated-files.jsonl"
    mutation_hashes = {}
    if validation_path.exists():
        for line in validation_path.read_text().splitlines():
            row = json.loads(line)
            mutation_hashes[row["path"]] = row["tested_sha256"]
    metadata["rust_mutation_hash_checks"] = 0
    rows = []
    # Flush each completed result so a failed run still leaves inspectable data.
    with output.with_suffix(".jsonl").open("w") as raw:
        for sample in ["bounded", "large"]:
            corpus_root = root / ("build/squat-cursors" if sample == "bounded"
                                  else "build/squat-cursors-large")
            registry = json.loads((corpus_root / "registry.json").read_text())
            for input_index, item in enumerate(manifest["samples"][sample]["inputs"]):
                relative = item["path"]
                path = Path(relative)
                grammar = item.get("grammar") or registry["suffixes"].get(path.name)
                grammar = grammar or registry["suffixes"][path.suffix[1:]]
                entry = registry["grammars"][grammar]
                library = corpus_root / "grammars" / Path(entry["library"]).name
                grammar_hash = digest(library.read_bytes())
                assert grammar_hash == entry["library_sha256"], library
                metadata["grammars"][f"{sample}/{grammar}"] = {
                    "library_sha256": grammar_hash, "grammar_commit": entry["sha"],
                    "symbol": entry["symbol"],
                }
                original = (corpus_root / "corpus" / relative).read_bytes()
                assert digest(original) == item["sha256"], relative
                for mutation in [False, True]:
                    source = mutate(original, relative) if mutation else original
                    source_hash = digest(source)
                    if mutation and relative in mutation_hashes:
                        assert source_hash == mutation_hashes[relative], relative
                        metadata["rust_mutation_hash_checks"] += 1
                    source_path = inputs_directory / source_hash
                    source_path.write_bytes(source)
                    measurements = {}
                    for mode, binary in binaries.items():
                        command = ([str(args.loader)] if args.loader else []) + [
                            str(binary), str(library), entry["symbol"], str(source_path)]
                        repeats = [json.loads(subprocess.check_output(command, text=True))
                                   for _ in range(args.repeats)]
                        assert all(row == repeats[0] for row in repeats), (relative, mode)
                        measurements[mode] = repeats[0]
                    points, byte_only = measurements["points"], measurements["bytes"]
                    assert points["points"] == 1 and byte_only["points"] == 0
                    for key in ["mainline", "parse_peak", "nodes", "source_bytes"]:
                        assert points[key] == byte_only[key], (relative, key)
                    row = {
                        "sample": sample, "path": relative, "grammar": grammar,
                        "mutation": mutation, "original_bytes": len(original),
                        "original_sha256": item["sha256"], "tested_sha256": source_hash,
                        "measurements": measurements,
                    }
                    rows.append(row)
                    raw.write(json.dumps(row, separators=(",", ":")) + "\n")
                    raw.flush()
                print(f"{sample}: {input_index + 1}/{len(manifest['samples'][sample]['inputs'])}",
                      flush=True)
    output.write_text(json.dumps({"metadata": metadata, "rows": rows}, indent=2) + "\n")
    print(f"Saved {len(rows)} input cases; {len(rows) * 2 * args.repeats} probe runs to {output}")


if __name__ == "__main__":
    main()
