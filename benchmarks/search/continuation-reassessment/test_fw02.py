"""Exercise the source-corrected terminal contract on the actual FW01 output."""
import copy
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from run_fw01 import check_result as original_check
from run_fw02 import check_result


class CorrectedWitnessGateTests(unittest.TestCase):
    def setUp(self):
        root = Path(__file__).resolve().parent
        self.reg = json.loads((root / 'fw01-registration.json').read_text())
        self.case = self.reg['cases'][0]
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name) / 'native'
        shutil.copytree(root / 'fw01-output/fw01-control-s3', self.folder)
        self.summary = json.loads((self.folder / 'summary.json').read_text())

    def check(self):
        for name, value in [('summary.json', self.summary),
                            ('ordinary.json', self.summary['ordinary']),
                            ('first.json', self.summary['one_frame'])]:
            (self.folder / name).write_text(json.dumps(value))
        return check_result(self.reg, self.case, self.folder)

    def test_actual_nonplaying_endpoint_is_nonterminal_and_fully_replayed(self):
        self.assertEqual(self.summary['one_frame']['endpoint']['mode'], 9)
        self.assertEqual(self.check()['known_auxiliary_frames'], 234966)

    def test_original_frozen_gate_remains_failed(self):
        with self.assertRaises(AssertionError):
            original_check(self.reg, self.case, self.folder)

    def test_terminal_states_still_fail(self):
        baseline = copy.deepcopy(self.summary)
        for change in ({'health': 0}, {'health': 9999}, {'ending': True}):
            with self.subTest(change=change):
                self.summary = copy.deepcopy(baseline)
                for mode in ('ordinary', 'one_frame'):
                    self.summary[mode]['endpoint'].update(change)
                with self.assertRaises(AssertionError):
                    self.check()

    def test_missing_bombs_still_fails(self):
        for mode in ('ordinary', 'one_frame'):
            self.summary[mode]['endpoint']['equipment'] &= ~0x01
        with self.assertRaises(AssertionError):
            self.check()

    def test_incomplete_replays_still_fail(self):
        self.summary['verified_replays'] = 2
        with self.assertRaises(AssertionError):
            self.check()

    def test_wrong_input_still_fails(self):
        self.summary['input_sha256'] = '0' * 64
        with self.assertRaises(AssertionError):
            self.check()


if __name__ == '__main__':
    unittest.main()
