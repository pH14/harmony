"""Finite counterexamples to two-representative coverage claims."""
import unittest

from analyze_survivor_cover import minimum_cover


class FiniteCoverContracts(unittest.TestCase):
    def test_pairwise_difference_can_be_redundant_in_the_union(self):
        self.assertEqual(minimum_cover([{1}, {2}, {1, 2}]), (1, [[2]]))

    def test_each_states_unique_event_requires_all_three(self):
        self.assertEqual(minimum_cover([{0, 1}, {0, 2}, {0, 3}]), (3, [[0, 1, 2]]))

    def test_bad_chosen_pair_does_not_imply_insufficient_capacity(self):
        offered = [{1, 2}, {1}, {3}]
        self.assertNotEqual(offered[1] | offered[2], set().union(*offered))
        self.assertEqual(minimum_cover(offered), (2, [[0, 2]]))

    def test_empty_observations_and_duplicate_coverage(self):
        self.assertEqual(minimum_cover([set(), set(), set()]), (0, [[]]))
        self.assertEqual(minimum_cover([{1}, {1}, {1}]), (1, [[0], [1], [2]]))


if __name__ == "__main__":
    unittest.main()
