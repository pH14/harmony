#!/usr/bin/env python3
"""Actual four-arm serialization, exact intervals, planted gates and cleanup."""
import copy
import gzip
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch
from score_sr01 import cell_gate
from score_sr02 import ARMS, milestone_interval, panel_score
from run_sr02 import finish_cell

ROOT = Path(__file__).resolve().parent


def fixture(arm):
    q = json.loads((ROOT / (arm + '-sr01-request.json')).read_text())
    out = ROOT / ('sr01-output/' + arm + '/campaign')
    def read(name):
        raw = gzip.decompress((out / (name + '.gz')).read_bytes())
        return json.loads(raw.splitlines()[-1]) if name.endswith('jsonl') else json.loads(raw)
    return q, read('result.json'), read('campaign.json'), read('usage.json'), read('progress.jsonl'), 2880


def panel(count, candidate=(800000, 850000), control=(1000000, 1000000)):
    return [dict(valid=True, pair=i, seed=100+i, arm=arm,
                 restricted_interval=list(candidate if arm == 'progress-half' else control),
                 cpu_microseconds=1000000, peak_rss_kib=100000)
            for i in range(count) for arm in sorted(ARMS)]


class EfficacyTests(unittest.TestCase):
    def test_actual_four_arm_contracts_and_planted_corruption(self):
        for arm in ARMS:
            args = fixture(arm)
            self.assertEqual('slot_retention' in args[2], arm != 'ordinary-control')
            self.assertTrue(cell_gate(*args))
            args[4]['retention_diagnostics']['alternative_admissions'] = 0
            self.assertTrue(cell_gate(*args), 'Nonactivation is valid efficacy data')
            for defect in ['selector', 'replay', 'frames', 'meter', 'retention']:
                args = fixture(arm)
                if defect == 'selector': args[2]['parent_scheduler'] += '-wrong'
                if defect == 'replay': args[1]['full_campaign_replay'] = False
                if defect == 'frames': args[0]['frames'] = 1000000
                if defect == 'meter': args[1]['physical_frames']['total'] -= 929
                if defect == 'retention': args[2]['slot_retention'] = 'unknown'
                self.assertFalse(cell_gate(*args), (arm, defect))

    def test_job_interval_and_censoring_exclude_unobserved_precision(self):
        q = dict(workers=4, frames=1000000)
        result = dict(milestone_reached_within_budget=True,
                      first_milestone=dict(execution=2, frames_emulated=180))
        rows = [dict(origin_kind='snapshot_root', resume_actions=0),
                dict(event='job', sequence=1, frames=100),
                dict(event='job', sequence=2, frames=80),
                dict(event='job', sequence=3, frames=700)]
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / 'stream'
            path.write_text(''.join(json.dumps(r)+'\n' for r in rows))
            self.assertEqual(milestone_interval(q, result, path), [4745, 4825])
            self.assertEqual(milestone_interval(q, dict(milestone_reached_within_budget=False), path), [1004645, 1004645])
            result['first_milestone']['frames_emulated'] = 179
            with self.assertRaises(AssertionError): milestone_interval(q, result, path)
            result['first_milestone'] = dict(execution=4, frames_emulated=180)
            with self.assertRaises(AssertionError): milestone_interval(q, result, path)

    def test_both_censored_ties_stop_only_when_three_wins_are_impossible(self):
        for count, expected in [(1, 'continue'), (2, 'futile')]:
            self.assertEqual(panel_score(panel(count, (1004645, 1004645), (1004645, 1004645)))['status'], expected)
        # Overlap is not a strict win even if a favorable point estimate could win.
        self.assertEqual(panel_score(panel(2, (800000, 900000), (850000, 950000)))['status'], 'futile')

    def test_exact_worst_case_ratio_and_all_comparators_are_required(self):
        self.assertEqual(panel_score(panel(4))['status'], 'passed')
        self.assertEqual(panel_score(panel(4, (800000, 850001)))['status'], 'failed')
        rows = panel(2)
        for row in rows:
            if row['arm'] == 'capacity-half': row['restricted_interval'] = [700000, 750000]
        self.assertEqual(panel_score(rows)['status'], 'futile')
        rows = panel(4)
        for row in rows:
            if row['pair'] == 0 and row['arm'] == 'progress-half': row['restricted_interval'] = [1000000, 1000000]
            elif row['arm'] == 'progress-half': row['restricted_interval'] = [700000, 700000]
        self.assertEqual(panel_score(rows)['status'], 'passed', 'Three strict wins and15% mean reduction suffice')

    def test_resource_limits_use_fixed_exact_comparisons(self):
        for field, original in [('cpu_microseconds', 1000000), ('peak_rss_kib', 100000)]:
            rows = panel(4)
            for row in rows:
                if row['arm'] == 'progress-half': row[field] = original * 125 // 100
            self.assertEqual(panel_score(rows)['status'], 'passed')
            next(row for row in rows if row['arm']=='progress-half')[field] += 1
            self.assertEqual(panel_score(rows)['status'], 'failed')

    def test_invalid_duplicate_incomplete_and_reused_pairs_cannot_pass(self):
        rows = panel(1); rows[0]['valid'] = False
        self.assertEqual(panel_score(rows)['status'], 'invalid')
        for rows in [panel(1)[:-1], panel(1)+[panel(1)[0]], panel(2)]:
            if len(rows) == 8:
                for row in rows: row['seed'] = 100
            with self.assertRaises(AssertionError): panel_score(rows)

    def test_already_removed_service_does_not_mask_failure_receipts(self):
        terminal = dict(ActiveState='failed', MainPID='0', ExecMainStatus='1')
        gone = dict(LoadState='not-found', ActiveState='inactive', MainPID='0')
        with tempfile.TemporaryDirectory() as d, \
             patch('run_sr02.subprocess.run', return_value=SimpleNamespace(returncode=5, stderr='Unit not loaded')), \
             patch('run_sr02.state', return_value=gone), \
             patch('run_sr02.subprocess.check_output', return_value=b'{"resource":"receipt"}\n'):
            out = Path(d)
            finish_cell('test-owned.service', out, terminal)
            finish_cell('test-owned.service', out, gone)  # Idempotent cleanup preserves first evidence.
            self.assertEqual(json.loads((out/'service-terminal.json').read_text()), terminal)
            self.assertEqual(json.loads((out/'service-stop-command.json').read_text())['returncode'], 5)
            self.assertEqual(json.loads((out/'service-stopped.json').read_text()), gone)
            self.assertTrue((out/'service-journal.jsonl').read_bytes())

if __name__ == '__main__': unittest.main()
