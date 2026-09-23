# SPDX-License-Identifier: AGPL-3.0-or-later
"""Ladder and milestone table scoring over synthetic matrix directories."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ladder  # noqa: E402
import milestones  # noqa: E402


def first_seen(**executions):
    return {
        name: None if value is None else {'execution': value, 'route_action_end_frame': 0}
        for name, value in executions.items()
    }


def write_cell(matrix, case, seed, seen, game='metroid', progress_log=None):
    cell = matrix / f'{case}-s{seed}-w4-m6144'
    cell.mkdir(parents=True)
    summary = {
        'case': case,
        'cell': cell.name,
        'status': 'complete',
        'exit_code': 0,
        'build': {'binary_sha256': 'ab' * 32},
        'search_request': {'game': game, 'seed': seed, 'root_input': f'roots/{case}.json'},
        'identity': {'policies': {'key_policy': 'metroid_test_key'}},
        'last_progress': {
            'executions': 100_000,
            'workload_diagnostics': {'named_progress': {'first_seen': seen}},
        },
    }
    (cell / 'summary.json').write_text(json.dumps(summary))
    if progress_log is not None:
        (cell / 'campaign').mkdir()
        lines = [json.dumps(record) for record in progress_log]
        (cell / 'campaign' / 'progress.jsonl').write_text('\n'.join(lines) + '\n{"truncated')
    return cell


class LadderTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.matrix = Path(self.directory.name) / 'ladder-build'

    def tearDown(self):
        self.directory.cleanup()

    def test_a_root_starts_after_its_deepest_held_milestone_and_needs_two_seeds_per_step(self):
        write_cell(self.matrix, 'ladder-seg4', 11, first_seen(brinstar=1, norfair=1, ridley_defeated=900, ice_beam=5_000))
        write_cell(self.matrix, 'ladder-seg4', 12, first_seen(brinstar=1, ridley_defeated=700, ice_beam=None))
        write_cell(self.matrix, 'ladder-seg4', 13, first_seen(brinstar=40))
        root = ladder.collect(self.matrix)['roots']['ladder-seg4']
        held, remaining, passed = ladder.score(root)
        self.assertEqual(held, {'brinstar'})
        self.assertEqual(remaining[0], 'norfair')
        self.assertEqual(passed, [])
        write_cell(self.matrix, 'ladder-seg5', 11, first_seen(norfair=1, ridley_defeated=900, ice_beam=5_000))
        write_cell(self.matrix, 'ladder-seg5', 12, first_seen(norfair=1, ridley_defeated=700))
        root = ladder.collect(self.matrix)['roots']['ladder-seg5']
        held, remaining, passed = ladder.score(root)
        self.assertEqual(held, {'brinstar', 'norfair'})
        self.assertEqual(passed, [('ridley_defeated', [700, 900])])

    def test_the_ending_passes_every_milestone_before_it(self):
        write_cell(self.matrix, 'ladder-seg23', 11, first_seen(mother_brain_room=1, ending=39_813))
        write_cell(self.matrix, 'ladder-seg23', 12, first_seen(mother_brain_room=1, ending=50_223))
        root = ladder.collect(self.matrix)['roots']['ladder-seg23']
        held, remaining, passed = ladder.score(root)
        self.assertEqual(remaining, ['mother_brain_defeated', 'escape_started', 'ending'])
        self.assertEqual([name for name, _ in passed], remaining)
        self.assertEqual(passed[-1][1], [39_813, 50_223])

    def test_render_prints_one_column_per_build_and_skips_other_games(self):
        write_cell(self.matrix, 'ladder-seg10', 11, first_seen(tourian_approach=1, tourian_end=14_251))
        write_cell(self.matrix, 'ladder-seg10', 12, first_seen(tourian_approach=1, tourian_end=11_286))
        write_cell(self.matrix, 'smb', 11, {}, game='smb')
        document = ladder.collect(self.matrix)
        self.assertEqual(set(document['roots']), {'ladder-seg10'})
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            ladder.render([document, document])
        table = output.getvalue().splitlines()
        self.assertIn('abababababab passed', table[0])
        self.assertEqual(table[1].count('1 to tourian_end at 11,286, 14,251'), 2)

    def test_eval_ladder_prints_the_table_and_writes_a_document_it_reads_back(self):
        write_cell(self.matrix, 'ladder-seg13', 11, first_seen(tourian_approach=1, tourian_end=1_751))
        write_cell(self.matrix, 'ladder-seg13', 12, first_seen(tourian_approach=1, tourian_end=9_614))
        out = Path(self.directory.name) / 'scores'
        script = Path(__file__).resolve().with_name('eval.py')
        printed = subprocess.run(
            [sys.executable, str(script), 'ladder', str(self.matrix), '--out', str(out)],
            check=True, capture_output=True, text=True,
        ).stdout
        self.assertIn('1 to tourian_end at 1,751, 9,614', printed)
        written = out / 'ladder-build.json'
        document = ladder.collect(written)
        self.assertEqual(document['format'], 'harmony-metroid-ladder-v1')
        self.assertEqual(ladder.score(document['roots']['ladder-seg13'])[2], [('tourian_end', [1_751, 9_614])])


class MilestoneTableTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.matrix = Path(self.directory.name) / 'panel'

    def tearDown(self):
        self.directory.cleanup()

    def test_the_progress_log_tail_wins_over_the_summary(self):
        log = [
            {'executions': 10, 'workload_diagnostics': {'named_progress': {'first_seen': first_seen(brinstar=10)}}},
            {'executions': 20, 'workload_diagnostics': {'named_progress': {'first_seen': first_seen(brinstar=10, bombs=20)}}},
        ]
        write_cell(self.matrix, 'metroid-new-game', 3, first_seen(brinstar=10), progress_log=log)
        write_cell(self.matrix, 'smb', 3, {}, game='smb')
        name, rows = milestones.collect(self.matrix)
        self.assertEqual(name, 'panel')
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['executions_reached'], 20)
        self.assertEqual(rows[0]['first_execution'], {'brinstar': 10, 'bombs': 20})
        self.assertEqual(rows[0]['key_policy'], 'metroid_test_key')

    def test_a_results_file_reads_summaries_under_rows(self):
        cell = write_cell(self.matrix, 'metroid-new-game', 4, first_seen(norfair=77, kraid_area=None))
        results = Path(self.directory.name) / 'results.json'
        results.write_text(json.dumps({'rows': [json.loads((cell / 'summary.json').read_text())]}))
        name, rows = milestones.collect(results)
        self.assertEqual(name, 'results')
        self.assertEqual(rows[0]['first_execution'], {'norfair': 77})
        self.assertEqual(rows[0]['seed'], 4)


if __name__ == '__main__':
    unittest.main()
