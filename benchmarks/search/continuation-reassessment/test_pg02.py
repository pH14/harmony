#!/usr/bin/env python3
"""Offline planted failure and decision-boundary tests for PG02."""
import copy
import gzip
import json
from pathlib import Path
import unittest
from score_pg02 import cell_gate, panel_score, restricted_cost

ROOT = Path(__file__).resolve().parent


def fixture():
    q = json.loads((ROOT / 'pm01-request.json').read_text())
    def read(name):
        raw = gzip.decompress((ROOT / 'pm01-output/cell' / (name + '.gz')).read_bytes())
        return json.loads(raw.splitlines()[-1]) if name.endswith('jsonl') else json.loads(raw)
    return q, read('result.json'), read('campaign.json'), read('usage.json'), read('progress.jsonl'), 2880


def panel(costs):
    return [dict(valid=True, pair=pair, seed=10 + pair, arm=arm, restricted_cost=cost)
            for pair, triple in enumerate(costs)
            for arm, cost in zip(['ordinary', 'progress', 'capacity'], triple)]


class EfficacyTests(unittest.TestCase):
    def test_final_drain_is_charged_and_late_attainment_cannot_beat_censoring(self):
        self.assertEqual(restricted_cost(True, 1005000, 1004645), 1004645)
        self.assertEqual(restricted_cost(False, 1005000, 1004645), 1004645)
        self.assertEqual(restricted_cost(True, 205000, 1004645), 205000)

    def test_optional_nonactivation_is_valid_but_ordinary_alternates_are_not(self):
        args = fixture()
        args[4]['retention_diagnostics']['alternative_admissions'] = 0
        self.assertTrue(cell_gate(*args))
        args[0]['slot_retention'] = args[2]['slot_retention'] = None
        self.assertTrue(cell_gate(*args))
        args[4]['retention_diagnostics']['alternative_admissions'] = 1
        self.assertFalse(cell_gate(*args))

    def test_premature_horizon_and_missing_accounting_fail(self):
        for mutation in ['execution', 'wall', 'physical', 'replay', 'drain']:
            args = fixture()
            if mutation in ['execution', 'wall']:
                args[0]['frames'] = 1000000
                args[1]['stop_reason'] = mutation + '_limit'
            elif mutation == 'physical':
                args[1]['cost']['search_engine']['targets_closed'] = 4
                args[3]['cost'] = copy.deepcopy(args[1]['cost'])
            elif mutation == 'replay':
                args[1]['full_campaign_replay'] = False
            else:
                args[0]['frames'] -= 3000
            self.assertFalse(cell_gate(*args), mutation)

    def test_defeat_uses_ridley_bit_two_and_a_living_in_budget_event(self):
        args = fixture()
        result = args[1]
        result['milestone_reached_within_budget'] = True
        result['first_milestone'] = dict(execution=100, frames_emulated=20000)
        result['witness']['context']['memory']['ridley_status'] = 2
        self.assertTrue(cell_gate(*args))
        result['witness']['context']['memory']['ridley_status'] = 1
        self.assertFalse(cell_gate(*args))
        result['witness']['context']['memory']['ridley_status'] = 2
        result['witness']['dead'] = True
        self.assertFalse(cell_gate(*args))
        result['witness']['dead'] = False
        result['first_milestone']['frames_emulated'] = 250001
        self.assertFalse(cell_gate(*args))

    def test_two_both_censored_pairs_stop_for_futility(self):
        self.assertEqual(panel_score(panel([(100, 100, 100)]))['status'], 'continue')
        score = panel_score(panel([(100, 100, 100)] * 2))
        self.assertEqual(score['status'], 'futile')
        self.assertEqual(score['unrun_pairs'], 2)
        self.assertEqual(score['comparisons']['ordinary']['strict_wins'], 0)

    def test_three_wins_and_exact_fifteen_percent_against_both_are_required(self):
        self.assertEqual(panel_score(panel([(100, 80, 100)] * 3 + [(100, 100, 100)]))['status'], 'passed')
        self.assertNotEqual(panel_score(panel([(100, 81, 100)] * 3 + [(100, 100, 100)]))['status'], 'passed')
        self.assertEqual(panel_score(panel([(100, 80, 70)] * 2))['status'], 'futile')
        rows = panel([(100, 80, 100)] * 4)
        rows[0]['valid'] = False
        self.assertEqual(panel_score(rows)['status'], 'invalid')

    def test_partial_or_duplicate_triples_cannot_be_scored(self):
        rows = panel([(100, 80, 100)])
        with self.assertRaises(AssertionError):
            panel_score(rows[:-1])
        with self.assertRaises(ValueError):
            panel_score(rows + rows[:1])


if __name__ == '__main__':
    unittest.main()
