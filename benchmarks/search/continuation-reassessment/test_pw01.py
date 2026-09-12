#!/usr/bin/env python3
"""Real serialized inputs, explicitly synthetic new IDs, and planted failures."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from run_pw01 import main
from score_pw01 import SELECTOR, MIXTURES, cell_gate, first_word
from test_pg02_audit import native_fixture


class ProductiveWordQualificationTests(unittest.TestCase):
    def test_cancelled_allocation_refuses_execution_before_host_or_process_access(self):
        registration = Path(__file__).with_name('pw01-registration.json')
        with patch('run_pw01.socket.gethostname', side_effect=AssertionError('host accessed')):
            with self.assertRaisesRegex(ValueError, 'cancelled or not explicitly executable'):
                main(registration)

    def fixture(self, mixture):
        args = native_fixture('progress')
        args[2]['mixture_policy'] = mixture
        args[0]['selector'] = args[2]['parent_scheduler'] = SELECTOR
        if mixture != 'alphabet_only':
            args[0]['mixture'] = mixture
        return args

    def test_real_progress_schema_with_explicitly_synthetic_new_policy_ids(self):
        for mixture in MIXTURES:
            self.assertTrue(cell_gate(*self.fixture(mixture)), mixture)
        self.assertNotIn('mixture', self.fixture('alphabet_only')[0])

    def test_planted_identity_replay_cost_horizon_and_witness_failures(self):
        for mixture in MIXTURES:
            for defect in ['replay', 'cost', 'horizon', 'retention', 'selector',
                           'mixture', 'unqualified_mixture', 'dead', 'receipt']:
                args = self.fixture(mixture)
                if defect == 'replay': args[1]['full_campaign_replay'] = False
                if defect == 'cost': args[1]['physical_frames']['total'] -= 929
                if defect == 'horizon': args[0]['frames'] = 1000001
                if defect == 'retention': args[2]['slot_retention'] = 'unknown'
                if defect == 'selector': args[0]['selector'] = args[2]['parent_scheduler'] = 'room_cell_uniform_128'
                if defect == 'mixture': args[2]['mixture_policy'] = 'unknown'
                if defect == 'unqualified_mixture': args[0]['mixture'] = args[2]['mixture_policy'] = 'alphabet_continuation_v1'
                if defect == 'dead': args[1]['witness']['dead'] = True
                if defect == 'receipt': args[1]['cost']['search_engine']['targets_closed'] -= 1
                self.assertFalse(cell_gate(*args), (mixture, defect))

    def test_first_difference_must_isolate_action_reuse(self):
        header = dict(mixture_policy='alphabet_scoped_progress_fresh_control_v1', seed=7)
        other = dict(header, mixture_policy='alphabet_scoped_progress_reuse_v1')
        job = dict(event='job', sequence=4, worker=0, parent_id=3, mutation_seed=9,
                   selector=dict(path='continuation'), mixture_weight=0, splice_weight=255,
                   splice=dict(donor_id=2, leaf_id=3, tail_postcard=[1, 0]))
        for defect in ['none', 'same_word', 'parent', 'seed', 'selector', 'schedule',
                       'donor', 'leaf', 'prior_difference']:
            a, b = copy.deepcopy(job), copy.deepcopy(job)
            b['splice']['tail_postcard'] = [1, 1]
            if defect == 'same_word': b['splice']['tail_postcard'] = [1, 0]
            if defect == 'parent': b['parent_id'] += 1
            if defect == 'seed': b['mutation_seed'] += 1
            if defect == 'selector': a['selector']['path'] = b['selector']['path'] = 'uniform'
            if defect == 'schedule': a['sequence'] = b['sequence'] = 5
            if defect == 'donor': b['splice']['donor_id'] += 1
            if defect == 'leaf': a['splice']['leaf_id'] = b['splice']['leaf_id'] = 8
            if defect == 'prior_difference': b['sequence'] += 4
            with tempfile.TemporaryDirectory() as d:
                left, right = Path(d) / 'a', Path(d) / 'b'
                for path, rows in [(left, [header, a]), (right, [other, b])]:
                    path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
                self.assertEqual(first_word(left, right)['activated'], defect == 'none', defect)


if __name__ == '__main__':
    unittest.main()
