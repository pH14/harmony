"""Recompute closed evidence and reject corrupted development measurements."""
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from score_ed01 import score
from verify_eq01 import verify


ROOT = Path(__file__).resolve().parent


class ClosedEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.evidence = Path(self.tmp.name)
        for path in (ROOT / 'ed01-output').glob('ed01-pair-*-results.json.gz'):
            (self.evidence / path.stem).write_bytes(gzip.decompress(path.read_bytes()))

    def test_lossless_ed01_archive_and_complete_inventory(self):
        output = ROOT / 'ed01-output'
        manifest = json.loads((output / 'manifest.json').read_text())
        self.assertEqual({str(p.relative_to(output)) for p in output.rglob('*.gz')},
                         {entry['file'] + '.gz' for entry in manifest})
        for entry in manifest:
            raw = gzip.decompress((output / (entry['file'] + '.gz')).read_bytes())
            self.assertEqual(len(raw), entry['bytes'], entry['file'])
            self.assertEqual(hashlib.sha256(raw).hexdigest(), entry['sha256'], entry['file'])

    def test_eq01_recomputes(self):
        self.assertEqual(verify(ROOT, ROOT / 'eq01-output'),
                         json.loads((ROOT / 'eq01-analysis.json').read_text()))

    def test_ed01_recomputes_with_futility_and_closed_costs(self):
        actual = json.loads(json.dumps(score(ROOT / 'ed01-registration.json', self.evidence)))
        saved = json.loads((ROOT / 'ed01-analysis.json').read_text())
        for result in (actual, saved):
            result.pop('recorded_utc')
        self.assertEqual(actual, saved)
        self.assertEqual(actual['decision'], 'stop_entry_cutoff_line')
        self.assertEqual(actual['screen']['completed_pairs'], 2)
        self.assertEqual(actual['screen']['strict_wins'], 0)
        self.assertIsNone(actual['screen']['candidate_to_control_resource_ratios'])
        ledger = json.loads((ROOT / 'ledger-after-ed01.json').read_text())
        prior = json.loads((ROOT / 'ledger-after-eq01.json').read_text())
        for field, name in [('prior_ledger_sha256', 'ledger-after-eq01.json'),
                            ('registration_sha256', 'ed01-registration.json'),
                            ('analysis_sha256', 'ed01-analysis.json')]:
            self.assertEqual(ledger[field], hashlib.sha256((ROOT / name).read_bytes()).hexdigest())
        self.assertEqual(actual['actual_admitted_search_frames'], 190_164_603)
        self.assertEqual(actual['known_auxiliary_frames'], 629_852)
        for field, score_field in [('admitted_search_frames', 'actual_admitted_search_frames'),
                                   ('known_auxiliary_frames', 'known_auxiliary_frames')]:
            self.assertEqual(ledger[field], actual[score_field])
            cumulative = 'cumulative_since_user_resumption_' + field
            self.assertEqual(ledger[cumulative], prior[cumulative] + ledger[field])
        self.assertEqual(sum(cell['admitted_stop_drain'] for cell in ledger['cells']), 3_780)
        self.assertEqual(ledger['unrun_pairs'], [2, 3])

    def test_checked_flag_does_not_hide_changed_selector(self):
        path = self.evidence / 'ed01-pair-0-results.json'
        panel = json.loads(path.read_text())
        panel['records'][0]['summary']['identity']['selector'] = 'changed'
        path.write_text(json.dumps(panel))
        with self.assertRaises(AssertionError):
            score(ROOT / 'ed01-registration.json', self.evidence)

    def test_attainment_requires_saved_witness(self):
        path = self.evidence / 'ed01-pair-0-results.json'
        panel = json.loads(path.read_text())
        del panel['records'][0]['summary']['result']['milestone_witnesses']['energy_tank']
        path.write_text(json.dumps(panel))
        with self.assertRaises((AssertionError, KeyError)):
            score(ROOT / 'ed01-registration.json', self.evidence)


if __name__ == '__main__':
    unittest.main()
