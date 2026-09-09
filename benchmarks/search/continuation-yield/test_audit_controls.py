import unittest

from audit_controls import milestone_interval


def row(frames, execution, arrival=None):
    seen = None if arrival is None else {"execution": arrival}
    return {"frames_emulated": frames, "executions": execution,
            "workload_diagnostics": {"named_progress": {"first_seen": {"missile_capacity": seen}}}}


class CensoringEvidence(unittest.TestCase):
    def test_never_attained_is_budgeted_failure_only_after_budget_observed(self):
        short = milestone_interval([row(40, 1)], 100)
        full = milestone_interval([row(40, 1), row(101, 2)], 100)
        self.assertIsNone(short["hit_by_budget"])
        self.assertIsNone(short["restricted_cost_interval"])
        self.assertIs(full["hit_by_budget"], False)
        self.assertEqual(full["restricted_cost_interval"], [100, 100])

    def test_arrival_crossing_cap_retains_uncertain_hit(self):
        result = milestone_interval([row(90, 1), row(110, 3, 2)], 100)
        self.assertIsNone(result["hit_by_budget"])
        self.assertEqual(result["restricted_cost_interval"], [90, 100])

    def test_post_budget_arrival_does_not_count_as_success(self):
        result = milestone_interval([row(101, 1), row(120, 3, 2)], 100)
        self.assertIs(result["hit_by_budget"], False)
        self.assertEqual(result["restricted_cost_interval"], [100, 100])

    def test_success_interval_survives_later_cumulative_reports(self):
        result = milestone_interval([row(10, 1), row(20, 3, 2), row(100, 5, 2)], 100)
        self.assertIs(result["hit_by_budget"], True)
        self.assertEqual(result["restricted_cost_interval"], [10, 20])

    def test_retroactive_or_nonmonotone_observations_fail_closed(self):
        with self.assertRaises(AssertionError):
            milestone_interval([row(20, 3), row(30, 4, 2)])
        with self.assertRaises(AssertionError):
            milestone_interval([row(20, 3), row(10, 4)])


if __name__ == "__main__":
    unittest.main()
