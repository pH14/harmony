import json
from pathlib import Path
import unittest

from score_screen import score_screen


def registration():
    return {"screen": {"budget_frames": 100, "required_strict_wins": 3,
                       "pairs": [{"seed": i, "control": f"c{i}", "candidate": f"a{i}"} for i in range(4)]}}


def record(name, interval):
    return {"id": name, "checks_passed": True,
            "endpoint_evidence": {"budget_frames": 100, "observed_full_budget": True,
                                  "hit_by_budget": interval != [100, 100],
                                  "restricted_cost_interval": interval}}


class FixedPanelGate(unittest.TestCase):
    def test_published_development_confirmation_and_failed_transfer_keep_their_scores(self):
        root = Path(__file__).parent
        for name in ('s01', 'r01', 't01'):
            with self.subTest(panel=name):
                reg = json.loads((root / (name + '-registration.json')).read_text())
                results = json.loads((root / (name + '-results.json')).read_text())
                analysis = json.loads((root / (name + '-analysis.json')).read_text())
                published = json.loads(json.dumps(score_screen(results['records'], reg)))
                self.assertEqual(published, analysis['score'])

    def test_both_censored_pairs_are_ties_and_make_three_wins_impossible(self):
        records = [record(f"{arm}{i}", [100, 100]) for i in range(2) for arm in ("c", "a")]
        result = score_screen(records, registration())
        self.assertEqual(result["decision"], "fail_impossible_win_count")
        self.assertEqual(result["strict_wins"], 0)

    def test_one_loss_does_not_stop_a_still_achievable_screen(self):
        records = [record("c0", [50, 51]), record("a0", [80, 81])]
        self.assertEqual(score_screen(records, registration())["decision"], "continue")

    def test_point_estimate_gain_does_not_pass_uncertain_intervals(self):
        records = [record(f"{arm}{i}", [80, 100] if arm == "c" else [60, 78])
                   for i in range(4) for arm in ("c", "a")]
        result = score_screen(records, registration())
        self.assertEqual(result["strict_wins"], 4)
        self.assertEqual(result["decision"], "fail")

    def test_three_strict_wins_and_interval_robust_mean_gain_pass(self):
        records = []
        for i in range(4):
            records += [record(f"c{i}", [99, 100]), record(f"a{i}", [60, 61] if i < 3 else [100, 100])]
        self.assertEqual(score_screen(records, registration())["decision"], "pass")

    def test_partial_next_pair_does_not_count_as_a_win_or_loss(self):
        self.assertEqual(score_screen([record("a0", [10, 11])], registration())["completed_pairs"], 0)

    def test_resource_overrun_cannot_be_promoted_as_a_frame_gain(self):
        reg = registration()
        reg["screen"]["resources"] = {"max_candidate_to_control_ratio": 1.25}
        records = []
        for i in range(4):
            for arm in ("c", "a"):
                row = record(f"{arm}{i}", [99, 100] if arm == "c" else [60, 61])
                row["summary"] = {"cpu_seconds": 1 if arm == "c" else 2, "elapsed_seconds": 1}
                records.append(row)
        self.assertEqual(score_screen(records, reg)["decision"], "fail_resource_gate")


if __name__ == "__main__":
    unittest.main()
