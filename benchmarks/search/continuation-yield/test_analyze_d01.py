import unittest
import math

from analyze_d01 import summarize


def record(index, lower, upper, hit):
    return {"index": index, "seed": index + 10,
            "endpoint_evidence": {"observed_full_budget": True, "budget_frames": 100,
                                  "restricted_cost_interval": [lower, upper],
                                  "hit_by_budget": hit, "arrival_interval": None}}


class RegisteredPanel(unittest.TestCase):
    def test_censored_controls_remain_in_hit_and_cost_denominators(self):
        rows = [record(0, 20, 30, True), record(1, 100, 100, False),
                record(2, 100, 100, False), record(3, 90, 100, None)]
        out = summarize(rows, [10, 11, 12, 13], 100)
        self.assertEqual(out["hit_fraction_interval"], [0.25, 0.5])
        self.assertEqual(out["mean_restricted_cost_interval"], [77.5, 82.5])
        self.assertEqual(out["known_nonattainments"], 2)
        self.assertIsNone(out["cv_of_uncensored_milestone_time"])

    def test_missing_or_replaced_seed_cannot_become_a_complete_panel(self):
        rows = [record(i, 20, 30, True) for i in range(4)]
        with self.assertRaises(AssertionError):
            summarize(rows[:3], [10, 11, 12, 13], 100)
        with self.assertRaises(AssertionError):
            summarize(rows, [10, 11, 12, 99], 100)

    def test_uncensored_exact_sample_has_known_cv(self):
        rows = [record(i, value, value, True) for i, value in enumerate([1, 1, 3, 3])]
        out = summarize(rows, [10, 11, 12, 13], 100)
        for bound in out["cv_of_uncensored_milestone_time"]:
            self.assertAlmostEqual(bound, 1 / math.sqrt(3))


if __name__ == "__main__":
    unittest.main()
