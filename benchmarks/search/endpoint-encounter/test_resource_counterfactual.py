"""Saved RF01 evidence must preserve completion and the exact intervention."""
import copy
from pathlib import Path
import unittest
from unittest.mock import patch

import verify_resource_counterfactual as scorer

ROOT = Path(__file__).parent


class ResourceCounterfactual(unittest.TestCase):
    def test_saved_panel_closes_without_defeat(self):
        result = scorer.verify(ROOT, ROOT/'rf01-output', 'all')
        self.assertEqual(result['decision'], 'no_defeat_with_full_resources_at_episode_bounds')
        self.assertEqual(result['known_auxiliary_frames'], 874952)
        self.assertTrue(result['default_identity_pass'])
        self.assertTrue(result['exact_resource_state_check'])
        self.assertEqual(result['paired_observed_defeats'],
                         {'ordinary': {'neither': 32}, 'passive': {'neither': 32}})

    def reject_mutation(self, filename, mutate):
        original = scorer.read

        def changed(root, name):
            value = original(root, name)
            if name == filename:
                value = copy.deepcopy(value)
                mutate(value)
            return value

        with patch.object(scorer, 'read', changed):
            with self.assertRaises((AssertionError, KeyError)):
                scorer.verify(ROOT, ROOT/'rf01-output', 'all')

    def test_other_state_change_and_missing_witness_operation_fail(self):
        self.reject_mutation('resource-root-snapshot.json',
                             lambda v: v['emulator_state'].__setitem__(15090, 1))
        self.reject_mutation('ordinary-surviving-damage.json',
                             lambda v: v.pop('resource_operation', None))

    def test_incomplete_process_cannot_be_a_completed_negative(self):
        self.reject_mutation('full-process.json',
                             lambda v: v.update(stop_reason='wall_limit'))
