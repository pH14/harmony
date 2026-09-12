#!/usr/bin/env python3
"""Check the actual earlier paused-reader schema and planted failures offline."""
import gzip
import json
from pathlib import Path
import unittest
from read_pq01 import paused_gate
from analyze_pq01 import qualified

ROOT = Path(__file__).resolve().parent.parent / 'endpoint-encounter'


def fixture():
    q = json.loads((ROOT / 'ci01-request.json').read_text())
    def read(name):
        return json.loads(gzip.decompress((ROOT / 'ci01-output' / (name + '.json.gz')).read_bytes()))
    return q, read('ci01-inventory'), read('ci01-context')


class PausedGateTests(unittest.TestCase):
    def test_prior_classification_and_unknown_hp_or_defeat_are_distinct(self):
        rows = fixture()[2]['entries']
        known = [qualified(row)[0] for row in rows]
        self.assertEqual(min(hp for hp in known if hp is not None), 129)
        row = json.loads(json.dumps(next(row for row in rows if qualified(row)[0] == 140)))
        row['context']['memory']['enemies'][0]['hit_points'] = 255
        self.assertEqual(qualified(row), (None, 'hp_unavailable'))
        row['context']['memory']['enemies'][0]['hit_points'] = 0
        self.assertEqual(qualified(row), (0, 'root_scope'))
        row['context']['memory']['ridley_status'] = 2
        self.assertEqual(qualified(row), (None, 'defeated'))

    def test_actual_previous_inspection_matches_inventory(self):
        self.assertTrue(paused_gate(*fixture()))

    def test_clock_root_inventory_or_completion_changes_fail(self):
        for field, value in [('direct_physical_frames', 930), ('setup_frames', 0),
                             ('continuation_frames', 1), ('verified_restores', 0),
                             ('positive_control', {}), ('origin_sha256', 'wrong')]:
            q, inv, report = fixture()
            report[field] = value
            self.assertFalse(paused_gate(q, inv, report), field)
        q, inv, report = fixture()
        report['entries'][0]['id'] += 1
        self.assertFalse(paused_gate(q, inv, report))
        q, inv, report = fixture()
        report['entries'].pop()
        self.assertFalse(paused_gate(q, inv, report))


if __name__ == '__main__':
    unittest.main()
