"""Native retry qualification must exercise the changed path and preserve bytes."""
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import verify_retry_qualification as verifier

ROOT = Path(__file__).parent
OUTPUT = ROOT / 'rq01-output'


class RetryQualificationEvidence(unittest.TestCase):
    def test_saved_qualification_recomputes(self):
        result = verifier.verify(ROOT, OUTPUT, 'all')
        self.assertEqual(result['known_auxiliary_frames'], 1513890)
        self.assertEqual(result['cells']['rq01-one']['retry_attempts'], 44)

    def test_retry_counter_without_surviving_witness_is_insufficient(self):
        original = verifier.read

        def changed(root, name):
            value = original(root, name)
            if root == OUTPUT / 'rq01-one' and name == 'campaign.json':
                value.pop('first_local_retry')
            return value

        with patch.object(verifier, 'read', changed), self.assertRaises(AssertionError):
            verifier.verify(ROOT, OUTPUT, 'all')

    def test_checkpoint_identity_is_checked_for_control_and_buffers(self):
        original = verifier.raw
        for target in ('rq01-control', 'rq01-two'):
            with self.subTest(target=target):
                def changed(root, name):
                    value = original(root, name)
                    return value + b'corrupt' if root == OUTPUT / target and name == 'checkpoint.bin' else value

                with patch.object(verifier, 'raw', changed), self.assertRaises(AssertionError):
                    verifier.verify(ROOT, OUTPUT, 'all')
