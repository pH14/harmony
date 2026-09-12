"""Planted scorer cases derived from D01; these are not P01 measurements."""
import copy
import gzip
import json
from pathlib import Path
import tempfile
import unittest

from score_persistence import score


class PersistenceScorerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.source = Path(__file__).parent
        self.regpath = self.root / 'p01-registration.json'
        self.regpath.write_bytes((self.source / self.regpath.name).read_bytes())
        self.reg = json.loads(self.regpath.read_text())
        self.evidence = self.root / 'synthetic-evidence'
        self.evidence.mkdir()
        self.panels = []
        for i, spec in enumerate(self.reg['panels']):
            path = self.source / spec['registration']
            (self.root / path.name).write_bytes(path.read_bytes())
            sub = json.loads(path.read_text())
            old = json.loads(gzip.decompress((self.source / f'd01-triplet-{i % 2}-results.json.gz').read_bytes()))
            by_arm = {r['id'].rsplit('-', 1)[1]: r for r in old['records']}
            panel = {'registration_sha256': spec['registration_sha256'],
                     'execution_complete': True, 'allocation_stop': None, 'records': []}
            for cell in sub['cells']:
                arm = cell['id'].rsplit('-', 1)[1]
                row = copy.deepcopy(by_arm[arm])
                row['id'] = cell['id']
                # Deliberately synthetic identity: no copied outcome is real P01 evidence.
                row['summary']['identity'] = cell['expected_identity']
                panel['records'].append(row)
            self.panels.append(panel)

    def save(self, count=4):
        for spec, panel in zip(self.reg['panels'][:count], self.panels[:count]):
            (self.evidence / (spec['id'] + '-results.json')).write_text(json.dumps(panel))

    def test_four_pairs_can_pass_with_eight_cells(self):
        self.save()
        result = score(self.regpath, self.evidence)
        self.assertEqual(result['decision'], 'earns_transfer_and_depth_decision')
        self.assertEqual(result['screen']['strict_wins'], 4)
        self.assertEqual(len(result['cells']), 8)

    def test_two_positive_pairs_do_not_pass(self):
        self.save(2)
        result = score(self.regpath, self.evidence)
        self.assertEqual(result['decision'], 'continue_registered_panel')
        self.assertEqual(result['screen']['completed_pairs'], 2)

    def test_two_censored_ties_stop(self):
        for panel in self.panels[:2]:
            ordinary = next(r for r in panel['records'] if r['id'].endswith('-ordinary'))
            whole = next(r for r in panel['records'] if r['id'].endswith('-whole'))
            identity = whole['summary']['identity']
            whole['summary'] = copy.deepcopy(ordinary['summary'])
            whole['summary']['identity'] = identity
            whole['endpoint_evidence'] = copy.deepcopy(ordinary['endpoint_evidence'])
        self.save(2)
        result = score(self.regpath, self.evidence)
        self.assertEqual(result['decision'], 'stop_persistence_line')
        self.assertEqual(result['screen']['strict_wins'], 0)

    def test_identity_corruption_is_rejected(self):
        self.panels[0]['records'][0]['summary']['identity']['seed'] += 1
        self.save(1)
        with self.assertRaises(AssertionError):
            score(self.regpath, self.evidence)

    def test_failed_cell_preserves_available_cost(self):
        panel = self.panels[0]
        row = panel['records'][0]
        panel['records'] = [row]
        panel['allocation_stop'] = 'planted measurement failure'
        panel['execution_complete'] = False
        row['checks_passed'] = False
        row['failure'] = 'planted failure after summary'
        self.save(1)
        result = score(self.regpath, self.evidence)
        self.assertEqual(result['decision'], 'stop_measurement_failure')
        self.assertEqual(result['actual_admitted_search_frames'], row['summary']['result']['frames_emulated'])
        self.assertIsNone(result['cells'][0]['known_auxiliary_frames'])
        self.assertEqual(len(result['incomplete_work']), 1)


if __name__ == '__main__':
    unittest.main()
