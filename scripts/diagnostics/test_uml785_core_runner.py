# SPDX-License-Identifier: AGPL-3.0-or-later
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("runner", Path(__file__).with_name("run-uml785-core.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class CoreRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_exact_input_hashes_and_symlinks_are_checked(self):
        source = self.root / "linux"
        source.write_bytes(b"reviewed")
        expected = {"linux": hashlib.sha256(b"reviewed").hexdigest()}
        with patch.object(runner, "EXPECTED", expected):
            self.assertEqual(runner.hashes(self.root), expected)
            source.write_bytes(b"changed")
            with self.assertRaises(ValueError):
                runner.hashes(self.root)
            source.unlink()
            backing = self.root / "backing"
            backing.write_bytes(b"reviewed")
            source.symlink_to(backing)
            with self.assertRaises(ValueError):
                runner.hashes(self.root)

    def suite_result(self, check, environment):
        def execute(command, **kwargs):
            self.assertEqual(kwargs["env"], environment)
            self.assertNotIn("GITHUB_TOKEN", kwargs["env"])
            self.assertEqual(command[3], str(self.root / "harmony-uml-qualify"))
            report = Path(command[command.index("--report") + 1])
            report.write_text(json.dumps({"result": "fail", "checks": [check]}))
            return SimpleNamespace(returncode=1)
        with patch.object(runner.subprocess, "run", side_effect=execute):
            return runner.run_suite(self.root, self.root / "output", environment, "checkpoint", 300)

    def test_original_check_and_signal_are_both_required(self):
        result = self.suite_result({"name": "checkpoint_import_values", "passed": False,
                                   "detail": {"error": "guest stopped: Signaled(11)"}}, {"PATH": "/usr/bin:/bin"})
        self.assertTrue(result["import_values_signaled_11"])

    def test_a_different_failure_is_preserved_without_claiming_reproduction(self):
        result = self.suite_result({"name": "checkpoint_fresh_values", "passed": False,
                                   "detail": {"error": "guest stopped: Signaled(11)"}}, {"PATH": "/usr/bin:/bin"})
        self.assertFalse(result["import_values_signaled_11"])
        self.assertEqual(result["failed_checks"], ["checkpoint_fresh_values"])


if __name__ == "__main__":
    unittest.main()
