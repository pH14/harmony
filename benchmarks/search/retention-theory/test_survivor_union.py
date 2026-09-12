"""Scientific counterexamples for complete-survivor interpretation."""
import copy
import unittest

from analyze_survivor_union import analyze, coverage
from run_u01 import route_frames, work_bound


class SurvivorUnionContracts(unittest.TestCase):
    def test_genesis_still_reserves_setup_and_every_possible_export(self):
        empty = {"actions": []}
        bound = work_bound([{"candidate_input": empty, "incumbent_input": empty}])
        self.assertEqual(route_frames(empty), 0)
        self.assertEqual(bound["prefix_upper_bound"], 8192)
        self.assertEqual(bound["gain_exports_upper_bound"], 16 * (4096 + 24 * 120))
        self.assertGreater(bound["total_upper_bound"], bound["prefix_upper_bound"] + bound["suffix_upper_bound"])

    def test_other_survivor_covers_pairwise_novelty(self):
        result = coverage({"door"}, [set(), {"door"}], None)
        self.assertEqual(result["pairwise_unique_before_union"], 1)
        self.assertEqual(result["covered_by_other_survivor"], 1)
        self.assertEqual(result["unretained_only"], 0)

    def test_actual_replacement_has_gains_and_losses(self):
        result = coverage({"lost", "shared"}, [{"gained"}, {"shared"}], 0)
        self.assertEqual(result["actual_local_replacement_gained"], 1)
        self.assertEqual(result["actual_local_replacement_lost"], 1)
        self.assertEqual(result["unretained_only"], 1)

    def test_rejected_opportunity_is_not_an_actual_replacement(self):
        result = coverage({"door"}, [set(), set()], None)
        self.assertEqual(result["unretained_only"], 1)
        self.assertEqual(result["actual_local_replacement_gained"], 0)
        self.assertEqual(result["actual_local_replacement_lost"], 0)

    def test_duplicate_replay_and_complete_trial_grid_are_required(self):
        registration = {"trials_per_competition": 1, "competitions": [
            {"source_sample": 0, "execution": 9, "candidate_survivor_index": None}]}
        outcome = {"dead": False, "equipment_gained": 0, "boss_gain": False,
                   "capacity_gain": False, "reached_maps": []}
        rows = [{"stratum": 3, "pair": i, "trial": 0, "candidate_replaces": False,
                 "execution": 9, "discarded": copy.deepcopy(outcome), "survivor": copy.deepcopy(outcome)}
                for i in range(2)]
        self.assertEqual(analyze(registration, rows)["totals"]["survival"]["any_retained_alive"], 1)
        with self.assertRaises(AssertionError):
            analyze(registration, rows[:1])
        rows[1]["discarded"]["dead"] = True
        with self.assertRaises(AssertionError):
            analyze(registration, rows)


if __name__ == "__main__":
    unittest.main()
