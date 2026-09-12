"""Use a prior qualified native result to test the compatibility gate offline."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

from run_fw01 import check_result


class WitnessGateTests(unittest.TestCase):
    def setUp(self):
        source = Path(__file__).resolve().parents[1] / 'depth-transfer/nq02-results.json'
        self.summary = copy.deepcopy(json.loads(source.read_text())['cases'][0]['summary'])
        self.reg = {k: self.summary[k] for k in ('rom_sha256', 'core_sha256')}
        self.case = {'input_sha256': self.summary['input_sha256'],
                     'route_frames': self.summary['one_frame']['route_frames'],
                     'expected_three_pass_frames': 383796}
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)

    def check(self):
        for name, value in [('summary.json', self.summary),
                            ('ordinary.json', self.summary['ordinary']),
                            ('first.json', self.summary['one_frame'])]:
            (self.folder / name).write_text(json.dumps(value))
        (self.folder / 'relevant-frames.jsonl').write_bytes(b'')
        return check_result(self.reg, self.case, self.folder)

    def test_qualified_native_evidence_passes_with_complete_work(self):
        self.assertEqual(self.check()['known_auxiliary_frames'], 383796)

    def test_three_completed_replays_are_required(self):
        self.summary['verified_replays'] = 2
        with self.assertRaises(AssertionError):
            self.check()

    def test_corrected_terminal_endpoint_is_required(self):
        for mode in ('ordinary', 'one_frame'):
            self.summary[mode]['endpoint']['health'] = 9999
        with self.assertRaises(AssertionError):
            self.check()

    def test_living_endpoint_must_still_hold_bombs(self):
        for mode in ('ordinary', 'one_frame'):
            self.summary[mode]['endpoint']['equipment'] &= ~0x01
        with self.assertRaises(AssertionError):
            self.check()


if __name__ == '__main__':
    unittest.main()
