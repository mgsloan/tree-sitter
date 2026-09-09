"""Signal integration tests; set PARETO_BINARY and PARETO_JSON_LIBRARY to run."""
import hashlib
from contextlib import closing
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest


@unittest.skipUnless(os.name == "posix" and os.environ.get("PARETO_BINARY") and os.environ.get("PARETO_JSON_LIBRARY"),
                     "requires a built analyzer and a pinned JSON grammar library")
class CancellationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = os.environ["PARETO_BINARY"]
        self.library = os.environ["PARETO_JSON_LIBRARY"]
        source = self.root / "input.json"
        source.write_text(json.dumps(list(range(1000))))
        entry = dict(path=str(source), sha256=hashlib.sha256(source.read_bytes()).hexdigest(),
                     weights={"training/repository": 1, "test/repository": 2})
        self.line = json.dumps(entry) + "\n"
        self.count = 10000
        self.sources = self.root / "sources.jsonl"
        self.sources.write_text(self.line * self.count)
        self.environment = dict(os.environ, CODE_CORPORA_SHA="a" * 40)
        search = {"capacities": [64]}
        self.search = self.root / "search.json"
        self.search.write_text(json.dumps(search))

    def command(self, sources, report):
        return [self.binary, "run", self.library, "tree_sitter_json", "json", str(sources), str(self.search), str(report)]

    def wait_for_progress(self, process, report):
        deadline = time.monotonic() + 20
        while not Path(str(report) + ".progress.json").exists():
            self.assertIsNone(process.poll(), "runner exited before signal test")
            self.assertLess(time.monotonic(), deadline, "no progress from runner")
            time.sleep(0.01)

    def test_sigint_and_sigterm_save_exact_partial_aggregates(self):
        for number in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=number):
                report_path = self.root / f"cancel-{number}.json"
                with (self.root / f"cancel-{number}.log").open("w") as log:
                    process = subprocess.Popen(self.command(self.sources, report_path), stdout=log, stderr=log, env=self.environment)
                    try:
                        self.wait_for_progress(process, report_path)
                        process.send_signal(number)
                        self.assertEqual(process.wait(timeout=20), 130)
                    finally:
                        if process.poll() is None:
                            process.kill()
                            process.wait()
                report = json.loads(report_path.read_text())
                self.assertTrue(report["cancelled"])
                self.assertFalse(report["complete"])
                self.assertEqual(report["failed_unique"], 0)
                processed = report["processed_unique"]
                self.assertGreater(processed, 0)
                self.assertLess(processed, self.count)
                self.assertEqual(report["planned_unique"], self.count)
                self.assertEqual(processed + report["deferred_unique"], self.count)
                self.assertEqual(report["physical_files"] + report["deferred_occurrences"], self.count * 3)
                ledger = [json.loads(line) for line in Path(str(report_path) + ".files.jsonl").read_text().splitlines()]
                self.assertEqual(len(ledger), self.count)
                self.assertEqual(sum(row["status"] == "ok" for row in ledger), processed)
                self.assertTrue(all(row["status"] == "deferred" for row in ledger[processed:]))
                prefix = self.root / f"prefix-{number}.jsonl"
                prefix.write_text(self.line * processed)
                reference = self.root / f"reference-{number}.json"
                subprocess.run(self.command(prefix, reference), check=True, env=self.environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                expected = json.loads(reference.read_text())
                self.assertTrue(expected["complete"])
                self.assertEqual(report["scopes"], expected["scopes"])

    def test_coordinator_forwards_cancel_and_does_not_start_queued_grammar(self):
        run = self.root / "run"
        run.mkdir()
        exclusions = self.root / "exclusions.json"
        exclusions.write_text("[]\n")
        grammar = dict(library=self.library, symbol="tree_sitter_json")
        (run / "grammars.json").write_text(json.dumps(dict(grammars={"first": grammar, "queued": grammar})))
        inputs = dict(files=self.count * 3, unique=self.count, sources=str(self.sources))
        (run / "inventory-summary.json").write_text(json.dumps(dict(corpus=str(self.root), max_file_bytes=4194304,
            grammars={"first": inputs, "queued": inputs})))
        with closing(sqlite3.connect(run / "inventory.sqlite")) as connection, connection:
            connection.execute("CREATE TABLE files(path TEXT, status TEXT)")
        (run / "provenance.json").write_text(json.dumps(dict(code_corpora_sha="a" * 40, missing_repositories=[])))
        worker = "import sys; from pathlib import Path; from types import SimpleNamespace; import run_corpus; "
        worker += "run_corpus.run(SimpleNamespace(directory=Path(sys.argv[1]), exclusions=Path(sys.argv[2]), "
        worker += "binary=Path(sys.argv[3]), search=Path(sys.argv[4]), jobs=1, skip_tsx_tail=0))"
        command = [sys.executable, "-c", worker, str(run), str(exclusions), self.binary, str(self.search)]
        self.environment["PYTHONPATH"] = str(Path(__file__).parent.resolve())
        with (run / "coordinator.log").open("w") as log:
            process = subprocess.Popen(command, stdout=log, stderr=log, env=self.environment)
            try:
                self.wait_for_progress(process, run / "first.report.json")
                process.send_signal(signal.SIGTERM)
                self.assertEqual(process.wait(timeout=20), 143)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
        self.assertTrue(json.loads((run / "first.report.json").read_text())["cancelled"])
        self.assertFalse((run / "queued.report.json").exists())
        subprocess.run([sys.executable, str(Path(__file__).with_name("run_corpus.py")), "summarize",
                        "--directory", str(run)], check=True, env=self.environment, stdout=subprocess.DEVNULL)
        coverage = json.loads((run / "summary.json").read_text())["coverage"]
        self.assertEqual(coverage["incomplete_grammars"], ["first"])
        self.assertEqual(coverage["missing_grammars"], ["queued"])
        exported = [json.loads(line) for line in (run / "configurations.jsonl").read_text().splitlines()]
        self.assertTrue(all(point["partial"] for point in exported))

    def test_squat_variants_survive_weighted_export(self):
        search = json.loads(Path(__file__).with_name("search.json").read_text())
        self.search.write_text(json.dumps(search))
        self.sources.write_text(self.line)
        run = self.root / "squat"
        run.mkdir()
        output = run / "json.report.json"
        subprocess.run(self.command(self.sources, output), check=True, env=self.environment)
        report = json.loads(output.read_text())
        self.assertTrue(report["complete"])
        self.assertEqual(report["configuration_count"], 40)
        for entry in report["scopes"]["all"]["configurations"]:
            self.assertEqual(entry["totals"]["file_header_bytes"], 3 * 16)
            self.assertGreater(entry["totals"]["lane_waste_bits"], 0)
            self.assertLessEqual(entry["totals"]["lane_waste_bits"] + entry["totals"]["word_tail_waste_bits"], entry["totals"]["padding_bits"])
        (run / "search.json").write_text(json.dumps(search))
        (run / "inventory-summary.json").write_text(json.dumps(dict(corpus=str(self.root), max_file_bytes=4194304,
            grammars={"json": dict(files=3, unique=1, sources=str(self.sources))})))
        (run / "run-inputs.json").write_text((run / "inventory-summary.json").read_text())
        (run / "provenance.json").write_text(json.dumps(dict(code_corpora_sha="a" * 40, missing_repositories=[])))
        subprocess.run([sys.executable, str(Path(__file__).with_name("run_corpus.py")), "summarize",
                        "--directory", str(run)], check=True, env=self.environment, stdout=subprocess.DEVNULL)
        variants = json.loads((run / "variants.json").read_text())
        self.assertEqual(len(variants), 120)
        self.assertEqual(len({entry["variant"] for entry in variants}), 8)
        self.assertTrue(all(entry["invalid_files"] == 0 and not entry["partial"] for entry in variants))
        self.assertTrue(all(entry["code_corpora_sha"] == "a" * 40 for entry in variants))
        self.assertEqual(report["code_corpora_sha"], "a" * 40)
        self.assertIn("end_byte_sub-u16", (run / "results.md").read_text())
        self.assertIn("| 4 slots | 8 slots | 16 slots | 32 slots | 64 slots |", (run / "results.md").read_text())

    def test_column_lengths_and_old_extraction_rejection(self):
        source = self.root / "positions.json"
        raw = ('{"key": "é",\r\n "other": ' + json.dumps(list(range(300))) + '}\r\n').encode()
        source.write_bytes(raw)
        records = self.root / "records.jsonl"
        subprocess.run([self.binary, "extract", self.library, "tree_sitter_json", "json", str(records), str(source)], check=True, env=self.environment)
        extracted = json.loads(records.read_text())
        self.assertEqual(extracted["schema"], 2)
        self.assertTrue(any(node[8] for node in extracted["nodes"]))
        self.assertTrue(any(node[8] == 0 and node[9] > 0 for node in extracted["nodes"]))
        for node in extracted["nodes"]:
            end_column = len(raw[:node[5] + node[6]].rsplit(b"\n", 1)[-1])
            self.assertEqual(node[9] + node[10] if node[8] == 0 else node[10], end_column)
        stale = self.root / "schema1.jsonl"
        extracted["schema"] = 1
        stale.write_text(json.dumps(extracted) + "\n")
        rejected = subprocess.run([self.binary, "analyze", str(stale), str(self.search), str(self.root / "stale.report.json")], capture_output=True, env=self.environment)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn(b"unsupported extraction provenance", rejected.stderr)



if __name__ == "__main__":
    unittest.main()
