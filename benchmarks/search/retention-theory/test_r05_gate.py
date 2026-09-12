#!/usr/bin/env python3
"""Adversarial allocation-gate fixtures; synthetic records, no emulator work."""
import json
from pathlib import Path
import tempfile
import unittest
from analyze_r05 import analyze
from run_r05 import ARMS, BINARY, SEED


INITIAL = {"brinstar": 0, "morph_ball": 1}
KEY = "metroid_items_tanks_spatial_16_posture_motion_context_selection_32_legacy_progress_v10"


class ReplicationGate(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for label, policy in ARMS:
            cell = self.root / f"runs/r05-{label}/synthetic"
            (cell / "campaign").mkdir(parents=True)
            events = {**INITIAL, **({"energy_tank": 2} if label == "context" else {})}
            rows = [(0, INITIAL), (20_000_000, events), (50_000_000, events)]
            summary = {"status": "complete", "build": {"binary_sha256": BINARY},
                       "search_request": {"seed": SEED, "slot_retention": policy,
                                          "metroid_terminal": "death_or_bcd_underflow_or_ending_v3"},
                       "identity": {"slot_retention": policy, "policies": {"key_policy": KEY},
                                    "source_tree_sha256": "synthetic-fixture"},
                       "last_progress": {"workload_diagnostics": {"underflow_candidate_eligible_endpoints": 0},
                                         "retained_diagnostics": {"underflow_endpoints_cached": 0}},
                       "result": {"frames_emulated": 50_000_000, "verification": "witness",
                                  "stop_reason": "frame_limit", "milestone_witnesses": {name: {} for name in events}},
                       "peak_process_tree_rss_bytes_sampled": 0}
            self.write(label, summary, rows)

    def write(self, label, summary, rows=None):
        cell = self.root / f"runs/r05-{label}/synthetic"
        (cell / "summary.json").write_text(json.dumps(summary))
        if rows is not None:
            values = [{"frames_emulated": frames, "workload_diagnostics": {"named_progress": {"first_seen": events}}}
                      for frames, events in rows]
            (cell / "campaign/progress.jsonl").write_text("\n".join(map(json.dumps, values)) + "\n")

    def summary(self, label):
        return json.loads((self.root / f"runs/r05-{label}/synthetic/summary.json").read_text())

    def test_positive_requires_both_comparisons_and_never_claims_breakthrough(self):
        result = analyze(self.root)
        self.assertTrue(result["decision"]["qualifies_one_longer_development_pair"])
        self.assertFalse(result["decision"]["qualifies_breakthrough"])
        summary = self.summary("quality")
        summary["result"]["milestone_witnesses"]["energy_tank"] = {}
        events = {**INITIAL, "energy_tank": 2}
        self.write("quality", summary, [(0, INITIAL), (20_000_000, events), (50_000_000, events)])
        result = analyze(self.root)
        self.assertTrue(result["comparisons"]["ordinary"]["passes"])
        self.assertFalse(result["decision"]["qualifies_one_longer_development_pair"])

    def test_new_milestone_does_not_excuse_a_lost_control_milestone(self):
        summary = self.summary("ordinary")
        summary["result"]["milestone_witnesses"]["norfair"] = {}
        events = {**INITIAL, "norfair": 2}
        self.write("ordinary", summary, [(0, INITIAL), (20_000_000, events), (50_000_000, events)])
        result = analyze(self.root)
        self.assertEqual(result["comparisons"]["ordinary"]["additional_control_names"], ["norfair"])
        self.assertFalse(result["decision"]["qualifies_one_longer_development_pair"])

    def test_telemetry_upper_bounds_cannot_fabricate_twenty_percent_speedup(self):
        for label, _ in ARMS:
            summary = self.summary(label)
            events = {**INITIAL, "missile_capacity": 2}
            summary["result"]["milestone_witnesses"] = {name: {} for name in events}
            lower, upper = (30_000_000, 32_000_000) if label == "context" else (25_000_000, 40_000_000)
            self.write(label, summary, [(0, INITIAL), (lower, INITIAL), (upper, events), (50_000_000, events)])
        # 32/40 suggests exactly 20% improvement, but the control may arrive at 25M.
        result = analyze(self.root)
        self.assertFalse(result["decision"]["qualifies_one_longer_development_pair"])

    def test_wall_censoring_blocks_an_otherwise_positive_comparison(self):
        summary = self.summary("quality")
        summary["result"]["stop_reason"] = "wall_limit"
        self.write("quality", summary)
        self.assertFalse(analyze(self.root)["decision"]["qualifies_one_longer_development_pair"])

    def test_unmatched_identity_or_missing_witness_fails_closed(self):
        for mutation in ["identity", "witness"]:
            with self.subTest(mutation=mutation):
                summary = self.summary("context")
                original = json.loads(json.dumps(summary))
                if mutation == "identity":
                    summary["identity"]["source_tree_sha256"] = "different-source"
                else:
                    del summary["result"]["milestone_witnesses"]["energy_tank"]
                self.write("context", summary)
                with self.assertRaises(AssertionError):
                    analyze(self.root)
                self.write("context", original)


if __name__ == "__main__":
    unittest.main()
