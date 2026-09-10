#!/usr/bin/env python3
"""Planted errors for PM01's fixed physical-accounting identity."""
import copy
import gzip
import json
from pathlib import Path
import unittest
from run_pm01 import accounting_gate

ROOT = Path(__file__).resolve().parent


def fixture():
    q = json.loads((ROOT / 'pg01-progress-request.json').read_text())
    result = json.loads(gzip.decompress((ROOT / 'pg01-output/progress/result.json.gz').read_bytes()))
    cost = result['cost']
    physical = dict(direct_helpers=cost['direct_physical_frames'], search_outside_admission=4645,
                    replay_outside_admission=929)
    for phase, count, field in [('search_engine', 5, 'admitted_search_frames'),
                                ('replay_engine', 1, 'campaign_replay_admitted_frames')]:
        cost[phase] = dict(construction_attempts=count, targets_created=count, targets_closed=count,
                           constructor_frames=count * 929, closed_post_constructor_frames=cost[field], invalid_counter=False)
        physical[phase] = count * 929 + cost[field]
    physical['total'] = physical['direct_helpers'] + physical['search_engine'] + physical['replay_engine']
    result['physical_frames'] = physical
    result['format'] = 'metroid-archive-challenge-physical-result-v2'
    return q, result


class AccountingTests(unittest.TestCase):
    def test_fixed_pg01_prediction(self):
        q, result = fixture()
        self.assertEqual(result['physical_frames']['total'], 983632)
        self.assertTrue(accounting_gate(q, result))

    def test_failed_or_live_target_and_unexplained_work_fail(self):
        q, result = fixture()
        for field, value in [('targets_closed', 4), ('construction_attempts', 6), ('invalid_counter', True),
                              ('closed_post_constructor_frames', 250321)]:
            changed = copy.deepcopy(result)
            changed['cost']['search_engine'][field] = value
            self.assertFalse(accounting_gate(q, changed))

    def test_omitted_or_double_counted_setup_fails(self):
        q, result = fixture()
        for delta in [-5574, 5574]:
            changed = copy.deepcopy(result)
            changed['physical_frames']['total'] += delta
            self.assertFalse(accounting_gate(q, changed))
        result['physical_frames']['replay_outside_admission'] = 0
        self.assertFalse(accounting_gate(q, result))


if __name__ == '__main__':
    unittest.main()
