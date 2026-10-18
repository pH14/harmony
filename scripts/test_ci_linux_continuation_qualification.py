# SPDX-License-Identifier: AGPL-3.0-or-later
import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("linux_qualification", Path(__file__).with_name("qualify-linux-continuation.py"))
L = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(L)
Q = L.Q
OBSERVATION = "X3_RESULT" + " 1234567890abcdef" * 7 + " "


class ContinuationQualificationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.artifact = {"source": "fixed-source", "files": {name: "a" * 64 for name in L.FILES}}
        self.exported = self.root / "exported"
        (self.exported / "cut").mkdir(parents=True)
        for name in L.CUT_FILES:
            (self.exported / "cut" / name).write_text(OBSERVATION if name == "observation.txt" else "None" if name == "host-class.txt" else name)
        self.cut = L.hashes(self.exported / "cut", L.CUT_FILES)
        self.original = self.receipt(self.exported, 1, "AuthenticAMD", "local")
        self.imports = [self.receipt(self.root / str(i), i, "GenuineIntel", "import") for i in (2, 3)]
        self.output = self.root / "verdict.json"
        identity = patch.object(L, "identity", return_value=self.artifact)
        identity.start()
        self.addCleanup(identity.stop)

    def receipt(self, directory, number, vendor, kind, status="matched", observation=OBSERVATION):
        directory.mkdir(exist_ok=True)
        if kind == "local":
            log = f"X3_REUSED=refused-amd-unchanged-usable\nX3_RAM_NEGATIVE=detected\nX3_CPU_NEGATIVE=detected\nX3_OBSERVATION={observation}\nX3_LOCAL=uninterrupted,capture-neutral,fresh,pressure,reused-or-refused\n"
        else:
            log = f"X3_SOURCE_CLASS=None\nX3_TARGET_CLASS={'None' if status == 'matched' else 'Some(Performance)'}\nX3_IMPORT={status}\n" + (f"X3_OBSERVATION={observation}\n" if status == "matched" else "")
        log += "test result: ok. 1 passed; 0 failed; 0 ignored;\n"
        (directory / "execution.log").write_text(log)
        Q.write(directory / "receipt.json", {"version": 1, "cell": L.cell(), "artifact": self.artifact,
            "kind": kind, "host": {"instance_id": f"12345678-1234-1234-1234-{number:012d}",
                "hostname": "reused-runner-name", "kernel": "linux", "vendor": vendor, "cpuinfo": f"vendor_id : {vendor}\n"},
            "log_sha256": Q.digest(directory / "execution.log"), "evidence": L.evidence(log, kind), "cut": self.cut})
        return directory / "receipt.json"

    def compare(self, imports=None):
        L.compare(self.root, self.original, self.imports if imports is None else imports, self.output)

    def alter(self, path, field, value):
        record = Q.read(path)
        record[field] = value
        Q.write(path, record)

    def test_supported_cross_vendor_migration_qualifies(self):
        self.compare()
        self.assertEqual(Q.read(self.output)["outcome"], "matched")

    def test_explicit_refusal_retained_but_cannot_qualify_cross_vendor(self):
        refusal = self.receipt(self.root / "refused", 4, "GenuineIntel", "import", "refused-host-core-class-unchanged")
        same_vendor = self.receipt(self.root / "amd", 5, "AuthenticAMD", "import")
        with self.assertRaisesRegex(ValueError, "cross-vendor"):
            self.compare([refusal, same_vendor])
        verdict = Q.read(self.output)
        self.assertEqual(verdict["outcome"], "inconclusive")
        self.assertEqual(verdict["imports"][0]["outcome"], "refused-host-core-class-unchanged")
        self.compare([refusal, self.imports[0]])

    def test_missing_and_repeated_hosts_are_inconclusive(self):
        for imports in ([], self.imports[:1], [self.imports[0], self.imports[0]], [self.original]):
            with self.subTest(imports=imports), self.assertRaises(ValueError):
                self.compare(imports)
            self.assertEqual(Q.read(self.output)["outcome"], "inconclusive")

    def test_different_binary_or_kernel_cannot_share_evidence(self):
        for name in L.FILES:
            changed = copy.deepcopy(self.artifact)
            changed["files"][name] = "b" * 64
            self.alter(self.imports[0], "artifact", changed)
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "mismatched"):
                self.compare()

    def test_snapshot_substitution_and_corruption_refuse(self):
        self.alter(self.imports[0], "cut", {**self.cut, "memory.bin": "b" * 64})
        with self.assertRaisesRegex(ValueError, "substituted"):
            self.compare()
        self.alter(self.imports[0], "cut", self.cut)
        (self.exported / "cut/state.bin").write_text("changed")
        with self.assertRaisesRegex(ValueError, "snapshot identity"):
            self.compare()

    def test_changed_guest_observation_is_divergent(self):
        self.imports[0] = self.receipt(self.root / "different", 6, "GenuineIntel", "import", observation=OBSERVATION.replace("1", "2"))
        with self.assertRaisesRegex(ValueError, "contradiction"):
            self.compare()
        self.assertEqual(Q.read(self.output)["outcome"], "diverged")

    def test_no_capture_neutral_or_negative_control_is_not_success(self):
        log = (self.exported / "execution.log").read_text()
        for key in ("X3_LOCAL", "X3_RAM_NEGATIVE", "X3_CPU_NEGATIVE", "X3_REUSED", "X3_OBSERVATION"):
            changed = "\n".join(line for line in log.splitlines() if not line.startswith(key))
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "missing"):
                L.evidence(changed, "local")
        with self.assertRaises(ValueError):
            L.evidence(log.replace("1 passed", "0 passed"), "local")
        with self.assertRaises(ValueError):
            L.evidence(log + log, "local")

    def test_receipt_does_not_override_actual_log(self):
        self.alter(self.imports[0], "evidence", {"import": "matched", "observation": "fabricated"})
        with self.assertRaisesRegex(ValueError, "execution log"):
            self.compare()
        self.alter(self.imports[0], "evidence", L.evidence((self.imports[0].parent / "execution.log").read_text(), "import"))
        (self.imports[0].parent / "execution.log").write_text("changed")
        with self.assertRaisesRegex(ValueError, "execution log"):
            self.compare()

    def test_unknown_host_and_unknown_refusal_do_not_count(self):
        host = Q.read(self.imports[0])["host"]
        self.alter(self.imports[0], "host", {**host, "vendor": "unknown"})
        with self.assertRaisesRegex(ValueError, "vendor"):
            self.compare()
        with self.assertRaisesRegex(ValueError, "IMPORT"):
            L.evidence("X3_IMPORT=some-error\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n", "import")

    def test_vendor_cannot_be_substituted_for_cross_vendor_credit(self):
        host = Q.read(self.imports[0])["host"]
        self.alter(self.imports[0], "host", {**host, "vendor": "AuthenticAMD"})
        with self.assertRaisesRegex(ValueError, "CPU inventory"):
            self.compare()

    def test_invalid_identity_replaces_an_earlier_passing_verdict(self):
        self.compare()
        with patch.object(L, "identity", side_effect=ValueError("identity mismatch")), self.assertRaises(ValueError):
            self.compare()
        self.assertEqual(Q.read(self.output)["outcome"], "inconclusive")

    def test_invalid_registry_replaces_an_earlier_passing_verdict(self):
        self.compare()
        with patch.object(L, "cell", side_effect=ValueError("invalid registry")), self.assertRaises(ValueError):
            self.compare()
        self.assertEqual(Q.read(self.output)["outcome"], "inconclusive")

    def test_workflow_uses_one_bundle_for_export_and_import(self):
        import yaml
        jobs = yaml.safe_load((L.ROOT / ".github/workflows/consonance-hardware-qualification.yml").read_text())["jobs"]
        self.assertEqual(jobs["x3-artifact"]["needs"], "guest-image")
        self.assertEqual(jobs["x3-export"]["needs"], "x3-artifact")
        self.assertIn("x3-export", jobs["x3-import"]["needs"])
        for name in ("x3-export", "x3-import"):
            steps = str(jobs[name]["steps"])
            self.assertIn("x3-executable", steps)
            self.assertNotIn("cargo", steps)
        self.assertIn("--import-from x3-export", str(jobs["x3-import"]["steps"]))
        self.assertIn("x3-compare", jobs["check"]["needs"])


if __name__ == "__main__":
    unittest.main()
