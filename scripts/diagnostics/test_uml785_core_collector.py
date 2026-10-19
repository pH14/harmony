# SPDX-License-Identifier: AGPL-3.0-or-later
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("collector", Path(__file__).with_name("uml785-core-collector.py"))
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)


class CoreCollectorTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        process = self.root / "proc/123"
        process.mkdir(parents=True)
        (process / "status").write_text("Name:\tfixture\nUid:\t1001\t1001\t1001\t1001\n")
        (process / "exe").write_bytes(b"exact reviewed executable")
        (process / "maps").write_text("1000-2000 rw-s 0 00:00 0 /owned/fixture-stack\n")
        (process / "coredump_filter").write_text("00000033\n")
        self.output = self.root / "output"
        self.output.mkdir()
        self.config = {"uid": 1001, "output": str(self.output), "per_core_limit": 4,
                       "total_limit": 6, "count_limit": 2,
                       "executables": {collector.digest(process / "exe"): "linux"}}

    def capture(self, data=b"core", pid="123", uid="1001", timestamp="1"):
        collector.collect(self.config, pid, uid, timestamp, io.BytesIO(data), self.root / "proc")

    def test_exact_uid_and_executable_preserve_complete_bytes(self):
        self.capture()
        self.assertEqual((self.output / "123-1-linux.core").read_bytes(), b"core")
        metadata = json.loads((self.output / "123-1-linux.json").read_text())
        self.assertFalse(metadata["truncated"])
        self.assertEqual(metadata["sha256"], hashlib.sha256(b"core").hexdigest())
        self.assertIn("rw-s", metadata["process"]["maps"]["text"])
        self.assertEqual(metadata["process"]["coredump_filter"]["text"], "00000033\n")
        self.assertEqual(set(metadata["process"]), {"maps", "status", "coredump_filter"})

    def test_unrelated_executable_and_argument_uid_are_discarded(self):
        self.capture(uid="0")
        (self.root / "proc/123/exe").write_bytes(b"unrelated executable")
        self.capture()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_proc_uid_must_independently_match(self):
        (self.root / "proc/123/status").write_text("Uid:\t0\t0\t0\t0\n")
        self.capture()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_per_core_total_and_count_limits_are_enforced(self):
        self.capture(b"123456")
        self.capture(b"abcd", timestamp="2")
        self.capture(b"ignored", timestamp="3")
        self.assertEqual((self.output / "123-1-linux.core").read_bytes(), b"1234")
        self.assertEqual((self.output / "123-2-linux.core").read_bytes(), b"ab")
        self.assertEqual(len(list(self.output.glob("*.core"))), 2)
        self.assertTrue(all(json.loads(path.read_text())["truncated"]
                            for path in self.output.glob("*.json")))

    def test_numeric_kernel_arguments_only(self):
        self.capture(pid="../123")
        self.assertEqual(list(self.output.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
