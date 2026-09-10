"""The allocation gate preserves censoring, interval uncertainty and resources."""
import copy
import unittest
from score_retry_screen import assess


def pair(candidate, control, complete=True):
    return {arm: {'restricted_interval': interval, 'fixed_horizon_completed': complete,
                  'process': {'wall_seconds': 10, 'cpu_seconds': 10}}
            for arm, interval in [('candidate', candidate), ('control', control)]}


class RetryScreenGate(unittest.TestCase):
    def test_two_censored_ties_stop_without_remaining_pairs(self):
        p = pair([250000, 250000], [250000, 250000])
        self.assertEqual(assess([p])['decision'], 'continue')
        self.assertEqual(assess([p, p])['decision'], 'stop_futility')

    def test_incomplete_is_not_a_completed_negative(self):
        self.assertEqual(assess([pair([250000]*2, [250000]*2, False)])['decision'],
                         'stop_incomplete_measurement')

    def test_overlap_and_resource_cost_prevent_promotion(self):
        p = pair([60, 70], [100, 110])
        self.assertEqual(assess([p]*4)['decision'], 'pass_conditional_gate')
        overlap = pair([60, 105], [100, 110])
        self.assertEqual(assess([overlap]*4)['decision'], 'stop_futility')
        costly = copy.deepcopy(p)
        costly['candidate']['process']['cpu_seconds'] = 13
        self.assertEqual(assess([costly]*4)['decision'], 'stop_failed_gate')
