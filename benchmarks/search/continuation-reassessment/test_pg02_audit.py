#!/usr/bin/env python3
"""Exercise actual serialized control data, including omitted optional fields."""
import copy
import gzip
import json
from pathlib import Path
import unittest
from audit_pg02 import compatible_cell_gate
from score_pg02 import cell_gate

ROOT = Path(__file__).resolve().parent


def native_fixture(arm):
    q = json.loads((ROOT / ('p0-' + arm + '-pg02-request.json')).read_text())
    out = ROOT / ('pg02-output/p0-' + arm + '/campaign')
    def read(name):
        raw = gzip.decompress((out / (name + '.gz')).read_bytes())
        return json.loads(raw.splitlines()[-1]) if name.endswith('jsonl') else json.loads(raw)
    return q, read('result.json'), read('campaign.json'), read('usage.json'), read('progress.jsonl'), 2880


class SerializedControlTests(unittest.TestCase):
    def test_all_three_original_qualification_reports_use_the_documented_schema(self):
        for arm in ['ordinary', 'progress', 'capacity']:
            q = json.loads((ROOT / ('pg01-' + arm + '-request.json')).read_text())
            campaign = json.loads(gzip.decompress((ROOT / ('pg01-output/' + arm + '/campaign.json.gz')).read_bytes()))
            self.assertEqual(campaign.get('slot_retention'), q['slot_retention'])
            self.assertEqual('slot_retention' in campaign, arm != 'ordinary')

    def test_frozen_ordinary_bug_reproduces_without_emulation(self):
        args = native_fixture('ordinary')
        self.assertNotIn('slot_retention', args[2])
        with self.assertRaisesRegex(KeyError, 'slot_retention'):
            cell_gate(*args)
        original = copy.deepcopy(args[2])
        self.assertTrue(compatible_cell_gate(*args))
        self.assertEqual(args[2], original)

    def test_progress_data_passes_both_checks(self):
        args = native_fixture('progress')
        self.assertTrue(cell_gate(*args))
        self.assertTrue(compatible_cell_gate(*args))
        del args[2]['slot_retention']
        self.assertFalse(compatible_cell_gate(*args))

    def test_normalization_does_not_relax_outcomes_resources_or_replay(self):
        for defect in ['replay', 'work', 'horizon', 'wrong_policy']:
            args = native_fixture('ordinary')
            if defect == 'replay':
                args[1]['full_campaign_replay'] = False
            elif defect == 'work':
                args[1]['physical_frames']['total'] -= 929
            elif defect == 'horizon':
                args[0]['frames'] += 1000000
            else:
                args[2]['slot_retention'] = 'resource_guarded_progress_2_v1'
            self.assertFalse(compatible_cell_gate(*args), defect)


if __name__ == '__main__':
    unittest.main()
