from copy import deepcopy
from contextlib import redirect_stdout
from datetime import datetime, timedelta, timezone
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from named_milestone_endpoint import named_milestone_endpoint, validate_named_panel
import run_cells
from score_screen import score_screen


class NamedMilestoneEvidence(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Real completed native fixtures, including a preserved failed expectation.
        path = Path(__file__).parent.parent / 'milestone-stop/q01-results.json'
        cls.summaries = {r['id']: r['summary'] for r in json.loads(path.read_text())['records']}

    def evidence(self, key, summary=None):
        summary = summary or self.summaries[key]
        return named_milestone_endpoint(summary, summary['search_request']['frames'],
                                        summary['identity']['milestone_stop'])

    def test_native_hit_uses_complete_admitted_job_cost_before_drain(self):
        e = self.evidence('morph-one-slot')
        self.assertEqual(e['restricted_cost_interval'], [22586, 22586])
        self.assertEqual(e['observed_through_frames'], 23883)
        self.assertIs(e['hit_by_budget'], True)
        self.assertFalse(e['observed_full_budget'])

    def test_native_nonattainment_short_of_horizon_stays_unknown(self):
        e = self.evidence('energy-unreached')
        self.assertIsNone(e['hit_by_budget'])
        self.assertIsNone(e['restricted_cost_interval'])
        self.assertFalse(e['observed_full_budget'])
        summary = deepcopy(self.summaries['energy-unreached'])
        summary['result'].update(frames_emulated=1000000, stop_reason='frame_limit')
        e = self.evidence('energy-unreached', summary)
        self.assertIs(e['hit_by_budget'], False)
        self.assertEqual(e['restricted_cost_interval'], [1000000, 1000000])

    def test_native_post_budget_observation_is_retained_without_passing_gate(self):
        e = self.evidence('morph-beyond-budget')
        self.assertEqual(e['arrival_exact'], 22586)
        self.assertEqual(e['restricted_cost_interval'], [22585, 22585])
        self.assertIs(e['hit_by_budget'], False)

    def test_witness_origin_clock_does_not_replace_campaign_first_admission(self):
        self.assertEqual(self.evidence('origin-brinstar')['arrival_exact'], 171)

    def test_mislabeled_or_inconsistent_evidence_is_rejected(self):
        original = self.summaries['morph-one-slot']
        mutations = [
            ('identity', 'milestone_stop', {'name': 'energy_tank', 'observation_policy': 'metroid-named-progress-v2'}),
            ('result', 'milestone_stop', {'name': 'morph_ball', 'observation_policy': 'unknown'}),
            ('search_request', 'stop_after_milestone', 'missile_capacity'),
            ('result', 'milestone_within_budget', False),
            ('result', 'first_milestone', {'execution': 148, 'frames_emulated': 999999}),
            ('result', 'first_milestone', {'execution': 156, 'frames_emulated': 22586}),
            ('result', 'frames_emulated', True), ('result', 'verification', 'none'),
            ('result', 'stop_reason', 'wall_limit'), ('identity', 'frames', 10),
        ]
        for section, key, value in mutations:
            with self.subTest(section=section, key=key):
                summary = deepcopy(original)
                summary[section][key] = value
                with self.assertRaises(ValueError):
                    named_milestone_endpoint(summary, 1000000, original['identity']['milestone_stop'])
        missing = deepcopy(self.summaries['energy-unreached'])
        del missing['result']['first_milestone']
        with self.assertRaises(ValueError): self.evidence('energy-unreached', missing)

    def test_panel_cannot_score_a_different_milestone_or_cost_clock(self):
        e = self.evidence('morph-one-slot')
        condition = e['milestone_stop']
        reg = {'screen': {'budget_frames': 1000000, 'required_strict_wins': 3,
            'milestone_stop': condition,
            'pairs': [{'seed': i, 'control': f'c{i}', 'candidate': f'a{i}'} for i in range(4)]}}
        records = [{'id': f'{arm}0', 'checks_passed': True, 'endpoint_evidence': deepcopy(e)} for arm in ('c', 'a')]
        self.assertEqual(score_screen(records, reg)['completed_pairs'], 1)
        records[1]['endpoint_evidence']['milestone_stop']['name'] = 'missile_capacity'
        with self.assertRaises(ValueError): score_screen(records, reg)
        records[1]['endpoint_evidence'] = deepcopy(e)
        records[1]['endpoint_evidence']['cost_convention'] = 'periodic_progress_checkpoint'
        with self.assertRaises(ValueError): score_screen(records, reg)

    def test_incomplete_endpoint_evidence_cannot_enter_a_scored_pair(self):
        e = self.evidence('energy-unreached')
        reg = {'screen': {'budget_frames': 1000000, 'required_strict_wins': 3,
            'milestone_stop': e['milestone_stop'],
            'pairs': [{'seed': i, 'control': f'c{i}', 'candidate': f'a{i}'} for i in range(4)]}}
        records = [{'id': f'{arm}0', 'checks_passed': True, 'endpoint_evidence': e} for arm in ('c', 'a')]
        with self.assertRaisesRegex(ValueError, 'incomplete endpoint evidence'): score_screen(records, reg)

    def test_mismatched_registration_is_rejected_before_dispatch(self):
        condition = {'name': 'energy_tank', 'observation_policy': 'metroid-named-progress-v2'}
        reg = {'milestone_stop': condition, 'screen': {'milestone_stop': condition,
            'resources': {'measurement': 'event_stopped_cell_totals_v1', 'max_candidate_to_control_ratio': 1.25}},
            'cells': [{'manifest': {'search': {'frames': 50000000, 'stop_after_milestone': 'energy_tank'}, 'cases': [{'search': {}}]}}]}
        self.assertEqual(validate_named_panel(reg), condition)
        bad = deepcopy(reg)
        bad['cells'][0]['manifest']['cases'][0]['search']['stop_after_milestone'] = 'missile_capacity'
        with self.assertRaises(ValueError): validate_named_panel(bad)
        bad = deepcopy(reg)
        del bad['screen']['milestone_stop']
        with self.assertRaises(ValueError): validate_named_panel(bad)
        bad = deepcopy(reg)
        bad['screen']['resources']['max_candidate_to_control_ratio'] = float('inf')
        with self.assertRaises(ValueError): validate_named_panel(bad)
        bad = deepcopy(reg)
        bad['cells'][0]['manifest']['cases'][0]['search']['frames'] = 10
        with self.assertRaises(ValueError): validate_named_panel(bad)

    def test_original_run_cells_registrations_retain_their_legacy_contract(self):
        for name in ('d02', 'd02r', 's01', 'r01', 'tq01', 't01'):
            reg = json.loads((Path(__file__).parent / (name + '-registration.json')).read_text())
            self.assertIsNone(validate_named_panel(reg))

    def exercise_runner(self, name, incomplete=False):
        source = Path(__file__).parent
        original = json.loads((source.parent / 'milestone-stop/q01-registration.json').read_text())
        cell = deepcopy(next(c for c in original['cells'] if c['id'] == name))
        summary = deepcopy(self.summaries[name])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build = root / 'build'
            build.mkdir()
            (build / 'nes-eval').write_bytes(b'upstream evaluator stand-in; never executed')
            (build / 'build-info.json').write_text('{}')
            summary['build']['binary_sha256'] = run_cells.sha(build / 'nes-eval')
            reg = {**original, 'output': 'output', 'build': 'build', 'required_evidence': [],
                'deadline_utc': (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat(),
                'milestone_stop': summary['identity']['milestone_stop'],
                'binary_sha256': summary['build']['binary_sha256'],
                'build_info_sha256': run_cells.sha(build / 'build-info.json'),
                'runner_sha256': run_cells.sha(source / 'run_cells.py'),
                'analyzer_sha256': run_cells.sha(source / 'audit_controls.py'),
                'named_endpoint_sha256': run_cells.sha(source / 'named_milestone_endpoint.py'),
                'cells': [cell]}
            if incomplete:
                second = deepcopy(cell)
                second['id'] = 'must-not-dispatch'
                reg['cells'].append(second)
            registration_path = root / 'registration.json'
            registration_path.write_text(json.dumps(reg))

            def evaluator_output(command, **_kwargs):
                # Exercise the real driver around a recorded upstream evaluator
                # result. No taskset, emulator or external process is executed.
                destination = Path(command[command.index('--out') + 1]) / summary['cell']
                (destination / 'campaign').mkdir(parents=True)
                (destination / 'summary.json').write_text(json.dumps(summary))
                (destination / 'campaign/progress.jsonl').write_text('{}\n')
                return run_cells.subprocess.CompletedProcess(command, 0)

            arguments = ['run_cells.py', '--experiment', str(root), '--registration',
                         str(registration_path), '--registration-commit', 'test-registration']
            with patch('sys.argv', arguments), redirect_stdout(io.StringIO()), \
                    patch.object(run_cells.subprocess, 'run', side_effect=evaluator_output) as process, \
                    patch.object(run_cells, 'milestone_interval', side_effect=AssertionError('legacy missile extractor was called')):
                if incomplete:
                    with self.assertRaisesRegex(AssertionError, 'did not reach the registered horizon'):
                        run_cells.main()
                else:
                    run_cells.main()
                self.assertEqual(process.call_count, 1)
            return json.loads((root / 'output/results.json').read_text())

    def test_driver_selects_the_registered_endpoint_without_legacy_extraction(self):
        result = self.exercise_runner('morph-one-slot')
        self.assertTrue(result['execution_complete'])
        self.assertTrue(result['records'][0]['checks_passed'])
        self.assertEqual(result['records'][0]['endpoint_evidence']['restricted_cost_interval'], [22586, 22586])

    def test_driver_stops_before_another_cell_on_incomplete_nonattainment(self):
        result = self.exercise_runner('energy-unreached', incomplete=True)
        self.assertFalse(result['execution_complete'])
        self.assertEqual(result['allocation_stop'], 'cell failure; no subsequent cells dispatched')
        self.assertEqual(len(result['records']), 1)
        self.assertNotIn('checks_passed', result['records'][0])


if __name__ == '__main__':
    unittest.main()
