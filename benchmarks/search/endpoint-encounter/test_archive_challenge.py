"""Corrupt native qualification records without executing the emulator."""
import json
from pathlib import Path
import tempfile
import unittest

from score_s01 import raw
import verify_archive_challenge as verify

ROOT = Path(__file__).parent


class ArchiveChallengeEvidence(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.evidence = Path(self.temp.name)
        for source in (ROOT / 'aq01-output').rglob('*'):
            if source.is_file():
                relative = source.relative_to(ROOT / 'aq01-output')
                name = relative.name.removesuffix('.gz')
                dest = self.evidence / relative.parent / name
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(raw(source.parent, name))

    def tearDown(self):
        self.temp.cleanup()

    def test_valid_qualification_keeps_all_cost_components(self):
        result = verify.compare(ROOT, self.evidence)
        self.assertEqual(result['known_auxiliary_frames'], 1200260)
        self.assertTrue(result['buffer_artifacts_identical'])

    def test_changed_root_emulator_bytes_are_rejected(self):
        path = self.evidence / 'prepare/root-snapshot.json'
        value = json.loads(path.read_text())
        value['emulator_state'][0] ^= 1
        path.write_text(json.dumps(value))
        with self.assertRaises(AssertionError):
            verify.prepare(ROOT, self.evidence)

    def test_double_prefix_is_rejected_before_a_witness_claim(self):
        path = self.evidence / 'one-slot/witness-full.json'
        value = json.loads(path.read_text())
        prefix = json.loads((ROOT / 'first-encounter-input.json').read_text())['actions']
        value['actions'] = prefix + value['actions']
        path.write_text(json.dumps(value))
        with self.assertRaises(AssertionError):
            verify.compare(ROOT, self.evidence)

    def test_missing_campaign_replay_cannot_qualify(self):
        path = self.evidence / 'one-slot/result.json'
        value = json.loads(path.read_text())
        value['full_campaign_replay'] = False
        path.write_text(json.dumps(value))
        with self.assertRaises(AssertionError):
            verify.compare(ROOT, self.evidence)
