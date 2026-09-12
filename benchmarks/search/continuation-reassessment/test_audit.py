"""Saved-data recomputation and planted invalid milestone evidence; no emulation."""
import gzip
import json
from pathlib import Path
import tempfile
import unittest

from audit import HERE, audit, raw


class HistoricalAuditTests(unittest.TestCase):
    def test_saved_audit_recomputes_without_promoting_censored_results(self):
        actual = audit(HERE / 'evidence')
        self.assertEqual(actual, json.loads((HERE / 'analysis.json').read_text()))
        self.assertEqual(actual['energy_loss_but_bombs_win_seeds'], [3, 5])
        self.assertTrue(all(p['strict_win'] for p in actual['comparisons']['bombs']['pairs']))
        for endpoint in ('long_beam', 'ridley_defeated', 'kraid_defeated'):
            self.assertIsNone(actual['comparisons'][endpoint]['all_attained_mean_cost_ratio_interval'])

    def test_milestone_cannot_precede_its_recorded_admission(self):
        relative = Path('metroid-long-control-012/metroid-full-s3-w4-m8192')
        source = HERE / 'evidence' / relative
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            cell = root / relative
            (cell / 'campaign').mkdir(parents=True)
            for name in ('summary.json', 'campaign/result.json'):
                (cell / name).write_bytes(raw(source / name))
            with gzip.open(source / 'campaign/progress.jsonl.gz', 'rt') as handle:
                first = json.loads(next(handle))
            first['workload_diagnostics']['named_progress']['first_seen']['bombs'] = {
                'execution': 1, 'route_action_end_frame': 1,
            }
            (cell / 'campaign/progress.jsonl').write_text(json.dumps(first) + '\n')
            with self.assertRaises(AssertionError):
                audit(root)


if __name__ == '__main__':
    unittest.main()
