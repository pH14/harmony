# SPDX-License-Identifier: AGPL-3.0-or-later
"""Negative controls for the ARM retained-evidence determinism comparators."""

import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


HOST = Path(__file__).resolve().parents[1]
AA1C = HOST / "aa1c-determinism-check.py"
AA3 = HOST / "aa3-determinism-compare.py"


def write_run_set(root, name, records):
    run_set = root / name
    run_set.mkdir()
    encoded = b"".join(
        (json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n").encode()
        for record in records
    )
    (run_set / "records.jsonl").write_bytes(encoded)
    manifest = {
        "attempted": len(records),
        "records_file": "records.jsonl",
        "records_sha256": hashlib.sha256(encoded).hexdigest(),
        "run_set_id": name,
    }
    (run_set / "run-set.json").write_text(json.dumps(manifest), encoding="utf-8")
    return run_set


def aa1c_record(seed, digest="sha256:same"):
    return {
        "measured_taken": 100,
        "overflow": {"deliveries": 1, "target": 90},
        "payload": "straight-line",
        "scale": "smoke",
        "seed": seed,
        "state_digest": digest,
    }


def aa3_record(sample_id, seed, digest="sha256:same"):
    return {
        "overflow": {
            "armed": True,
            "landed_digest": digest,
            "target": 90,
        },
        "payload": "straight-line",
        "sample_id": sample_id,
        "scale": "s1e6",
        "seed": seed,
        "state_digest": digest,
    }


def run_comparator(script, *inputs):
    result = subprocess.run(
        [sys.executable, str(script), *(str(path) for path in inputs)],
        check=False,
        capture_output=True,
        text=True,
    )
    return result, json.loads(result.stdout)


class Aa1cComparatorTests(unittest.TestCase):
    def test_full_join_matches(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa1c_record(1), aa1c_record(2)])
            cotenant = write_run_set(
                root, "cotenant", [aa1c_record(1), aa1c_record(2)]
            )
            result, report = run_comparator(AA1C, solo, cotenant)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(report["verdict"], "MATCH")
            self.assertTrue(report["join_cardinality"]["full_both_sides"])

    def test_partial_overlap_is_not_match(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa1c_record(1), aa1c_record(2)])
            cotenant = write_run_set(root, "cotenant", [aa1c_record(1)])
            result, report = run_comparator(AA1C, solo, cotenant)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INCOMPLETE_COVERAGE")
            self.assertEqual(report["join_cardinality"]["solo_only_keys"], 1)

    def test_duplicate_key_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            duplicate = [aa1c_record(1), aa1c_record(1)]
            solo = write_run_set(root, "solo", duplicate)
            cotenant = write_run_set(root, "cotenant", duplicate)
            result, report = run_comparator(AA1C, solo, cotenant)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INVALID_INPUT")
            self.assertIn("duplicate comparison key", report["error"])

    def test_joined_digest_divergence_is_p0(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa1c_record(1, "sha256:solo")])
            cotenant = write_run_set(
                root, "cotenant", [aa1c_record(1, "sha256:cotenant")]
            )
            result, report = run_comparator(AA1C, solo, cotenant)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(report["verdict"], "P0_DIVERGENCE")
            self.assertEqual(report["divergences"][0]["field"], "state_digest")

    def test_manifest_hash_mismatch_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa1c_record(1)])
            cotenant = write_run_set(root, "cotenant", [aa1c_record(1)])
            manifest_path = solo / "run-set.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["records_sha256"] = "0" * 64
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            result, report = run_comparator(AA1C, solo, cotenant)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INVALID_INPUT")
            self.assertIn("sha256", report["error"])


class Aa3ComparatorTests(unittest.TestCase):
    def test_full_join_matches_with_repetitions(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            records = [aa3_record(0, 1), aa3_record(1, 1)]
            solo = write_run_set(root, "solo", records)
            cotenant = write_run_set(root, "cotenant", records)
            result, report = run_comparator(AA3, solo, cotenant)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(report["verdict"], "MATCH")
            self.assertEqual(report["join_cardinality"]["shared_keys"], 1)
            self.assertEqual(report["join_cardinality"]["solo_included_records"], 2)

    def test_partial_overlap_is_not_match(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(
                root,
                "solo",
                [aa3_record(0, 1), aa3_record(1, 1), aa3_record(2, 2), aa3_record(3, 2)],
            )
            cotenant = write_run_set(
                root, "cotenant", [aa3_record(0, 1), aa3_record(1, 1)]
            )
            result, report = run_comparator(AA3, solo, cotenant)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INCOMPLETE_COVERAGE")
            self.assertEqual(report["join_cardinality"]["solo_only_keys"], 1)

    def test_repetition_count_mismatch_is_not_match(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa3_record(0, 1), aa3_record(1, 1)])
            cotenant = write_run_set(root, "cotenant", [aa3_record(0, 1)])
            result, report = run_comparator(AA3, solo, cotenant)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INCOMPLETE_COVERAGE")
            self.assertEqual(report["join_cardinality"]["multiplicity_mismatches"], 1)

    def test_duplicate_sample_id_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            duplicate = [aa3_record(0, 1), aa3_record(0, 1)]
            solo = write_run_set(root, "solo", duplicate)
            cotenant = write_run_set(root, "cotenant", duplicate)
            result, report = run_comparator(AA3, solo, cotenant)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INVALID_INPUT")
            self.assertIn("duplicate sample_id", report["error"])

    def test_tuple_collision_across_cotenant_inputs_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa3_record(0, 1)])
            first = write_run_set(root, "cotenant-a", [aa3_record(0, 1)])
            second = write_run_set(root, "cotenant-b", [aa3_record(0, 1)])
            result, report = run_comparator(AA3, solo, first, second)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INVALID_INPUT")
            self.assertIn("duplicate co-tenant tuple", report["error"])

    def test_joined_exact_landing_divergence_is_p0(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa3_record(0, 1, "sha256:solo")])
            cotenant = write_run_set(
                root, "cotenant", [aa3_record(0, 1, "sha256:cotenant")]
            )
            result, report = run_comparator(AA3, solo, cotenant)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(report["verdict"], "P0_DIVERGENCE")
            self.assertEqual(len(report["divergences"]), 1)


if __name__ == "__main__":
    unittest.main()
