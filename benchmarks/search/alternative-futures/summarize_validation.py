#!/usr/bin/env python3
"""Summarize the frozen Metroid panel without treating partial work as an outcome."""
import argparse
from bisect import bisect_left, bisect_right
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path

BOSSES = ('kraid_defeated', 'ridley_defeated', 'mother_brain_defeated')


def read(path):
    return json.loads(path.read_text())


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def progress(path):
    if not path.exists():
        return []
    rows = []
    with path.open() as stream:
        for line in stream:
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                # A live writer can leave its final line incomplete.
                if stream.read():
                    raise
    assert all(a['executions'] <= b['executions'] and
               a['frames_emulated'] <= b['frames_emulated']
               for a, b in zip(rows, rows[1:])), 'nonmonotone progress'
    return rows


def work_bounds(points, first_execution):
    executions = [point['executions'] for point in points]
    index = bisect_left(executions, first_execution)
    return {
        'first_execution': first_execution,
        'admitted_frames_lower': points[index - 1]['frames_emulated'] if index else 0,
        'admitted_frames_upper': points[index]['frames_emulated'] if index < len(points) else None,
    }


def at_frame(points, boundary):
    index = bisect_right([point['frames_emulated'] for point in points], boundary) - 1
    if index < 0:
        return None
    point = points[index]
    workload = point.get('workload_diagnostics', {})
    named = workload.get('named_progress', {})
    return {'executions': point['executions'], 'frames_emulated': point['frames_emulated'],
            'map_cells_observed': workload.get('map_cells_observed'),
            'max_missile_capacity': named.get('max_missile_capacity'),
            'max_energy_tanks': named.get('max_energy_tanks'),
            'observed_named_milestones': [key for key, value in named.get('first_seen', {}).items()
                                         if value is not None]}


