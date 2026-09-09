#!/usr/bin/env python3
"""Analyze staged code-corpora sources using the grammars container catalog."""
import argparse
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import subprocess
import time


def command(arguments, **options):
    return subprocess.check_output(list(map(str, arguments)), **options).decode().strip()


def sha256(path):
    with open(path, "rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def read_exclusions(arguments):
    entries = json.loads(arguments.exclusions.read_text())
    paths = {}
    for entry in entries:
        path = Path(entry["path"])
        if path.is_absolute() or ".." in path.parts or not entry["reason"]:
            raise ValueError("exclusions require corpus-relative paths and nonempty reasons")
        absolute = str(arguments.corpus / path)
        if absolute in paths:
            raise ValueError("duplicate exclusion: " + entry["path"])
        paths[absolute] = entry["reason"]
    return paths


def prepare(arguments):
    """Use the selected, precompiled parsers in the grammars image."""
    metadata = json.loads((arguments.corpus / "metadata.json").read_text())
    catalog_path = arguments.grammar_root.parent / "grammar-catalog.json"
    catalog = json.loads(catalog_path.read_text())
    selected = {row["name"]: row for row in metadata["selected_grammars"]}
    grammars = {}
    for row in catalog:
        name = row["name"]
        if name not in selected:
            continue
        entry = dict(row, status="unavailable")
        if row["status"] == "built":
            root = arguments.grammar_root / name
            artifact = json.loads((root / "artifact.json").read_text())
            if artifact["sha"] != selected[name]["sha"]:
                raise ValueError("grammar image pin differs from code-corpora: " + name)
            library = root / "parser.so"
            if sha256(library) != artifact["sha256"]:
                raise ValueError("grammar library checksum mismatch: " + name)
            entry.update(status="ready", library=str(library), symbol=artifact["symbol"],
                         library_sha256=artifact["sha256"])
        grammars[name] = entry
    for name, row in selected.items():
        grammars.setdefault(name, dict(row, status="unavailable"))
    classification = metadata["classification"]
    write_json(arguments.directory / "grammars.json", dict(
        code_corpora_sha=metadata["code_corpora_sha"], grammars=grammars,
        suffixes=classification["suffixes"], first_lines=classification["first_lines"],
        catalog_sha256=sha256(catalog_path)))
    write_json(arguments.directory / "provenance.json", dict(metadata,
        catalog_sha256=sha256(catalog_path), binary_sha256=sha256(arguments.binary)))


def inventory(arguments):
    exclusions = read_exclusions(arguments)
    registry = json.loads((arguments.directory / "grammars.json").read_text())
    suffixes = registry["suffixes"]
    first_lines = [(re.compile(pattern), grammar) for pattern, grammar in registry["first_lines"]]
    metadata = json.loads((arguments.corpus / "metadata.json").read_text())
    tracked = {str(arguments.corpus / path) for path in metadata["tracked_files"]}
    database = arguments.directory / "inventory.sqlite"
    if database.exists():
        raise RuntimeError("inventory already exists")
    connection = sqlite3.connect(database)
    connection.execute("CREATE TABLE files(path TEXT PRIMARY KEY, grammar TEXT, sha256 TEXT, size INTEGER, scope TEXT, status TEXT)")
    counts = Counter()
    unknown = Counter()
    started = time.monotonic()
    def visit(root):
        try:
            entries = sorted(os.scandir(root), key=lambda entry: entry.name)
        except OSError as error:
            connection.execute("INSERT INTO files VALUES(?,?,?,?,?,?)", (str(root),None,None,0,"unknown",str(error)))
            return
        for entry in entries:
            if entry.name == ".git":
                continue
            if entry.is_symlink():
                connection.execute("INSERT INTO files VALUES(?,?,?,?,?,?)", (entry.path,None,None,0,"unknown","symlink"))
            elif entry.is_dir(follow_symlinks=False):
                visit(entry.path)
            elif entry.is_file(follow_symlinks=False):
                path = Path(entry.path)
                parts = path.relative_to(arguments.corpus).parts
                split = parts[0] if parts[0] in ("training", "test") else "support"
                origin = "repository" if entry.path in tracked else "dependency_or_generated"
                scope = split + "/" + origin
                suffix = path.suffix[1:]
                grammar = suffixes.get(entry.name, suffixes.get(suffix))
                size = entry.stat().st_size
                status = "candidate" if grammar else "unclassified"
                digest = None
                try:
                    if grammar is None and not suffix:
                        with open(path, "rb") as source:
                            first = source.readline(512).decode("utf-8", errors="replace")
                        grammar = next((grammar for pattern, grammar in first_lines if pattern.search(first)), None)
                    if grammar:
                        if size > arguments.max_file_bytes:
                            status = "excluded_size"
                        else:
                            digest = sha256(path)
                            status = "candidate" if registry["grammars"][grammar]["status"] == "ready" else "grammar_unavailable"
                    else:
                        unknown[suffix] += 1
                except OSError as error:
                    status = "read_error:" + str(error)
                if entry.path in exclusions:
                    status = "excluded_explicit"
                connection.execute("INSERT INTO files VALUES(?,?,?,?,?,?)", (entry.path,grammar,digest,size,scope,status))
                counts[status] += 1
                if sum(counts.values()) % 10000 == 0:
                    connection.commit()
                    print("inventory", sum(counts.values()), dict(counts), "seconds", round(time.monotonic()-started), flush=True)
    for split in ("training", "test"):
        visit(arguments.corpus / split)
    connection.commit()
    connection.execute("CREATE INDEX file_grammar_hash ON files(grammar,sha256)")
    finish_inventory(arguments, connection, started, unknown)
    connection.close()


def finish_inventory(arguments, connection, started, unknown):
    """Regenerate occurrence weights only after applying the inclusion rule."""
    manifests = {}
    for (grammar,) in connection.execute("SELECT DISTINCT grammar FROM files WHERE status='candidate'"):
        output = arguments.directory / f"{grammar}.sources.jsonl"
        grouped = connection.execute("SELECT sha256, MIN(path), MAX(size) FROM files WHERE grammar=? AND status='candidate' GROUP BY sha256 ORDER BY MAX(size),sha256", (grammar,)).fetchall()
        with output.open("x") as destination:
            for digest, path, size in grouped:
                weights = dict(connection.execute("SELECT scope,COUNT(*) FROM files WHERE grammar=? AND sha256=? AND status='candidate' GROUP BY scope", (grammar,digest)))
                destination.write(json.dumps(dict(path=path,sha256=digest,weights=weights)) + "\n")
        files, size = connection.execute("SELECT COUNT(*),SUM(size) FROM files WHERE grammar=? AND status='candidate'", (grammar,)).fetchone()
        manifests[grammar] = dict(files=files,unique=len(grouped),bytes=size,sources=str(output))
    counts = dict(connection.execute("SELECT status,COUNT(*) FROM files GROUP BY status"))
    excluded_files, excluded_bytes = connection.execute("SELECT COUNT(*),COALESCE(SUM(size),0) FROM files WHERE status='excluded_size'").fetchone()
    with (arguments.directory / "excluded-size.jsonl").open("x") as output:
        for path, grammar, digest, size, scope in connection.execute("SELECT path,grammar,sha256,size,scope FROM files WHERE status='excluded_size' ORDER BY path"):
            output.write(json.dumps(dict(path=path, grammar=grammar, sha256=digest, size=size, scope=scope,
                reason="file_exceeds_max_bytes", max_file_bytes=arguments.max_file_bytes)) + "\n")
    exclusions = read_exclusions(arguments)
    write_json(arguments.directory / "exclusions.json", json.loads(arguments.exclusions.read_text()))
    explicit_files, explicit_bytes = connection.execute("SELECT COUNT(*),COALESCE(SUM(size),0) FROM files WHERE status='excluded_explicit'").fetchone()
    with (arguments.directory / "excluded-explicit.jsonl").open("x") as output:
        for path, grammar, digest, size, scope in connection.execute("SELECT path,grammar,sha256,size,scope FROM files WHERE status='excluded_explicit' ORDER BY path"):
            output.write(json.dumps(dict(path=path, grammar=grammar, sha256=digest, size=size, scope=scope,
                reason=exclusions.get(path, "explicit exclusion"))) + "\n")
    write_json(arguments.directory / "inventory-summary.json", dict(counts=counts, unknown_extensions=unknown.most_common(), grammars=manifests,
        corpus=str(arguments.corpus), seconds=time.monotonic()-started,
        max_file_bytes=arguments.max_file_bytes, excluded_size_files=excluded_files, excluded_size_bytes=excluded_bytes,
        excluded_explicit_files=explicit_files, excluded_explicit_bytes=explicit_bytes,
        exclusions_sha256=sha256(arguments.directory / "exclusions.json"),
        code_corpora_sha=metadata_sha(arguments),
        classification="explicit filename/suffix map; no directory symlinks followed; .git excluded; inclusive file-size cutoff; generated code retained below cutoff; duplicates weighted by occurrence"))


def select_run_inputs(arguments, inventory):
    """Select from immutable original manifests, so reruns never trim twice."""
    skipped = arguments.skip_tsx_tail
    if skipped < 0:
        raise ValueError("--skip-tsx-tail must be nonnegative")
    selected = dict(grammars=dict(inventory["grammars"]), skip_tsx_tail=skipped,
                    excluded_tsx_unique=0, excluded_tsx_files=0, excluded_tsx_bytes=0)
    tsx = inventory["grammars"].get("tsx")
    if tsx is not None:
        source = Path(tsx["sources"])
        lines = source.read_text().splitlines(keepends=True)
        boundary = max(0, len(lines) - skipped)
        excluded = [json.loads(line) for line in lines[boundary:]]
        with closing(sqlite3.connect(f"file:{arguments.directory / 'inventory.sqlite'}?mode=ro", uri=True)) as connection:
            for entry in excluded:
                entry["source_bytes"] = connection.execute("SELECT size FROM files WHERE path=?", (entry["path"],)).fetchone()[0]
                entry.update(status="excluded_tsx_tail", reason="size_ordered_tsx_tail")
        selected.update(excluded_tsx_unique=len(excluded),
            excluded_tsx_files=sum(sum(entry["weights"].values()) for entry in excluded),
            excluded_tsx_bytes=sum(entry["source_bytes"] * sum(entry["weights"].values()) for entry in excluded),
            original_tsx_sources_sha256=sha256(source))
        path = arguments.directory / "tsx.selected.sources.jsonl"
        snapshots = {path: "".join(lines[:boundary]),
            arguments.directory / "excluded-tsx-tail.jsonl": "".join(json.dumps(entry) + "\n" for entry in excluded)}
        for destination, contents in snapshots.items():
            if destination.exists() and destination.read_text() != contents:
                raise ValueError("TSX selection differs from existing run; choose a fresh directory")
        for destination, contents in snapshots.items():
            destination.write_text(contents)
        selected["grammars"]["tsx"] = dict(tsx, sources=str(path), unique=boundary,
            files=tsx["files"] - selected["excluded_tsx_files"],
            bytes=tsx["bytes"] - selected["excluded_tsx_bytes"])
        if not boundary:
            del selected["grammars"]["tsx"]
    path = arguments.directory / "run-inputs.json"
    if path.exists() and json.loads(path.read_text()) != selected:
        raise ValueError("input selection differs from existing run; choose a fresh directory")
    write_json(path, selected)
    return selected


def run(arguments):
    registry = json.loads((arguments.directory / "grammars.json").read_text())
    inventory = json.loads((arguments.directory / "inventory-summary.json").read_text())
    if "max_file_bytes" not in inventory:
        raise ValueError("inventory has no file-size cutoff; create a fresh inventory")
    arguments.corpus = Path(inventory["corpus"])
    with closing(sqlite3.connect(f"file:{arguments.directory / 'inventory.sqlite'}?mode=ro", uri=True)) as connection:
        for path in read_exclusions(arguments):
            if connection.execute("SELECT 1 FROM files WHERE path=? AND status='candidate'", (path,)).fetchone():
                raise ValueError("inventory includes an explicitly excluded path; create a fresh inventory: " + path)
    search = arguments.directory / "search.json"
    search_bytes = arguments.search.read_bytes()
    if search.exists() and search.read_bytes() != search_bytes:
        raise ValueError("search differs from the existing run; choose a fresh directory")
    search.write_bytes(search_bytes)
    search_hash = hashlib.sha256(search_bytes).hexdigest()
    selected = select_run_inputs(arguments, inventory)
    cancel_requested = 0

    def request_cancel(signum, _frame):
        nonlocal cancel_requested
        cancel_requested = signum

    def analyze(name, inputs):
        if cancel_requested:
            return name, "not started (cancelled)"
        grammar = registry["grammars"][name]
        output = arguments.directory / f"{name}.report.json"
        if output.exists():
            report = json.loads(output.read_text())
            if report["schema"] != 2:
                raise ValueError("existing report uses an older layout schema; choose a fresh run directory: " + name)
            if (report["code_corpora_sha"] != metadata_sha(arguments) or
                    report["sources_sha256"] != sha256(inputs["sources"]) or
                    report["search_sha256"] != search_hash or
                    report["grammar_sha256"] != sha256(grammar["library"])):
                raise ValueError("existing report inputs changed: " + name)
            return name, "already complete" if report["complete"] else (130 if report.get("cancelled") else 1)
        with (arguments.directory / f"{name}.log").open("w") as log:
            process = subprocess.Popen([str(arguments.binary), "run", grammar["library"], grammar["symbol"], name,
                inputs["sources"], str(search), str(output)], stdout=log, stderr=log)
            forwarded = False
            while True:
                if cancel_requested and not forwarded:
                    process.send_signal(signal.SIGTERM)
                    forwarded = True
                try:
                    process.wait(timeout=0.25)
                    break
                except subprocess.TimeoutExpired:
                    pass
        status = process.returncode
        if status == 0 and not json.loads(output.read_text())["complete"]:
            status = 1
        return name, status
    previous = {number: signal.signal(number, request_cancel) for number in (signal.SIGINT, signal.SIGTERM)}
    unsuccessful = False
    try:
        with ThreadPoolExecutor(max_workers=arguments.jobs) as executor:
            futures = [executor.submit(analyze,name,inputs) for name,inputs in sorted(selected["grammars"].items(),key=lambda item:item[1]["unique"])]
            for future in as_completed(futures):
                name, status = future.result()
                print("analysis", name, status, flush=True)
                unsuccessful |= isinstance(status, int) and status != 0
    finally:
        for number, handler in previous.items():
            signal.signal(number, handler)
    if cancel_requested:
        raise SystemExit(128 + cancel_requested)
    if unsuccessful:
        raise SystemExit(1)


def metadata_sha(arguments):
    return json.loads((arguments.directory / "provenance.json").read_text())["code_corpora_sha"]


def summarize(arguments):
    directory = arguments.directory
    provenance = json.loads((directory / "provenance.json").read_text())
    sha = provenance["code_corpora_sha"]
    selection = json.loads((directory / "run-inputs.json").read_text())
    totals = {}
    missing, failed = [], []
    with (directory / "configurations.jsonl").open("w") as output:
        for grammar in selection["grammars"]:
            path = directory / f"{grammar}.report.json"
            if not path.exists():
                missing.append(grammar)
                continue
            report = json.loads(path.read_text())
            if report["code_corpora_sha"] != sha:
                raise ValueError("report code-corpora SHA mismatch: " + grammar)
            if not report["complete"]:
                failed.append(grammar)
            for scope, aggregate in report["scopes"].items():
                for index, evaluation in enumerate(aggregate["configurations"]):
                    config = evaluation["configuration"]
                    invalid = aggregate["invalid_file_counts"][index]
                    row = dict(evaluation, grammar=grammar, scope=scope,
                        grammar_sha256=report["grammar_sha256"], code_corpora_sha=sha,
                        invalid_files=invalid, partial=not report["complete"])
                    output.write(json.dumps(row) + "\n")
                    key = (scope, config["variant"], config["capacity"])
                    total = totals.setdefault(key, dict(scope=scope, variant=config["variant"],
                        capacity=config["capacity"], code_corpora_sha=sha,
                        files=0, nodes=0, total_bytes=0, invalid_files=0, partial=False))
                    total["files"] += aggregate["files"]
                    total["nodes"] += aggregate["nodes"]
                    total["total_bytes"] += evaluation["totals"]["total_bytes"]
                    total["invalid_files"] += invalid
                    total["partial"] |= not report["complete"]
    for row in totals.values():
        row["partial"] |= bool(missing)
    coverage = dict(missing_grammars=missing, incomplete_grammars=failed,
        missing_repositories=provenance["missing_repositories"],
        inventory=json.loads((directory / "inventory-summary.json").read_text()),
        selection=selection)
    write_json(directory / "summary.json", dict(code_corpora_sha=sha, coverage=coverage))
    write_json(directory / "variants.json", list(totals.values()))
    lines = ["# Memory Pareto exploration", "", f"code-corpora SHA: `{sha}`", "",
        "All-u8 and seven independent single-field u16 variants; SWAR symbol/field packing.",
        "Storage estimates include headers, padding and unused slots. CPU costs are not measured.", "",
        f"Missing reports: {', '.join(missing) or 'none'}. Incomplete reports: {', '.join(failed) or 'none'}.",
        f"Missing repositories: {', '.join(provenance['missing_repositories']) or 'none'}.", "",
        "See summary.json for classified coverage and exclusions, provenance.json for image and input identities,",
        "and configurations.jsonl for every layout, including invalid and dominated layouts.", ""]
    capacities = json.loads((directory / "search.json").read_text())["capacities"]
    for scope in ("training/repository", "test/repository", "all"):
        entries = [r for r in totals.values() if r["scope"] == scope]
        if not entries: continue
        lines += [f"## {scope}", "", "Bytes per node; partial runs cover only successfully processed inputs.", "",
            "| Variant | " + " | ".join(str(c) + " slots" for c in capacities) + " |",
            "| --- | " + " | ".join("---:" for _ in capacities) + " |"]
        for variant in dict.fromkeys(row["variant"] for row in entries):
            cells = [variant]
            for capacity in capacities:
                row = totals.get((scope, variant, capacity))
                if not row or not row["nodes"]: cell = "—"
                elif row["invalid_files"]: cell = f"invalid ({row['invalid_files']} files)"
                else: cell = f"{row['total_bytes'] / row['nodes']:.3f}" + (" partial" if row["partial"] else "")
                cells.append(cell)
            lines.append("| " + " | ".join(cells) + " |")
        lines.append("")
    (directory / "results.md").write_text("\n".join(lines) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["all", "summarize"])
    parser.add_argument("--directory", type=Path, default=Path("/out"))
    parser.add_argument("--corpus", type=Path, default=Path("/input"))
    parser.add_argument("--grammar-root", type=Path, default=Path("/opt/corpus/grammars"))
    parser.add_argument("--binary", type=Path, default=Path("/opt/memory-pareto/tree-sitter-memory-pareto"))
    parser.add_argument("--search", type=Path, default=Path(__file__).with_name("search.json"))
    parser.add_argument("--exclusions", type=Path, default=Path(__file__).with_name("exclusions.json"))
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--max-file-bytes", type=int, default=4*1024*1024)
    parser.add_argument("--skip-tsx-tail", type=int, default=3000)
    arguments = parser.parse_args()
    if arguments.jobs < 1 or arguments.max_file_bytes < 0 or arguments.skip_tsx_tail < 0:
        parser.error("jobs must be positive; cutoffs must be nonnegative")
    arguments.directory.mkdir(parents=True, exist_ok=True)
    if arguments.action == "summarize":
        summarize(arguments)
        return
    if any(arguments.directory.iterdir()):
        parser.error("choose a fresh, empty output directory")
    prepare(arguments)
    os.environ["CODE_CORPORA_SHA"] = metadata_sha(arguments)
    inventory(arguments)
    try:
        run(arguments)
    finally:
        if (arguments.directory / "run-inputs.json").exists():
            summarize(arguments)


if __name__ == "__main__":
    main()
