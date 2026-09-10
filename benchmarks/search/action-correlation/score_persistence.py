#!/usr/bin/env python3
"""Score the prospectively registered persistence panel, separately from D01."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sys

HELPERS = Path(__file__).resolve().parent.parent / 'continuation-yield'
sys.path.insert(0, str(HELPERS))
from named_milestone_endpoint import named_milestone_endpoint
from score_screen import score_screen


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def score(registration, evidence):
    reg = json.loads(registration.read_text())
    assert sha(Path(__file__)) == reg['scorer_sha256']
    for name, expected in reg['helper_sha256'].items():
        assert sha(HELPERS / name) == expected
    records, panels, costs, incomplete_work = [], [], [], []
    failed = False
    expected_cells = 0
    for spec in reg['panels']:
        subpath = registration.parent / spec['registration']
        assert sha(subpath) == spec['registration_sha256']
        sub = json.loads(subpath.read_text())
        expected_cells += len(sub['cells'])
        path = evidence / (spec['id'] + '-results.json')
        if not path.exists():
            continue
        panel = json.loads(path.read_text())
        assert panel['registration_sha256'] == sha(subpath)
        assert [r['id'] for r in panel['records']] == [c['id'] for c in sub['cells'][:len(panel['records'])]]
        assert len(panel['records']) <= len(sub['cells'])
        failed |= panel['allocation_stop'] is not None
        panels.append({'id': spec['id'], 'sha256': sha(path),
                       'execution_complete': panel['execution_complete'],
                       'allocation_stop': panel['allocation_stop']})
        for record, cell in zip(panel['records'], sub['cells']):
            summary = record.get('summary') or {}
            result = summary.get('result') or {}
            cost = {'id': record['id'], 'admitted_search_frames': result.get('frames_emulated'),
                    'known_auxiliary_frames': None}
            if record.get('checks_passed'):
                assert record['exit_code'] == 0 and summary['status'] == 'complete'
                assert summary['host']['hostname'] == spec['host']
                assert summary['build']['binary_sha256'] == sub['binary_sha256']
                for key, expected in cell['expected_identity'].items():
                    assert summary['identity'][key] == expected, key
                endpoint = named_milestone_endpoint(summary, reg['horizon_frames'], reg['milestone_stop'])
                assert endpoint == record['endpoint_evidence']
                if endpoint['hit_by_budget']:
                    proof = result['milestone_witnesses'][reg['milestone_stop']['name']]['replay']
                    assert proof['diagnostics']['named_progress']['first_seen'][reg['milestone_stop']['name']] is not None
                cost.update(known_auxiliary_frames=2 * (result['witness']['physical_suffix_frames'] + sum(
                    x['replay']['physical_suffix_frames'] for x in result['milestone_witnesses'].values())),
                    endpoint=endpoint, cpu_seconds=summary['cpu_seconds'], elapsed_seconds=summary['elapsed_seconds'])
                records.append(record)
            else:
                failed |= 'failure' in record
                incomplete_work.append({'id': record['id'], 'scope': 'Available admitted work is charged; unfinished/replay/setup advancement remains unknown.'})
            costs.append(cost)
    assert expected_cells == 8
    result = score_screen(records, {'screen': reg['screen']})
    search = sum(c['admitted_search_frames'] or 0 for c in costs)
    auxiliary = sum(c['known_auxiliary_frames'] or 0 for c in costs)
    if failed:
        decision = 'stop_measurement_failure'
    elif search > reg['nominal_search_limit'] + expected_cells * reg['bounded_inflight_drain_per_cell']:
        decision = 'stop_search_limit'
    elif auxiliary > reg['known_auxiliary_limit']:
        decision = 'stop_auxiliary_limit'
    elif result['decision'].startswith('fail'):
        decision = 'stop_persistence_line'
    elif result['decision'] == 'pass':
        assert len(records) == expected_cells and all(p['execution_complete'] for p in panels)
        decision = 'earns_transfer_and_depth_decision'
    else:
        decision = 'continue_registered_panel'
    return {'format': 'action-persistence-p01-analysis-v1',
            'recorded_utc': datetime.now(timezone.utc).isoformat(),
            'registration_sha256': sha(registration), 'scorer_sha256': sha(Path(__file__)),
            'decision': decision, 'screen': result, 'panels': panels, 'cells': costs,
            'actual_admitted_search_frames': search, 'known_auxiliary_frames': auxiliary,
            'incomplete_work': incomplete_work,
            'limits': 'Prospective allocation gate for an exploratory nominee; D01 is excluded. This is not population significance or a boss/Wily result. Setup and unadmitted work remain additional unknowns.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--registration', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    result = score(args.registration, args.evidence)
    assert not args.out.exists()
    args.out.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({key: result[key] for key in ('decision', 'actual_admitted_search_frames', 'known_auxiliary_frames')}))


if __name__ == '__main__':
    main()
