#!/usr/bin/env python3
"""Recover historical milestone frame intervals; never run a target or select seeds."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path


HERE = Path(__file__).resolve().parent
HISTORY = HERE.parent / 'results/remine-012.json'


def raw(path):
    if path.exists():
        return path.read_bytes()
    return gzip.decompress(Path(str(path) + '.gz').read_bytes())


def sha(data):
    return hashlib.sha256(data).hexdigest()


def audit(evidence):
    history = json.loads(HISTORY.read_bytes())
    cells = []
    for arm in ('control', 'continuation'):
        panel = history['panels']['metroid-long-' + arm]
        for seed in (3, 4, 5):
            cell = f'metroid-full-s{seed}-w4-m8192'
            folder = evidence / f'metroid-long-{arm}-012' / cell
            summary_bytes = raw(folder / 'summary.json')
            summary = json.loads(summary_bytes)
            published = next(c for c in panel['cells'] if c['cell'] == cell)
            # The historical publication retained witness hashes and added
            # selector accounting; raw summaries retain the full replay records.
            projected = json.loads(summary_bytes)
            witnesses = projected['result'].pop('milestone_witnesses')
            projected['result']['milestone_witness_inputs'] = {
                name: witness['input_sha256'] for name, witness in witnesses.items()
            }
            assert projected == {k: v for k, v in published.items() if k != 'selector_accounting'}
            result_bytes = raw(folder / 'campaign/result.json')
            assert json.loads(result_bytes) == summary['result']
            assert summary['status'] == summary['result']['status'] == 'complete'
            assert summary['result']['verification'] == 'witness'
            first = summary['last_progress']['workload_diagnostics']['named_progress']['first_seen']
            bounds = {name: {'lower': None, 'upper': None} for name in first}
            brackets = {name: {'before': None, 'after': None} for name in first}
            progress_bytes = raw(folder / 'campaign/progress.jsonl')
            previous = (0, 0)
            count = 0
            for line in progress_bytes.splitlines():
                record = json.loads(line)
                execution, frames = record['executions'], record['frames_emulated']
                assert execution >= previous[0] and frames >= previous[1]
                previous = (execution, frames)
                count += 1
                observed = record['workload_diagnostics']['named_progress']
                assert observed['format'] == 'metroid-named-progress-v2'
                for name, event in first.items():
                    expected = event if event and execution >= event['execution'] else None
                    assert observed['first_seen'][name] == expected
                    if event is None:
                        continue
                    point = {'execution': execution, 'frames': frames}
                    if execution < event['execution']:
                        bounds[name]['lower'] = frames + 1
                        brackets[name]['before'] = point
                    elif bounds[name]['upper'] is None:
                        bounds[name]['upper'] = frames
                        brackets[name]['after'] = point
                        if execution == event['execution']:
                            bounds[name]['lower'] = frames
            assert record == summary['last_progress']
            assert previous == (summary['result']['executions'], summary['result']['frames_emulated'])
            endpoints = {}
            for name, event in first.items():
                if event is not None:
                    lower, upper = bounds[name]['lower'], bounds[name]['upper']
                    assert lower is not None and lower <= upper
                    assert name in witnesses
                    assert witnesses[name]['replay']['diagnostics']['named_progress']['first_seen'][name] is not None
                    interval = [lower, upper]
                else:
                    assert name not in witnesses
                    interval = None
                endpoints[name] = {'first_execution': event['execution'] if event else None,
                                   'arrival_frames_interval': interval,
                                   'right_censored_after_frames': previous[1] if event is None else None,
                                   'bracket': brackets[name]}
            cells.append({'arm': arm, 'seed': seed, 'cell': cell,
                          'summary_sha256': sha(summary_bytes), 'result_sha256': sha(result_bytes),
                          'progress_sha256': sha(progress_bytes), 'progress_bytes': len(progress_bytes),
                          'progress_records': count, 'identity': summary['identity'],
                          'cpu_set': summary['cpu_set'], 'endpoints': endpoints})
    comparisons = {}
    for name in cells[0]['endpoints']:
        pairs = []
        for seed in (3, 4, 5):
            control, candidate = [next(c for c in cells if c['arm'] == arm and c['seed'] == seed)
                                  ['endpoints'][name]['arrival_frames_interval']
                                  for arm in ('control', 'continuation')]
            pairs.append({'seed': seed, 'control': control, 'continuation': candidate,
                          'strict_win': candidate[1] < control[0] if control and candidate else None})
        ratio = None
        if all(p['control'] and p['continuation'] for p in pairs):
            ratio = [sum(p['continuation'][0] for p in pairs) / sum(p['control'][1] for p in pairs),
                     sum(p['continuation'][1] for p in pairs) / sum(p['control'][0] for p in pairs)]
        comparisons[name] = {'pairs': pairs, 'all_attained_mean_cost_ratio_interval': ratio}
    reversal_seeds = []
    for early, later in zip(comparisons['energy_tank']['pairs'], comparisons['bombs']['pairs']):
        assert early['seed'] == later['seed']
        if early['continuation'][0] > early['control'][1] and later['strict_win']:
            reversal_seeds.append(early['seed'])
    return {'format': 'historical-continuation-frame-audit-v1',
            'historical_record_sha256': sha(HISTORY.read_bytes()), 'cells': cells,
            'comparisons': comparisons, 'energy_loss_but_bombs_win_seeds': reversal_seeds,
            'new_emulator_frames': 0,
            'limits': ['Retrospective reused development seeds; no fresh confirmation or new allocation gate.',
                       'Intervals bound checkpoint timing, not population uncertainty; no within-job arrival claim.',
                       'Original arms used different CPU sets concurrently; no matched wall/CPU efficacy claim.',
                       'All six use legacy terminal v2; current corrected-terminal efficacy remains unmeasured.',
                       'No boss defeat is observed; earlier milestone gains do not qualify untouched validation.',
                       'Unattained endpoints remain censored; no success-only means are computed for them.']}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', type=Path, default=HERE / 'evidence')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.out.write_text(json.dumps(audit(args.evidence), indent=2) + '\n')
