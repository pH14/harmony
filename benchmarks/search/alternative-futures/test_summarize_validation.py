"""Planted reporting failures: partial work, absent replay, and boundary ambiguity."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('assessment', Path(__file__).with_name('summarize_validation.py'))
assessment = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assessment)


class AssessmentTests(unittest.TestCase):
    def fixture(self, root, *, executions=3_000_000, frames=390_000_000,
                replay=True, discovery=True, launcher='runner_complete'):
        plan = {'id': 'fixture', 'seeds': [17], 'source_tree_sha256': 'source',
                'binary_sha256': 'binary', 'cells': []}
        for arm in ['control', 'corrected']:
            label = 'fixture-' + arm
            plan['cells'].append({'label': label, 'seed': 17, 'arm': arm, 'suite': {'search': {}}})
            directory = root / 'runs' / label / 'metroid-full-s17-w4-m8192'
            (directory / 'campaign').mkdir(parents=True)
            point = {'executions': executions, 'frames_emulated': frames,
                     'workload_diagnostics': {'named_progress': {'first_seen': {
                         'kraid_defeated': {'execution': executions - 1} if discovery else None}}}}
            prior = copy.deepcopy(point)
            prior['executions'] -= 100
            prior['frames_emulated'] -= 1000
            prior['workload_diagnostics']['named_progress']['first_seen']['kraid_defeated'] = None
            (directory / 'campaign' / 'progress.jsonl').write_text(json.dumps(prior) + '\n' + json.dumps(point) + '\n')
            witness = {'input_sha256': 'input', 'replay': {'diagnostics': {
                'named_progress': {'first_seen': {'kraid_defeated': {'execution': 10}}}}}}
            summary = {'status': 'complete', 'identity': {'seed': 17, 'prefix_sha256': None,
                       'source_tree_sha256': 'source'}, 'build': {'binary_sha256': 'binary'},
                       'search_request': {}, 'last_progress': point,
                       'result': {'executions': executions, 'frames_emulated': frames,
                                  'milestone_witnesses': {'kraid_defeated': witness} if replay else {}}}
            (directory / 'summary.json').write_text(json.dumps(summary))
            launch_dir = root / 'validation' / 'fixture'
            launch_dir.mkdir(parents=True, exist_ok=True)
            (launch_dir / (label + '-execution.json')).write_text(json.dumps({'status': launcher}))
        return plan

    def test_partial_work_and_missing_replay_cannot_count_as_success(self):
        for options in [{'executions': 2_000_000}, {'replay': False}, {'launcher': 'incomplete_or_error'}]:
            with self.subTest(options=options), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                value = assessment.summarize(root, self.fixture(root, **options))
                self.assertEqual(value['completed_boss_successes_by_arm'], {'control': 0, 'corrected': 0})

    def test_verified_in_budget_witness_and_late_export_are_preserved(self):
        for discovery in [True, False]:
            with self.subTest(discovery=discovery), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                value = assessment.summarize(root, self.fixture(root, discovery=discovery))
                self.assertTrue(value['panel_complete'])
                self.assertEqual(value['completed_boss_successes_by_arm'], {'control': 1, 'corrected': 1})

    def test_frame_limit_ambiguity_is_not_a_success(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            value = assessment.summarize(root, self.fixture(root, frames=400_000_500))
            self.assertTrue(value['panel_complete'])
            self.assertEqual(value['completed_boss_successes_by_arm'], {'control': 0, 'corrected': 0})
            self.assertEqual(value['cells'][0]['verified_bosses'], ['kraid_defeated'])

    def test_common_frame_sampling_does_not_use_future_work(self):
        points = [{'executions': 1, 'frames_emulated': 100}, {'executions': 2, 'frames_emulated': 200}]
        self.assertIsNone(assessment.at_frame(points, 99))
        self.assertEqual(assessment.at_frame(points, 199)['executions'], 1)
        self.assertEqual(assessment.work_bounds(points, 2)['admitted_frames_lower'], 100)

    def test_infrastructure_failure_preserves_other_cells_and_complete_requires_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            plan = self.fixture(root)
            path = root / 'runs' / 'fixture-control' / 'metroid-full-s17-w4-m8192' / 'summary.json'
            value = json.loads(path.read_text())
            value.update(status='infrastructure_error', identity=None, result=None,
                         last_progress={}, error='planted setup failure')
            path.write_text(json.dumps(value))
            report = assessment.summarize(root, plan)
            self.assertFalse(report['panel_complete'])
            self.assertEqual(report['completed_by_arm'], {'control': 0, 'corrected': 1})
            self.assertEqual(report['cells'][0]['failure']['error'], 'planted setup failure')
            value['status'] = 'complete'
            path.write_text(json.dumps(value))
            with self.assertRaisesRegex(AssertionError, 'completed outcome lacks provenance'):
                assessment.summarize(root, plan)


if __name__ == '__main__':
    unittest.main()