def summarize(root, plan):
    rows, points_by_label = [], {}
    launcher = root / 'validation' / plan['id']
    for cell in plan['cells']:
        label, seed, arm = cell['label'], cell['seed'], cell['arm']
        directory = root / 'runs' / label / f'metroid-full-s{seed}-w4-m8192'
        summary_path = directory / 'summary.json'
        execution_path = launcher / (label + '-execution.json')
        summary = read(summary_path) if summary_path.exists() else {}
        execution = read(execution_path) if execution_path.exists() else {}
        result = summary.get('result') or {}
        points = progress(directory / 'campaign' / 'progress.jsonl')
        points_by_label[label] = points
        latest = summary.get('last_progress') or (points[-1] if points else {})
        if result and (not points or result['executions'] > points[-1]['executions']):
            points.append({**latest, 'executions': result['executions'],
                           'frames_emulated': result['frames_emulated']})
        if summary:
            identity = summary.get('identity')
            if summary.get('status') == 'complete':
                assert isinstance(identity, dict) and result, 'completed outcome lacks provenance'
            if identity is not None:
                assert identity['seed'] == seed and identity['prefix_sha256'] is None
                assert identity['source_tree_sha256'] == plan['source_tree_sha256']
            if summary.get('build'):
                assert summary['build']['binary_sha256'] == plan['binary_sha256']
            else:
                assert summary.get('status') != 'complete', 'completed outcome lacks build identity'
            for key, expected in cell['suite']['search'].items():
                assert summary['search_request'][key] == expected, (label, key)
        complete = (summary.get('status') == 'complete' and
                    execution.get('status') == 'runner_complete' and
                    (result.get('executions', 0) >= 3_000_000 or
                     result.get('frames_emulated', 0) >= 400_000_000 or
                     result.get('solved') is True))
        named = latest.get('workload_diagnostics', {}).get('named_progress', {})
        first_seen = named.get('first_seen', {})
        witnesses = result.get('milestone_witnesses', {})
        milestones = {}
        for name in sorted(set(first_seen) | set(witnesses)):
            discovery = first_seen.get(name)
            if discovery is None and name not in witnesses:
                continue
            witness = witnesses.get(name, {})
            replayed = witness.get('replay', {}).get('diagnostics', {}).get('named_progress', {}).get('first_seen', {}).get(name)
            bounds = (work_bounds(points, discovery['execution']) if discovery else
                      {'first_execution': None, 'admitted_frames_lower': latest.get('frames_emulated', 0),
                       'admitted_frames_upper': result.get('frames_emulated')})
            milestones[name] = {**bounds,
                                'verified_by_completed_runner': summary.get('status') == 'complete' and replayed is not None,
                                'within_registered_work': bounds['admitted_frames_upper'] is not None and
                                bounds['admitted_frames_upper'] <= 400_000_000 and
                                (discovery['execution'] if discovery else result.get('executions', 3_000_001)) <= 3_000_000,
                                'input_sha256': witness.get('input_sha256')}
        verified_bosses = [name for name in BOSSES if milestones.get(name, {}).get('verified_by_completed_runner')]
        row = {'label': label, 'seed': seed, 'arm': arm,
               'anchor_complete': complete, 'runner_status': summary.get('status'),
               'launcher_status': execution.get('status', 'active_or_not_started'),
               'failure': {key: summary[key] for key in ['error', 'exit_code', 'timeout'] if key in summary},
               'identity_available': isinstance(summary.get('identity'), dict),
               'summary_sha256': sha(summary_path) if summary else None,
               'execution_sha256': sha(execution_path) if execution else None,
               'executions': result.get('executions', latest.get('executions', 0)),
               'admitted_frames': result.get('frames_emulated', latest.get('frames_emulated', 0)),
               'stop_reason': result.get('stop_reason'), 'stream_sha256': result.get('stream_sha256'),
               'milestones': milestones, 'verified_bosses': verified_bosses,
               'completed_boss_success': complete and any(milestones[name]['within_registered_work'] for name in verified_bosses),
               'map_cells_observed': latest.get('workload_diagnostics', {}).get('map_cells_observed'),
               'max_missile_capacity': named.get('max_missile_capacity'),
               'max_energy_tanks': named.get('max_energy_tanks'),
               'cpu_seconds': summary.get('cpu_seconds'),
               'peak_process_tree_rss_bytes_sampled': summary.get('peak_process_tree_rss_bytes_sampled')}
        rows.append(row)
    paired = []
    for seed in plan['seeds']:
        pair = {row['arm']: row for row in rows if row['seed'] == seed}
        assert set(pair) == {'control', 'corrected'}
        boundary = min(row['admitted_frames'] for row in pair.values())
        paired.append({'seed': seed, 'both_anchor_complete': all(row['anchor_complete'] for row in pair.values()),
                       'common_admitted_frame_boundary': boundary,
                       'at_common_frame_work': {arm: at_frame(points_by_label[row['label']], boundary)
                                                for arm, row in pair.items()}})
    return {'format': 'alternative-futures-validation-assessment-v1',
            'time_utc': datetime.now(timezone.utc).isoformat(), 'plan_id': plan['id'],
            'limits': ['Incomplete cells are neither failures nor completed successes.',
                       'The frozen evaluator replays each named witness twice on independent ordinary-genesis targets before reporting complete.',
                       'Milestone admitted work is bounded by adjacent progress samples; route duration is not search work.',
                       'A boss whose work bound overlaps the admitted-frame limit is preserved as a verified witness but is not counted as an in-budget success.',
                       'Common-frame measurements use the last completed sample at or below the stated boundary.',
                       'Capability and coverage counts are campaign-wide observations, not necessarily one trajectory.'],
            'panel_complete': all(row['anchor_complete'] for row in rows),
            'completed_by_arm': {arm: sum(row['anchor_complete'] for row in rows if row['arm'] == arm)
                                 for arm in ['control', 'corrected']},
            'completed_boss_successes_by_arm': {arm: sum(row['completed_boss_success'] for row in rows if row['arm'] == arm)
                                              for arm in ['control', 'corrected']},
            'cells': rows, 'paired_common_work': paired}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('plan', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    value = summarize(args.root, read(args.plan))
    value['plan_sha256'] = sha(args.plan)
    with args.output.open('x') as output:
        output.write(json.dumps(value, indent=2) + '\n')
    print(json.dumps({key: value[key] for key in ['panel_complete', 'completed_by_arm', 'completed_boss_successes_by_arm']}))


if __name__ == '__main__':
    main()
