#!/usr/bin/env python3
"""Planted failures for PG01's prospective qualification gate; no emulator."""
import copy
import unittest
from run_pg01 import qualified


class QualificationTests(unittest.TestCase):
    def fixture(self, optional=True):
        cell = dict(require_alternate=optional, max_drain_frames=2880)
        request = dict(expected_snapshot_sha256='root', expected_retention_progress={'scope': 7, 'value': 114},
                       slot_retention='resource_guarded_progress_2_v1' if optional else None,
                       direct_frame_limit=2000000, frames=250000)
        cost = dict(admitted_search_frames=250121, campaign_replay_admitted_frames=250121,
                    direct_physical_frames=500000)
        result = dict(complete=True, full_campaign_replay=True, root_local_witness_replays=2,
                      complete_prefix_witness_replays=2, frames=250121, executions=2000,
                      cost=cost, root={'snapshot_sha256': 'root', 'qualified_retention_progress': request['expected_retention_progress']})
        campaign = dict(slot_retention=request['slot_retention'])
        usage = dict(completed_execution=True, cost=copy.deepcopy(cost))
        progress = dict(executions=2000, frames_emulated=250121,
                        retention_diagnostics={'alternative_admissions': 2 if optional else 0})
        return [cell, request, result, campaign, usage, progress]

    def test_no_boss_defeat_is_required_for_mechanism_qualification(self):
        for optional in [False, True]:
            self.assertTrue(qualified(*self.fixture(optional)))

    def test_silent_nonactivation_or_ordinary_activation_fails(self):
        for optional, count in [(True, 0), (False, 1)]:
            args = self.fixture(optional)
            args[5]['retention_diagnostics']['alternative_admissions'] = count
            self.assertFalse(qualified(*args))

    def test_corrupt_root_policy_replay_or_final_progress_fails(self):
        for position, key, value in [(2, 'full_campaign_replay', False), (3, 'slot_retention', None),
                                     (4, 'completed_execution', False), (5, 'executions', 1999)]:
            args = self.fixture()
            args[position][key] = value
            self.assertFalse(qualified(*args))
        args = self.fixture()
        args[2]['root']['qualified_retention_progress'] = {'scope': 8, 'value': 114}
        self.assertFalse(qualified(*args))

    def test_cost_mismatch_and_excess_frames_fail(self):
        args = self.fixture()
        args[4]['cost']['admitted_search_frames'] -= 1
        self.assertFalse(qualified(*args))
        args = self.fixture()
        args[1]['frames'] = 200000
        self.assertFalse(qualified(*args))


if __name__ == '__main__':
    unittest.main()
