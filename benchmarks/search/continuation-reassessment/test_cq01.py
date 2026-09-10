"""Verify native continuation coverage, rather than trusting a completed flag."""
import gzip
import json
from pathlib import Path
import tempfile
import unittest

from verify_cq01 import verify

ROOT = Path(__file__).resolve().parent


class ContinuationQualificationTests(unittest.TestCase):
    def test_saved_native_events_recompute(self):
        actual = verify(ROOT, ROOT / 'cq01-output')
        self.assertEqual(actual, json.loads((ROOT / 'cq01-analysis.json').read_text()))
        self.assertTrue(all(r['continuation_jobs'] == 190 for r in actual['records']))
        ledger = json.loads((ROOT / 'ledger-after-cq01.json').read_text())
        prior = json.loads((ROOT / 'ledger-after-fw02.json').read_text())
        self.assertEqual(ledger['known_auxiliary_frames'], actual['known_auxiliary_frames'])
        self.assertEqual(ledger['cumulative_since_user_resumption_known_auxiliary_frames'],
                         prior['cumulative_since_user_resumption_known_auxiliary_frames'] + actual['known_auxiliary_frames'])

    def test_a_completed_flag_cannot_hide_absent_continuation_dispatch(self):
        source = ROOT / 'cq01-output'
        changed = Path('msr1/one-slot/metroid-full-s2026090902-w4-m8192/campaign/stream.jsonl.gz')
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp)
            for p in source.rglob('*'):
                if p.is_file() and p.relative_to(source) != changed:
                    dest = output / p.relative_to(source)
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    dest.symlink_to(p)
            rows = [json.loads(line) for line in gzip.decompress((source / changed).read_bytes()).splitlines()]
            for row in rows[1:]:
                if row.get('selector', {}).get('path') == 'continuation':
                    row['selector']['path'] = 'room_cell_uniform'
            (output / changed).write_bytes(gzip.compress(('\n'.join(map(json.dumps, rows)) + '\n').encode(), mtime=0))
            with self.assertRaisesRegex(AssertionError, 'exercise bounded continuation'):
                verify(ROOT, output)


if __name__ == '__main__':
    unittest.main()
