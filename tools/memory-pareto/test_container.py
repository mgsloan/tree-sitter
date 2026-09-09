"""Container boundary, pin validation, and complete runner smoke checks."""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import container
import run_corpus

SHA = "a" * 40


class ContainerTests(unittest.TestCase):
    def test_remote_arguments_preserve_spaces_and_shell_metacharacters(self):
        arguments = SimpleNamespace(host="builder", remote_checkout="/work/a tree;echo bad",
            code_corpora=Path("/data/code corpora"), directory=Path("/out/a $run"), jobs=2,
            max_file_bytes=1000, skip_tsx_tail=0, image=None, build_image=None,
            grammars_image=None, repo=["a repo"])
        command = container.remote_command(arguments)
        self.assertEqual(command[:2], ["ssh", "builder"])
        remote = shlex.split(command[2])
        self.assertEqual(remote[1], "/work/a tree;echo bad/tools/memory-pareto/container.py")
        self.assertEqual(remote[remote.index("--directory") + 1], "/out/a $run")
        self.assertEqual(remote[-2:], ["--repo", "a repo"])

    def test_mutable_image_reference_is_rejected_before_execution(self):
        with patch.object(container, "command") as command:
            with self.assertRaisesRegex(ValueError, "immutable"):
                container.image_id("grammars:latest")
            command.assert_not_called()

    def test_prepare_checks_pin_checksum_and_quarantine(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            corpus, output, grammars = root / "input", root / "out", root / "grammars"
            corpus.mkdir(); output.mkdir(); grammars.mkdir()
            library = grammars / "json"
            library.mkdir()
            (library / "parser.so").write_bytes(b"library")
            artifact = dict(sha=SHA, sha256=run_corpus.sha256(library / "parser.so"), symbol="tree_sitter_json")
            run_corpus.write_json(library / "artifact.json", artifact)
            run_corpus.write_json(root / "grammar-catalog.json", [dict(name="json", status="built"),
                dict(name="bad", status="quarantined-smoke-failure")])
            run_corpus.write_json(corpus / "metadata.json", dict(code_corpora_sha=SHA,
                selected_grammars=[dict(name="json", sha=SHA), dict(name="bad", sha=SHA)],
                classification=dict(suffixes={"json": "json"}, first_lines=[])))
            arguments = SimpleNamespace(corpus=corpus, directory=output, grammar_root=grammars,
                binary=library / "parser.so")
            run_corpus.prepare(arguments)
            registry = json.loads((output / "grammars.json").read_text())
            self.assertEqual(registry["code_corpora_sha"], SHA)
            self.assertEqual(registry["grammars"]["json"]["status"], "ready")
            self.assertEqual(registry["grammars"]["bad"]["status"], "unavailable")
            artifact["sha"] = "b" * 40
            run_corpus.write_json(library / "artifact.json", artifact)
            with self.assertRaisesRegex(ValueError, "pin differs"):
                run_corpus.prepare(arguments)
            artifact["sha"] = SHA
            artifact["sha256"] = "wrong"
            run_corpus.write_json(library / "artifact.json", artifact)
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                run_corpus.prepare(arguments)

    @unittest.skipUnless(os.environ.get("PARETO_BINARY") and os.environ.get("PARETO_JSON_LIBRARY"),
                         "requires analyzer and JSON grammar")
    def test_complete_pipeline_records_sha_weights_and_cutoff(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            corpus, output, grammars = root / "input", root / "out", root / "grammars"
            corpus.mkdir(); grammars.mkdir()
            for split in ("training", "test"):
                (corpus / split).mkdir()
                (corpus / split / "small.json").write_text('{"x": [1, 2]}')
            (corpus / "test/large.json").write_text(' ' * 101)
            (corpus / "test/link.json").symlink_to(corpus / "training/small.json")
            grammar = grammars / "json"
            grammar.mkdir()
            shutil.copyfile(os.environ["PARETO_JSON_LIBRARY"], grammar / "parser.so")
            run_corpus.write_json(grammar / "artifact.json", dict(sha=SHA,
                sha256=run_corpus.sha256(grammar / "parser.so"), symbol="tree_sitter_json"))
            run_corpus.write_json(root / "grammar-catalog.json", [dict(name="json", status="built")])
            run_corpus.write_json(corpus / "metadata.json", dict(code_corpora_sha=SHA,
                selected_grammars=[dict(name="json", sha=SHA)], missing_repositories=[],
                tracked_files=["training/small.json", "test/small.json", "test/large.json"],
                classification=dict(suffixes={"json": "json"}, first_lines=[])))
            subprocess.run([sys.executable, str(Path(run_corpus.__file__)), "all",
                "--corpus", str(corpus), "--directory", str(output), "--grammar-root", str(grammars),
                "--binary", os.environ["PARETO_BINARY"], "--max-file-bytes", "100"], check=True)
            report = json.loads((output / "json.report.json").read_text())
            self.assertEqual(report["code_corpora_sha"], SHA)
            self.assertEqual(report["configuration_count"], 40)
            self.assertEqual(report["processed_unique"], 1)
            self.assertEqual(report["physical_files"], 2)
            self.assertTrue(report["complete"])
            summary = json.loads((output / "summary.json").read_text())
            self.assertEqual(summary["code_corpora_sha"], SHA)
            self.assertEqual(summary["coverage"]["inventory"]["counts"]["symlink"], 1)
            self.assertEqual(summary["coverage"]["inventory"]["excluded_size_files"], 1)
            rows = [json.loads(line) for line in (output / "configurations.jsonl").read_text().splitlines()]
            self.assertEqual(len(rows), 120)
            self.assertTrue(all(row["code_corpora_sha"] == SHA for row in rows))
            self.assertTrue(any(not row["pareto"] for row in rows))
            self.assertIn(SHA, (output / "results.md").read_text())
            report["code_corpora_sha"] = "b" * 40
            run_corpus.write_json(output / "json.report.json", report)
            with self.assertRaisesRegex(ValueError, "SHA mismatch"):
                run_corpus.summarize(SimpleNamespace(directory=output))


if __name__ == "__main__":
    unittest.main()
