"""Fixed-horizon classification must not inherit a process-completion claim."""
from pathlib import Path
import unittest

import score_archive_pilot as scorer

ROOT = Path(__file__).parent


class ArchivePilotEvidence(unittest.TestCase):
    def test_known_pilot_recomputes_with_drain_and_replay_costs(self):
        result = scorer.score(ROOT, ROOT / 'ap01-output')
        self.assertEqual(result['decision'], 'no_defeat_at_horizon')
        self.assertEqual(result['admitted_drain_frames'], 267)
        self.assertEqual(result['known_auxiliary_frames'], 977952)

    def test_execution_and_wall_censoring_are_not_completed_negatives(self):
        self.assertEqual(scorer.classify(None, 5000, 100000, 250000), (False, False, 'incomplete'))

    def test_origin_and_post_budget_events_cannot_pass(self):
        for event in [{'execution': 0, 'frames_emulated': 0},
                      {'execution': 4, 'frames_emulated': 250001}]:
            self.assertEqual(scorer.classify(event, 4, 250267, 250000),
                             (False, True, 'no_defeat_at_horizon'))
        self.assertEqual(scorer.classify({'execution': 4, 'frames_emulated': 249999}, 4, 250267, 250000),
                         (True, True, 'conditional_capability'))
