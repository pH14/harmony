# SPDX-License-Identifier: AGPL-3.0-or-later
import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("qualification", Path(__file__).with_name("qualify-consonance.py"))
Q = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(Q)


class QualificationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.output = self.root / "result.json"
        self.artifact = {"source": "retained-source", "executable_sha256": "a" * 64}
        self.cell = Q.registry()["cells"][0]
        self.receipts = []
        for index in range(2):
            folder = self.root / str(index)
            folder.mkdir()
            tests = {}
            for test in Q.TESTS:
                log = folder / f"{test}.log"
                log.write_text("test result: ok. 1 passed; 0 failed; 0 ignored;\n" + (
                    "X1_DIGEST=" + "b" * 64 + "\nX1_EVENTS=6\nX1_GUEST_DELIVERIES=2\nX1_CHECKPOINTS=3\n" if test == Q.TESTS[0] else ""))
                tests[test] = Q.digest(log)
            receipt = folder / "receipt.json"
            Q.write(receipt, {"version": 1, "cell": self.cell, "artifact": self.artifact,
                "replica": str(index), "host": {"hostname": f"host-{index}", "kernel": "linux", "cpuinfo": "cpu"},
                "tests": tests, "evidence": {"digest": "b" * 64, "events": 6, "guest_deliveries": 2, "checkpoints": 3}})
            self.receipts.append(receipt)
        self.identity = patch.object(Q, "identity", return_value=self.artifact)
        self.identity.start()
        self.addCleanup(self.identity.stop)

    def compare(self, receipts=None):
        Q.compare(self.root, self.receipts if receipts is None else receipts, self.output)

    def alter(self, field, value):
        record = Q.read(self.receipts[1])
        record[field] = value
        Q.write(self.receipts[1], record)

    def test_retained_production_cells_match(self):
        self.compare()
        self.assertEqual(Q.read(self.output)["outcome"], "matched")
        self.assertEqual(Q.read(self.output)["cells"][0]["cell"], self.cell)

    def test_missing_and_duplicate_replicas_refuse(self):
        for receipts in ([], self.receipts[:1], [self.receipts[0], self.receipts[0]]):
            with self.subTest(receipts=receipts), self.assertRaises(ValueError):
                self.compare(receipts)
            self.assertEqual(Q.read(self.output)["outcome"], "inconclusive")

    def test_changed_executable_refuses(self):
        self.alter("artifact", {**self.artifact, "executable_sha256": "c" * 64})
        with self.assertRaisesRegex(ValueError, "mismatched"):
            self.compare()

    def test_same_host_is_not_cross_host_evidence(self):
        self.alter("host", Q.read(self.receipts[0])["host"])
        with self.assertRaisesRegex(ValueError, "replicas"):
            self.compare()

    def test_missing_oracle_and_unknown_fields_refuse(self):
        self.alter("tests", {})
        with self.assertRaisesRegex(ValueError, "oracle"):
            self.compare()
        self.alter("typo", True)
        with self.assertRaisesRegex(ValueError, "fields"):
            self.compare()

    def test_missing_and_corrupt_logs_refuse(self):
        log = self.receipts[1].parent / f"{Q.TESTS[0]}.log"
        log.write_text("fabricated")
        with self.assertRaisesRegex(ValueError, "corrupted"):
            self.compare()
        log.unlink()
        with self.assertRaises(OSError):
            self.compare()

    def test_receipt_cannot_substitute_observation(self):
        self.alter("evidence", {"digest": "c" * 64, "events": 6, "guest_deliveries": 2, "checkpoints": 3})
        with self.assertRaisesRegex(ValueError, "execution log"):
            self.compare()

    def test_cross_host_contradiction_invalidates_verdict(self):
        record = Q.read(self.receipts[1])
        log = self.receipts[1].parent / f"{Q.TESTS[0]}.log"
        log.write_text(log.read_text().replace("b" * 64, "c" * 64))
        record["tests"][Q.TESTS[0]] = Q.digest(log)
        record["evidence"]["digest"] = "c" * 64
        Q.write(self.receipts[1], record)
        with self.assertRaisesRegex(ValueError, "contradiction"):
            self.compare()
        self.assertEqual(Q.read(self.output)["outcome"], "diverged")

    def test_libtest_prefix_does_not_hide_first_observation(self):
        log = (self.receipts[0].parent / f"{Q.TESTS[0]}.log").read_text()
        log = log.replace("X1_DIGEST=", f"test {Q.TESTS[0]} ... X1_DIGEST=")
        self.assertEqual(Q.parse_evidence(log), Q.read(self.receipts[0])["evidence"])

    def test_vacuous_and_duplicate_observations_refuse(self):
        for text in ("", "X1_DIGEST=" + "b" * 64 + "\nX1_EVENTS=0\nX1_GUEST_DELIVERIES=2\nX1_CHECKPOINTS=3\n",
                     (self.receipts[0].parent / f"{Q.TESTS[0]}.log").read_text() * 2):
            with self.subTest(text=text), self.assertRaises(ValueError):
                Q.parse_evidence(text)

    def test_empty_oracles_and_manifest_refuse(self):
        manifest = Q.registry()
        for changed in ({"version": 1, "cells": []}, {"version": 1, "cells": [{**copy.deepcopy(self.cell), "oracles": []}]}):
            with patch.object(Q, "read", return_value=changed), self.assertRaises(ValueError):
                Q.registry()
        self.assertTrue(manifest["cells"])

    def test_invalid_identity_replaces_prior_success(self):
        self.compare()
        with patch.object(Q, "identity", side_effect=ValueError("identity mismatch")):
            with self.assertRaisesRegex(ValueError, "identity mismatch"):
                self.compare()
        self.assertEqual(Q.read(self.output)["outcome"], "inconclusive")

    def test_workflow_builds_once_and_requires_replica_comparison(self):
        import yaml
        workflow = yaml.safe_load((Q.ROOT / ".github/workflows/consonance-hardware-qualification.yml").read_text())
        jobs = workflow["jobs"]
        self.assertEqual(jobs["x1-minimal-guest"]["needs"], "x1-artifact")
        replica = str(jobs["x1-minimal-guest"]["steps"])
        self.assertIn("x1-executable", replica)
        self.assertNotIn("cargo test", replica)
        self.assertIn("--ignored", replica)
        self.assertIn("x1-minimal-guest", jobs["check"]["needs"])
        self.assertIn("qualify-consonance.py compare", str(jobs["check"]["steps"]))
        self.assertIn("JOB_RESULTS", str(jobs["check"]["steps"]))

    def test_duplicate_json_keys_refuse(self):
        path = self.root / "duplicate.json"
        path.write_text('{"cells": [], "cells": [1]}')
        with self.assertRaisesRegex(ValueError, "duplicate"):
            Q.read(path)


if __name__ == "__main__":
    unittest.main()
