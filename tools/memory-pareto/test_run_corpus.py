"""Checks for cutoff boundaries, occurrence weights, and corpus provenance."""
import json
from contextlib import closing
from pathlib import Path
import sqlite3
import tempfile
from types import SimpleNamespace
import unittest

import run_corpus


class SizeLimitTests(unittest.TestCase):
    def test_tsx_tail_is_weighted_recorded_and_not_applied_twice(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            entries = [dict(path=f"/{i}.tsx", sha256=str(i), weights={"training/repository": i + 1}) for i in range(4)]
            source = root / "tsx.sources.jsonl"
            original = "".join(json.dumps(entry) + "\n" for entry in entries)
            source.write_text(original)
            with closing(sqlite3.connect(root / "inventory.sqlite")) as connection, connection:
                connection.execute("CREATE TABLE files(path TEXT PRIMARY KEY, size INTEGER)")
                connection.executemany("INSERT INTO files VALUES(?,?)", ((entry["path"], 100) for entry in entries))
            inventory = dict(grammars={"tsx": dict(sources=str(source), unique=4, files=10, bytes=1000)})
            arguments = SimpleNamespace(directory=root, skip_tsx_tail=2)
            selected = run_corpus.select_run_inputs(arguments, inventory)
            self.assertEqual(selected["grammars"]["tsx"]["unique"], 2)
            self.assertEqual(selected["grammars"]["tsx"]["files"], 3)
            self.assertEqual(selected["excluded_tsx_files"], 7)
            self.assertEqual(selected["excluded_tsx_bytes"], 700)
            self.assertEqual(run_corpus.select_run_inputs(arguments, inventory), selected)
            self.assertEqual(source.read_text(), original)
            self.assertEqual(len((root / "excluded-tsx-tail.jsonl").read_text().splitlines()), 2)
            arguments.skip_tsx_tail = 0
            with self.assertRaisesRegex(ValueError, "fresh directory"):
                run_corpus.select_run_inputs(arguments, inventory)

    def test_tsx_tail_handles_zero_and_small_corpora(self):
        for skip in (0, 3000):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source = root / "tsx.sources.jsonl"
                source.write_text(json.dumps(dict(path="/x", sha256="x", weights={"test/repository": 1})) + "\n")
                with closing(sqlite3.connect(root / "inventory.sqlite")) as connection, connection:
                    connection.execute("CREATE TABLE files(path TEXT PRIMARY KEY, size INTEGER)")
                    connection.execute("INSERT INTO files VALUES('/x',1)")
                inventory = dict(grammars={"tsx": dict(sources=str(source), unique=1, files=1, bytes=1)})
                selected = run_corpus.select_run_inputs(SimpleNamespace(directory=root, skip_tsx_tail=skip), inventory)
                self.assertEqual("tsx" in selected["grammars"], skip == 0)
                self.assertEqual(selected["excluded_tsx_unique"], int(skip != 0))

    def test_inventory_applies_limit_before_hashing_and_keeps_boundary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            corpus, directory = root / "corpus", root / "run"
            corpus.mkdir()
            directory.mkdir()
            (corpus / "training").mkdir()
            (corpus / "test").mkdir()
            run_corpus.write_json(corpus / "metadata.json", dict(tracked_files=["training/generated.c"]))
            run_corpus.write_json(directory / "provenance.json", dict(code_corpora_sha="a" * 40))
            (corpus / "training/generated.c").write_bytes(b"x" * 100)
            (corpus / "training/large.c").write_bytes(b"x" * 101)
            run_corpus.write_json(directory / "grammars.json", dict(suffixes={"c": "c"}, first_lines=[], grammars={"c": {"status": "ready"}}))
            exclusions = root / "exclusions.json"
            run_corpus.write_json(exclusions, [])
            run_corpus.inventory(SimpleNamespace(directory=directory, corpus=corpus, max_file_bytes=100, exclusions=exclusions))
            with closing(sqlite3.connect(directory / "inventory.sqlite")) as connection:
                rows = connection.execute("SELECT path,sha256,status FROM files ORDER BY path").fetchall()
            self.assertIsNotNone(rows[0][1])
            self.assertEqual(rows[0][2], "candidate")
            self.assertIsNone(rows[1][1])
            self.assertEqual(rows[1][2], "excluded_size")
            run_corpus.write_json(exclusions, [dict(path="training/generated.c", reason="new explicit exclusion")])
            with self.assertRaisesRegex(ValueError, "inventory includes an explicitly excluded path"):
                run_corpus.run(SimpleNamespace(directory=directory, exclusions=exclusions))


if __name__ == "__main__":
    unittest.main()
