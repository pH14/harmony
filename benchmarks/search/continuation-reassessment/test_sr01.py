#!/usr/bin/env python3
"""Actual serialized arm contracts plus planted scorer/activation failures."""
import copy
import gzip
import json
from pathlib import Path
import tempfile
import unittest
from score_sr01 import campaign_identity, cell_gate, first_return
from test_pg02_audit import native_fixture

ROOT = Path(__file__).resolve().parent
PREFIX = 'room_cell_uniform_128_energy_progress_cheapest_scoped_return_'

class QualificationTests(unittest.TestCase):
    def test_real_serialized_all_arm_identities_including_omitted_default(self):
        for arm in ['ordinary', 'progress', 'capacity']:
            q = json.loads((ROOT / ('pg01-' + arm + '-request.json')).read_text())
            out = ROOT / ('pg01-output/' + arm)
            def read(name):
                raw = gzip.decompress((out / (name + '.gz')).read_bytes())
                return json.loads(raw.splitlines()[-1]) if name.endswith('jsonl') else json.loads(raw)
            result, campaign, progress = read('result.json'), read('campaign.json'), read('progress.jsonl')
            self.assertEqual('slot_retention' in campaign, arm != 'ordinary')
            self.assertTrue(campaign_identity(q, result, campaign, progress))
            for mode in ['control', 'half']:
                # New IDs are synthetic schema tests; native activation remains required.
                request, report = copy.deepcopy(q), copy.deepcopy(campaign)
                request['selector'] = report['parent_scheduler'] = PREFIX + mode + '_v1:3,6,12,2'
                self.assertTrue(campaign_identity(request, result, report, progress))
                report['parent_scheduler'] += '-wrong'
                self.assertFalse(campaign_identity(request, result, report, progress))

    def test_full_actual_metered_controls_and_planted_failures(self):
        for arm in ['ordinary', 'progress']:
            self.assertTrue(cell_gate(*native_fixture(arm)))
            for defect in ['replay', 'cost', 'horizon', 'retention', 'selector']:
                args = native_fixture(arm)
                if defect == 'replay': args[1]['full_campaign_replay'] = False
                if defect == 'cost': args[1]['physical_frames']['total'] -= 929
                if defect == 'horizon': args[0]['frames'] = 1000001
                if defect == 'retention': args[2]['slot_retention'] = 'unknown'
                if defect == 'selector': args[0]['selector'] = PREFIX + 'half_v1:3,6,12,2'
                self.assertFalse(cell_gate(*args), (arm, defect))

    def test_activation_requires_first_difference_to_be_a_coupled_parent_change(self):
        header = dict(parent_scheduler=PREFIX + 'control_v1:3,6,12,2', seed=7)
        other_header = dict(header, parent_scheduler=PREFIX + 'half_v1:3,6,12,2')
        job = dict(event='job', sequence=1, worker=0, parent_id=3, mutation_seed=9,
                   selector=dict(path='room_cell_uniform'), frames=5)
        for defect in ['none', 'same_parent', 'seed', 'uniform', 'prior_difference']:
            a, b = copy.deepcopy(job), copy.deepcopy(job)
            b['parent_id'] = 4
            if defect == 'same_parent': b['parent_id'] = 3
            if defect == 'seed': b['mutation_seed'] = 10
            if defect == 'uniform': a['selector']['path'] = b['selector']['path'] = 'uniform'
            if defect == 'prior_difference': b['sequence'] = 2
            with tempfile.TemporaryDirectory() as d:
                left, right = Path(d) / 'a', Path(d) / 'b'
                for path, rows in [(left, [header, a]), (right, [other_header, b])]:
                    path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
                self.assertEqual(first_return(left, right)['activated'], defect == 'none')

if __name__ == '__main__': unittest.main()
