# SPDX-License-Identifier: AGPL-3.0-or-later
"""Negative controls for the ARM retained-evidence determinism comparators.

Built on the shared planted-failure harness (spikes/negcontrol.py): each control starts from a
known-good pair of lanes, applies ONE mutation, and asserts the comparator goes red — the same
discipline the AMD floor checker's controls use, written once.
"""

import json
import sys
import tempfile
import unittest
from pathlib import Path


HOST = Path(__file__).resolve().parents[1]
# The shared harness lives at the spikes/ root: …/spikes/arm-altra/host/tests/ -> parents[3].
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from negcontrol import run_grader, write_run_set  # noqa: E402

AA1C = HOST / "aa1c-determinism-check.py"
AA3 = HOST / "aa3-determinism-compare.py"
CAMPAIGN_SCRIPTS = (
    HOST / "aa1c-parallel.sh",
    HOST / "aa1c-run-all.sh",
    HOST / "aa3-exact-shard.sh",
)


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
    """Run a comparator over `inputs` and return (result, parsed-JSON-report). The comparators
    always emit a stable-JSON report on stdout, including on an INVALID_INPUT refusal."""
    result = run_grader(script, *inputs)
    return result, result.json()


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

    def test_divergence_is_p0_even_when_coverage_is_incomplete(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(
                root,
                "solo",
                [aa1c_record(1, "sha256:solo"), aa1c_record(2)],
            )
            cotenant = write_run_set(
                root, "cotenant", [aa1c_record(1, "sha256:cotenant")]
            )
            result, report = run_comparator(AA1C, solo, cotenant)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(report["verdict"], "P0_DIVERGENCE")
            self.assertFalse(report["join_cardinality"]["full_both_sides"])

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

    def test_symmetric_missing_compared_fields_is_rejected_not_matched(self):
        # hm-cte negative control: records omitting all three compared fields (state_digest,
        # measured_taken, overflow.deliveries) on BOTH lanes used to compare None==None and report
        # MATCH having compared nothing. The comparator now requires + type-checks them, so a
        # symmetric schema drift is INVALID_INPUT, never a match.
        def missing(seed):
            r = aa1c_record(seed)
            del r["state_digest"]
            del r["measured_taken"]
            del r["overflow"]["deliveries"]
            return r

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [missing(1)], condition="pinned-solo")
            cotenant = write_run_set(root, "cotenant", [missing(1)], condition="co-tenant-load")
            result, report = run_comparator(AA1C, solo, cotenant)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INVALID_INPUT")
            self.assertIn("malformed comparison record", report["error"])

    def test_self_comparison_is_rejected(self):
        # hm-6sj negative control: the same run-set dir passed as BOTH lanes. Same run_set_id and
        # condition, so it is not a solo-vs-co-tenant contrast — the comparator refuses rather than
        # full-join MATCHing a directory against itself (the most embarrassing false green).
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa1c_record(1), aa1c_record(2)])
            result, report = run_comparator(AA1C, solo, solo)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INVALID_INPUT")
            self.assertIn("same run-set", report["error"])


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

    def test_divergence_is_p0_even_when_coverage_is_incomplete(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(
                root,
                "solo",
                [aa3_record(0, 1, "sha256:solo"), aa3_record(1, 2)],
            )
            cotenant = write_run_set(
                root, "cotenant", [aa3_record(0, 1, "sha256:cotenant")]
            )
            result, report = run_comparator(AA3, solo, cotenant)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(report["verdict"], "P0_DIVERGENCE")
            self.assertFalse(report["join_cardinality"]["full_both_sides"])

    def test_bare_records_file_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa3_record(0, 1)])
            cotenant = write_run_set(root, "cotenant", [aa3_record(0, 1)])
            result, report = run_comparator(
                AA3, solo / "records.jsonl", cotenant / "records.jsonl"
            )
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INVALID_INPUT")
            self.assertIn("run-set directory", report["error"])

    def test_self_comparison_is_rejected(self):
        # hm-6sj negative control: the same run-set dir as solo AND co-tenant. Same run_set_id and
        # condition — not a solo-vs-co-tenant contrast, so the comparator refuses it.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa3_record(0, 1)])
            result, report = run_comparator(AA3, solo, solo)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INVALID_INPUT")
            self.assertIn("same run-set", report["error"])

    def test_copied_lane_same_condition_is_rejected(self):
        # hm-6sj negative control: two DISTINCT run-set dirs mislabelled with the SAME condition —
        # a copied/mislabelled lane. Distinct run_set_ids, but identical conditions cannot be the
        # pinned-solo-vs-co-tenant contrast the comparison claims.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            solo = write_run_set(root, "solo", [aa3_record(0, 1)], condition="pinned-solo")
            cotenant = write_run_set(
                root, "cotenant", [aa3_record(0, 1)], condition="pinned-solo"
            )
            result, report = run_comparator(AA3, solo, cotenant)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report["verdict"], "INVALID_INPUT")
            self.assertIn("same condition", report["error"])


class CampaignMarkerTests(unittest.TestCase):
    def test_same_tag_success_marker_is_invalidated_before_campaign_work(self):
        for script in CAMPAIGN_SCRIPTS:
            with self.subTest(script=script.name):
                source = script.read_text(encoding="utf-8")
                invalidation = source.index('rm -f -- "$OK_MARKER"')
                self.assertLess(invalidation, source.index("cd ~/harmony"))
                self.assertIn('touch "$OK_MARKER"', source)


if __name__ == "__main__":
    unittest.main()
