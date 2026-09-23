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


def write_cell(matrix, case, seed, seen, game='metroid', progress_log=None, workers=4):
    cell = matrix / f'{case}-s{seed}-w{workers}-m6144'
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
        write_cell(self.matrix, 'ladder-seg4', 13, first_seen(brinstar=1))
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

    def test_a_seed_whose_first_observation_comes_late_still_shows_the_held_rungs(self):
        write_cell(self.matrix, 'ladder-seg22', 11, first_seen(tourian=4, tourian_bottom=4, escape_started=900))
        write_cell(self.matrix, 'ladder-seg22', 12, first_seen(tourian=1, tourian_bottom=1, escape_started=1_300))
        write_cell(self.matrix, 'ladder-seg22', 13, first_seen(tourian=1, tourian_bottom=1))
        held, remaining, passed = ladder.score(ladder.collect(self.matrix)['roots']['ladder-seg22'])
        self.assertIn('tourian_bottom', held)
        self.assertEqual(remaining[0], 'tourian_approach')

    def test_a_rung_is_held_only_when_every_seed_shows_it_in_its_first_observation(self):
        write_cell(self.matrix, 'ladder-seg6', 11, first_seen(norfair=1, ridley_defeated=1))
        write_cell(self.matrix, 'ladder-seg6', 12, first_seen(norfair=1, ridley_defeated=4_200))
        write_cell(self.matrix, 'ladder-seg6', 13, first_seen(norfair=1))
        held, remaining, passed = ladder.score(ladder.collect(self.matrix)['roots']['ladder-seg6'])
        self.assertEqual(held, {'brinstar', 'norfair'})
        self.assertEqual(remaining[0], 'ridley_defeated')
        self.assertEqual(passed, [('ridley_defeated', [1, 4_200])])

    def test_cells_of_one_seed_count_as_one_seed(self):
        write_cell(self.matrix, 'ladder-seg7', 11, first_seen(norfair=1, ridley_defeated=900), workers=4)
        write_cell(self.matrix, 'ladder-seg7', 11, first_seen(norfair=1, ridley_defeated=500), workers=8)
        root = ladder.collect(self.matrix)['roots']['ladder-seg7']
        self.assertEqual(sorted(root['cells']), ['ladder-seg7-s11-w4-m6144', 'ladder-seg7-s11-w8-m6144'])
        self.assertEqual({cell['seed'] for cell in root['cells'].values()}, {11})
        self.assertEqual(ladder.score(root)[2], [])
        write_cell(self.matrix, 'ladder-seg7', 12, first_seen(norfair=1, ridley_defeated=700))
        root = ladder.collect(self.matrix)['roots']['ladder-seg7']
        self.assertEqual(ladder.score(root)[2], [('ridley_defeated', [500, 700])])

    def test_the_ending_passes_every_milestone_before_it(self):
        write_cell(self.matrix, 'ladder-seg23', 11, first_seen(mother_brain_room=1, ending=39_813))
        write_cell(self.matrix, 'ladder-seg23', 12, first_seen(mother_brain_room=1, ending=50_223))
        root = ladder.collect(self.matrix)['roots']['ladder-seg23']
        held, remaining, passed = ladder.score(root)
        self.assertEqual(remaining, ['mother_brain_defeated', 'escape_started', 'ending'])
        self.assertEqual([name for name, _ in passed], remaining)
        self.assertEqual(passed[-1][1], [39_813, 50_223])

    def test_render_prints_one_column_per_build_and_skips_other_games_and_cases(self):
        write_cell(self.matrix, 'ladder-seg10', 11, first_seen(tourian_approach=1, tourian_end=14_251))
        write_cell(self.matrix, 'ladder-seg10', 12, first_seen(tourian_approach=1, tourian_end=11_286))
        write_cell(self.matrix, 'smb', 11, {}, game='smb')
        write_cell(self.matrix, 'metroid-full', 11, first_seen(brinstar=40))
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
        self.assertEqual(document['format'], 'harmony-metroid-ladder-v2')
        self.assertEqual(ladder.score(document['roots']['ladder-seg13'])[2], [('tourian_end', [1_751, 9_614])])
        written.write_text(json.dumps({**document, 'format': 'harmony-metroid-ladder-v1'}))
        with self.assertRaisesRegex(ValueError, 'score its matrix directory again'):
            ladder.collect(written)


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

    def test_the_table_carries_every_ladder_rung_in_game_order(self):
        self.assertLessEqual(set(ladder.LADDER), set(milestones.MILESTONES))
        tourian = ladder.LADDER[ladder.LADDER.index('tourian'):]
        self.assertEqual([name for name in milestones.MILESTONES if name in tourian], tourian)
        write_cell(self.matrix, 'ladder-seg9', 11, first_seen(tourian=1, tourian_far=300, zebetite_destroyed=None))
        name, rows = milestones.collect(self.matrix)
        self.assertEqual(rows[0]['first_execution'], {'tourian': 1, 'tourian_far': 300})

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
