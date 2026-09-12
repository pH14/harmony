"""Recompute completed native evidence and the old corrected-control prefix."""
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from audit_c01 import audit
from verify_fw02 import verify

HERE = Path(__file__).resolve().parent


class ClosureTests(unittest.TestCase):
    def test_all_six_witnesses_recompute_with_first_failure_preserved(self):
        self.assertEqual(verify(HERE), json.loads((HERE / 'ledger-after-fw02.json').read_text()))

    def test_a_missing_completed_case_prevents_pass(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for p in HERE.iterdir():
                if p.name != 'fw02-output':
                    (root / p.name).symlink_to(p, target_is_directory=p.is_dir())
            shutil.copytree(HERE / 'fw02-output', root / 'fw02-output')
            path = root / 'fw02-output/results.json'
            data = json.loads(path.read_text())
            data['records'].pop()
            path.write_text(json.dumps(data))
            with self.assertRaises(AssertionError):
                verify(root)

    def test_corrected_control_arrivals_preserve_incomplete_horizon(self):
        actual = audit(HERE / 'c01-evidence')
        self.assertEqual(actual, json.loads((HERE / 'c01-arrivals.json').read_text()))
        self.assertEqual(actual['endpoints']['bombs']['arrival_frames_interval'], [113325933, 113337939])
        self.assertEqual(actual['endpoints']['ridley_defeated']['right_censored_after_frames'], 240942610)


if __name__ == '__main__':
    unittest.main()
