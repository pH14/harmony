"""Planted corruption of the completed conditional evidence; no emulation."""
import json
from pathlib import Path
import tempfile
import unittest

import score_s01

ROOT = Path(__file__).parent


class ConditionalEvidence(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.output = Path(self.temp.name)
        for source in (ROOT / 's01-output').iterdir():
            name = source.name.removesuffix('.gz')
            (self.output / name).write_bytes(score_s01.raw(ROOT / 's01-output', name))

    def tearDown(self):
        self.temp.cleanup()

    def test_completed_evidence_keeps_earlier_damage_despite_later_death(self):
        result = score_s01.score(ROOT, self.output)
        self.assertEqual(result['arms']['ordinary']['trials_with_surviving_damage_endpoint'], 1)
        self.assertEqual(result['arms']['ordinary']['stops']['death'], 32)
        self.assertEqual(result['known_auxiliary_frames'], 378525)

    def test_changed_frozen_draws_are_rejected(self):
        (self.output / 'suffixes.json').write_text('[]')
        with self.assertRaises(AssertionError):
            score_s01.score(ROOT, self.output)

    def test_missing_required_witness_is_rejected(self):
        path = self.output / 'summary.json'
        value = json.loads(path.read_text())
        value['witnesses'] = []
        path.write_text(json.dumps(value))
        with self.assertRaises(AssertionError):
            score_s01.score(ROOT, self.output)

    def test_corrupted_witness_is_rejected(self):
        path = self.output / 'ordinary-surviving-damage.json'
        value = json.loads(path.read_text())
        value['endpoint']['health'] += 1
        path.write_text(json.dumps(value))
        with self.assertRaises(AssertionError):
            score_s01.score(ROOT, self.output)

    def test_second_stage_uses_its_longer_prefix_and_keeps_passive_survival(self):
        result = score_s01.score(ROOT, ROOT / 's02-output', 's02')
        self.assertEqual(result['known_auxiliary_frames'], 539906)
        self.assertEqual(result['arms']['ordinary']['trials_with_surviving_damage_endpoint'], 5)
        self.assertEqual(result['arms']['ordinary']['stops'], {'death': 32})
        self.assertEqual(result['arms']['passive']['stops'], {'action_limit': 32})
        self.assertEqual(result['arms']['passive']['trials_with_observed_hp_drop'], 0)
