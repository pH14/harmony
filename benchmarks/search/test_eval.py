# SPDX-License-Identifier: AGPL-3.0-or-later
"""Executable runner contracts; no licensed assets or emulator required."""
import argparse
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('search_eval', Path(__file__).with_name('eval.py'))
eval = importlib.util.module_from_spec(spec)
spec.loader.exec_module(eval)


class EvaluationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.suite = json.loads(Path(__file__).with_name('qualification.json').read_text())
        self.suite['cases'] = self.suite['cases'][:1]
        self.suite['workers'] = [1]
        self.suite['memory_mib'] = [16]

    def test_duplicate_axes_and_path_components_are_rejected(self):
        for axis in ('seeds', 'workers', 'memory_mib'):
            bad = copy.deepcopy(self.suite)
            bad[axis] *= 2
            with self.assertRaises(ValueError): eval.expand_suite(bad)
        for name in ('..', '.', '../elsewhere', '/absolute'):
            bad = copy.deepcopy(self.suite)
            bad['cases'][0]['id'] = name
            with self.assertRaises(ValueError): eval.expand_suite(bad)

    def test_unknown_settings_and_empty_selection_fail_before_dispatch(self):
        bad = copy.deepcopy(self.suite)
        bad['search']['excutions'] = 1
        with self.assertRaises(ValueError): eval.expand_suite(bad)
        with self.assertRaises(ValueError): eval.expand_suite(self.suite, ['missing'])
        self.assertEqual(len(eval.expand_suite(self.suite)), 1)

    def test_asset_hashes_are_checked_against_inventory_and_suite(self):
        rom = self.root / 'private.nes'
        core = self.root / 'private.so'
        rom.write_bytes(b'private ROM sentinel')
        core.write_bytes(b'core sentinel')
        job = eval.expand_suite(self.suite)[0]
        job['case']['rom_sha256'] = eval.digest(rom)
        assets = {'smb': {'path': str(rom), 'sha256': eval.digest(rom)}, 'core': {'path': str(core), 'sha256': eval.digest(core)}}
        self.assertEqual(eval.resolve_assets(job, assets)['rom_sha256'], eval.digest(rom))
        rom.write_bytes(b'different ROM')
        with self.assertRaises(ValueError): eval.resolve_assets(job, assets)
        assets['smb']['sha256'] = eval.digest(rom)
        with self.assertRaises(ValueError): eval.resolve_assets(job, assets)

    def test_tail_preserves_partial_records(self):
        path = self.root / 'progress.jsonl'
        tail = eval.Tail(path)
        self.assertEqual(tail.read(), {})
        path.write_text('{"frames_emulated":12')
        self.assertEqual(tail.read(), {})
        with path.open('a') as stream: stream.write('}\n{"frames_emulated":24}\n')
        self.assertEqual(tail.read(), {'frames_emulated': 24})
        self.assertEqual(tail.read(), {'frames_emulated': 24})

    def fake_run(self, body, require_solved=False, finish=1, disk=1):
        binary = self.root / 'fake-eval'
        binary.write_text('#!/usr/bin/env python3\nimport json,sys,time\nfrom pathlib import Path\np=Path(sys.argv[2]);p.mkdir()\n' + body)
        binary.chmod(0o755)
        jobs = eval.expand_suite(self.suite)
        job = jobs[0]
        job['case']['require_solved'] = require_solved
        job['request']['wall_seconds'] = 0
        out = self.root / 'runs'
        out.mkdir()
        args = argparse.Namespace(out=out, binary=binary, suite_id='test', suite_sha256='0'*64,
                                  finish_seconds=finish, disk_limit_gib=disk, sample_seconds=.02)
        return eval.run_one(job, job['request'], args, [], {}, {'hostname': 'test'})

    def test_failure_keeps_observed_progress(self):
        result = self.fake_run('(p/"progress.jsonl").write_text(\'{"executions":7,"frames_emulated":123}\\n\')\nsys.exit(9)\n')
        self.assertEqual(result['status'], 'error')
        self.assertEqual(result['exit_code'], 9)
        self.assertEqual(result['last_progress']['frames_emulated'], 123)
        self.assertIsNone(result['result'])

    def test_unavailable_rss_samples_stay_unavailable_in_summaries(self):
        with patch.object(eval.ProcessMetrics, 'sample', return_value={
                'rss_bytes': None, 'read_bytes': None, 'write_bytes': None}):
            result = self.fake_run('sys.exit(9)\n')
        self.assertIsNone(result['peak_process_tree_rss_bytes_sampled'])
        self.assertEqual(result['rss_by_phase_bytes_sampled'], {'preparation': None})
        self.assertIsNone(result['io_bytes_last_sample']['read_bytes'])
        self.assertGreater(result['max_process_rss_bytes'], 0)

    def test_watchdog_kills_only_the_run_and_retains_timeout(self):
        result = self.fake_run('(p/"progress.jsonl").write_text(\'{"executions":1}\\n\')\ntime.sleep(30)\n', finish=.1)
        self.assertEqual(result['status'], 'timeout')
        self.assertLess(result['elapsed_seconds'], 2)
        self.assertLess(result['exit_code'], 0)

    def test_disk_limit_is_a_visible_failure(self):
        result = self.fake_run('(p/"large").write_bytes(b"x"*100000)\ntime.sleep(30)\n', disk=.00001)
        self.assertEqual(result['status'], 'disk_limit')

    def test_unsolved_required_case_is_a_regression(self):
        result = self.fake_run('(p/"result.json").write_text(\'{"solved":false}\')\n', require_solved=True)
        self.assertEqual(result['status'], 'regression')
        self.assertEqual(result['final_disk'], eval.disk_usage(self.root / 'runs' / result['cell']))

    def matrix(self, path):
        path.mkdir()
        item = {'cell': 'smb-s1-w1-m16', 'case': 'smb', 'origin': 'genesis', 'status': 'complete',
                'identity': {'policies': {'key': 'minimal-v1'}},
                'search_request': {'game': 'smb', 'seed': 1, 'workers': 1, 'memory_mib': 16},
                'host': {'hostname': 'test'}, 'result': {'solved': True, 'frames_to_first_victory': 12,
                'frames_emulated': 16, 'frames_per_second': 4}, 'last_progress': {},
                'max_process_rss_bytes': 1024, 'peak_disk_logical_bytes_sampled': 2048}
        eval.write_json(path/'results.json', [item])
        eval.write_json(path/'matrix.json', {'cells': [item['cell']]})
        eval.write_json(path/'suite.json', {'id': 'test'})
        cell = path/item['cell']
        (cell/'campaign').mkdir(parents=True)
        eval.write_json(cell/'summary.json', item)
        (cell/'resources.jsonl').write_text('{}\n')
        return item

    def test_comparison_refuses_changed_policies_budgets_and_missing_cells(self):
        a, b = self.root/'a', self.root/'b'
        original = self.matrix(a)
        self.matrix(b)
        self.assertTrue(eval.compare(a, b)['pairs'][0]['comparable'])
        for field, key, value in [('identity', 'policies', {'key': 'hinted'}), ('search_request', 'memory_mib', 32), ('search_request', 'frames', 100)]:
            changed = copy.deepcopy(original)
            changed[field][key] = value
            eval.write_json(b/'results.json', [changed])
            with self.assertRaises(ValueError): eval.compare(a, b)
        eval.write_json(b/'results.json', [])
        with self.assertRaises(ValueError): eval.compare(a, b)

    def test_export_is_allowlisted_and_checksummed(self):
        private, public = self.root/'private', self.root/'public'
        item = self.matrix(private)
        for name in ('rom.nes', 'checkpoint.json', 'stream.jsonl', 'request.private.json'):
            (private/item['cell']/'campaign'/name).write_text('DO NOT PUBLISH')
        eval.export(private, public)
        for path in public.rglob('*'):
            if path.is_file(): self.assertNotIn('DO NOT PUBLISH', path.read_text())
        checksums = eval.read_json(public/'checksums.json')
        self.assertIn('index.html', checksums)
        for path, expected in checksums.items(): self.assertEqual(eval.digest(public/path), expected)

    def test_changed_affinity_keeps_quality_comparable_but_flags_timing(self):
        a, b = self.root/'a', self.root/'b'
        original = self.matrix(a)
        self.matrix(b)
        for path, cpus in ((a, [0]), (b, [1])):
            changed = copy.deepcopy(original)
            changed['cpu_set'] = cpus
            eval.write_json(path/'results.json', [changed])
            eval.write_json(path/'matrix.json', {'cells': [changed['cell']], 'runner': {'jobs': 1}})
        pair = eval.compare(a, b)['pairs'][0]
        self.assertTrue(pair['comparable'])
        self.assertTrue(pair['same_host'])
        self.assertTrue(pair['same_runner_configuration'])
        self.assertFalse(pair['timing_environment_matches'])

    def test_export_refuses_symlinks(self):
        private, public = self.root/'private', self.root/'public'
        item = self.matrix(private)
        secret = self.root/'secret'
        secret.write_text('private')
        (private/item['cell']/'campaign'/'result.json').symlink_to(secret)
        with self.assertRaises(ValueError): eval.export(private, public)
        self.assertFalse(public.exists())

    def test_censored_runs_are_not_imputed_as_completion_times(self):
        item = self.matrix(self.root/'matrix')
        censored = copy.deepcopy(item)
        censored['result']['solved'] = False
        censored['result']['frames_to_first_victory'] = None
        rows = eval.aggregates([item, censored])
        self.assertEqual(rows[0]['solve_fraction_of_valid'], .5)
        self.assertEqual(rows[0]['median_frames_to_victory_among_successes'], 12)
        low, high = rows[0]['solve_fraction_wilson95']
        self.assertLess(low, .2)
        self.assertGreater(high, .8)

    def test_solve_intervals_include_observed_fraction_at_boundaries(self):
        self.assertIsNone(eval.solve_interval(0, 0))
        for total in range(1, 101):
            for solved in range(total + 1):
                low, high = eval.solve_interval(solved, total)
                self.assertLessEqual(0., low)
                self.assertLessEqual(low, solved / total)
                self.assertLessEqual(solved / total, high)
                self.assertLessEqual(high, 1.)

    def test_completion_figures_keep_failed_and_censored_trials_in_denominator(self):
        import plots
        solved = {'result': {'solved': True, 'frames_to_first_victory': 12}}
        censored = {'result': {'solved': False, 'frames_to_first_victory': 99}}
        failed = {'result': None}
        self.assertEqual(plots.completions([solved, censored, failed], 'frames_to_first_victory'),
                         ([0, 12], [0, 1 / 3]))


if __name__ == '__main__':
    unittest.main()
