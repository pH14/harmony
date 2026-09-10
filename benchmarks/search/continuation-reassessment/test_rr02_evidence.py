import copy
import gzip
import json
from pathlib import Path
import unittest

from run_rr02 import known_frames
from score_rr02 import compare_root


class ReconstructionEvidence(unittest.TestCase):
    def test_failed_and_interrupted_work_uses_available_counters_once(self):
        self.assertEqual(known_frames(dict(result=dict(status='complete',
            known_inspector_setup_frames=929, known_reconstruction_frames=117875,
            known_replay_frames=865))), 119669)
        cost = dict(inspector_setup_frames=929, reconstruction_frames=1234,
                    verified_replay_frames=None, replay_started=True)
        self.assertEqual(known_frames(dict(failure=dict(cost=cost))), 2163)
        self.assertEqual(known_frames(dict(last_cost_receipt=cost)), 2163)
        cost['verified_replay_frames'] = 865
        self.assertEqual(known_frames(dict(failure=dict(cost=cost), last_cost_receipt=cost)), 3028)
        # No receipt establishes no positive lower bound; total work stays unknown.
        self.assertEqual(known_frames({}), 0)

    def test_actual_saved_root_and_planted_correspondence_mismatches(self):
        p = Path(__file__).resolve().parents[1] / 'endpoint-encounter/ap01-output/root-snapshot.json.gz'
        original = json.loads(gzip.decompress(p.read_bytes()))
        old = bytes(original['emulator_state'][48:112]).decode('ascii')
        new = 'b' * 64
        # Synthetic comparison witness only; never supplied to an emulator.
        actual = copy.deepcopy(original)
        actual['emulator_state'][48:112] = list(new.encode('ascii'))
        compare_root(original, actual, old, new)
        for index in (0, 8, 48, 112, 120, len(actual['emulator_state']) - 1):
            bad = copy.deepcopy(actual)
            bad['emulator_state'][index] ^= 1
            with self.assertRaises(AssertionError):
                compare_root(original, bad, old, new)
        bad = copy.deepcopy(actual)
        bad['observation']['decoded']['health'] += 1
        with self.assertRaises(AssertionError):
            compare_root(original, bad, old, new)
        with self.assertRaises(AssertionError):
            compare_root(original, actual, new, old)


if __name__ == '__main__':
    unittest.main()
