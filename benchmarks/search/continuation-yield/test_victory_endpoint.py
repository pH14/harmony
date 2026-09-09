import unittest

from victory_endpoint import victory_endpoint


class VictoryBudget(unittest.TestCase):
    def test_drained_post_budget_victory_is_not_a_budgeted_hit(self):
        result = victory_endpoint({"frames_emulated": 107, "frames_to_first_victory": 103}, 100)
        self.assertIs(result["hit_by_budget"], False)
        self.assertEqual(result["restricted_cost_interval"], [100, 100])

    def test_success_before_search_stop_supplies_complete_event_evidence(self):
        result = victory_endpoint({"frames_emulated": 73, "frames_to_first_victory": 70}, 100)
        self.assertFalse(result["observed_full_budget"])
        self.assertIs(result["hit_by_budget"], True)
        self.assertEqual(result["restricted_cost_interval"], [70, 70])

    def test_incomplete_nonattainment_cannot_be_scored_as_failure(self):
        result = victory_endpoint({"frames_emulated": 73, "frames_to_first_victory": None}, 100)
        self.assertIsNone(result["hit_by_budget"])
        self.assertIsNone(result["restricted_cost_interval"])


if __name__ == "__main__":
    unittest.main()
