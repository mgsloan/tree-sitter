#!/usr/bin/env python3
"""Build and test native grammars inside code-corpora's offline build image."""
import argparse
import hashlib
import json
import pathlib
import subprocess
import time
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[3]


def run(command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--code-corpora", type=pathlib.Path, default=ROOT / "../../code-corpora")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--grammar", action="append", help="selected-grammars.toml entry; repeatable")
    parser.add_argument("--image", help="defaults to the corpus lock's build image ID")
    parser.add_argument("--sanitize", action="store_true")
    parser.add_argument("--strict-seeks", action="store_true", help="treat known seek differences as failures")
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()
    corpus = args.code_corpora.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    lock = tomllib.loads((corpus / "containers/images.lock.toml").read_text())
    image = args.image or lock["build"]["local_image_id"]
    catalog = tomllib.loads((corpus / "selected-grammars.toml").read_text())
    entries = {entry["name"]: entry for entry in catalog["repo"]}
    grammars = args.grammar or ["json", "python", "c", "cpp", "tsx", "html", "css", "yaml", "go", "bash"]
    provenance = {
        "code_corpora_sha": run(["git", "-C", str(corpus), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip(),
        "tool_sha": run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip(),
        "tool_dirty": bool(run(["git", "-C", str(ROOT), "status", "--porcelain"], capture_output=True, text=True).stdout),
        "image": image,
        "sanitize": args.sanitize,
        "strict_seeks": args.strict_seeks,
        "grammars": [],
    }
    common = ["podman", "run", "--rm", "--network=none", "--cap-drop=all",
              "--security-opt=no-new-privileges", "--memory=4g", "--cpus=4", "--pids-limit=256",
              "--entrypoint", "sh", "-v", f"{ROOT}:/work:ro", "-v", f"{output}:/out:rw"]
    if args.strict_seeks:
        common += ["-e", "SQ_STRICT_SEEKS=1"]
    flags = "-O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer" if args.sanitize else "-O2 -g"
    run(common + [image, "-c", f"make -C /work/lib/squat -j4 BUILD=/out CFLAGS='{flags}' all check"], timeout=args.timeout)
    failures = 0
    for name in grammars:
        started = time.monotonic()
        entry = entries[name]
        checkout = corpus / "grammars" / name
        directory = entry.get("directory", "")
        source = checkout / directory / "src/parser.c"
        record = {"name": name, "pin": entry["sha"], "directory": directory}
        if not source.is_file():
            record.update(status="unavailable", reason=f"missing {source}")
            failures += 1
        else:
            record["parser_sha256"] = hashlib.sha256(source.read_bytes()).hexdigest()
            grammar_path = "/grammar/" + directory
            source_path = grammar_path.rstrip("/") + "/src"
            symbol = "tree_sitter_" + entry.get("grammar", name).replace("-", "_")
            # Shell arguments are positional, so grammar metadata cannot become shell code.
            command = r'''
set -eu
source=$1
symbol=$2
name=$3
flags=$4
set -- "$source/parser.c"
if test -f "$source/scanner.c"; then set -- "$@" "$source/scanner.c"; fi
cc -shared -fPIC $flags -I"$source" "$@" -o "/out/$name.so"
set --
for sample in /grammar/setup.py /grammar/grammar.js /grammar/examples/*; do
  if test -f "$sample"; then set -- "$@" "$sample"; fi
done
ASAN_OPTIONS=detect_leaks=1 /out/compare "/out/$name.so" "$symbol" "$@"
'''
            try:
                with (output / f"{name}.log").open("w") as log:
                    run(common + ["-v", f"{checkout}:/grammar:ro", image, "-c", command,
                                  "sh", source_path, symbol, name, flags],
                        stdout=log, stderr=subprocess.STDOUT, timeout=args.timeout)
                record["status"] = "passed"
                log_text = (output / f"{name}.log").read_text()
                import re
                mismatches = re.search(r"seek mismatches: (\d+)", log_text)
                record["seek_mismatches"] = int(mismatches[1]) if mismatches else 0
                if record["seek_mismatches"]:
                    record["status"] = "structural-pass-seek-differences"
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as failure:
                record.update(status="failed", reason=str(failure))
                failures += 1
        record["seconds"] = time.monotonic() - started
        provenance["grammars"].append(record)
        (output / "run.json").write_text(json.dumps(provenance, indent=2) + "\n")
        print(f"{name}: {record['status']} ({record['seconds']:.1f}s)", flush=True)
    raise SystemExit(bool(failures))


if __name__ == "__main__":
    main()
