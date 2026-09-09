#!/usr/bin/env python3
"""Combine frozen host-local triplets and apply both registered allocation gates."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'continuation-yield'))
from named_milestone_endpoint import named_milestone_endpoint
from score_screen import score_screen


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--registration', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    reg = json.loads(args.registration.read_text())
    assert sha(Path(__file__)) == reg['scorer_sha256']
    records, panels, costs = [], [], []
    failure = False
    for spec in reg['triplets']:
        subpath = args.registration.parent / spec['registration']
        assert sha(subpath) == spec['registration_sha256']
        sub = json.loads(subpath.read_text())
        evidence = args.evidence / (spec['id'] + '-results.json')
        if not evidence.exists():
            continue
        panel = json.loads(evidence.read_text())
        assert panel['registration_sha256'] == sha(subpath)
        assert [r['id'] for r in panel['records']] == [c['id'] for c in sub['cells'][:len(panel['records'])]]
        assert len(panel['records']) <= len(sub['cells'])
        failure |= panel['allocation_stop'] is not None
        panels.append({'id': spec['id'], 'sha256': sha(evidence),
                       'execution_complete': panel['execution_complete'], 'allocation_stop': panel['allocation_stop']})
        for record, cell in zip(panel['records'], sub['cells']):
            if not record.get('checks_passed'):
                failure |= 'failure' in record
                continue
            assert record['exit_code'] == 0
            summary = record['summary']
            assert summary['host']['hostname'] == spec['host']
            assert summary['build']['binary_sha256'] == sub['binary_sha256']
            for key, value in cell['expected_identity'].items():
                assert summary['identity'][key] == value, key
            endpoint = named_milestone_endpoint(summary, reg['horizon_frames'], reg['milestone_stop'])
            assert endpoint == record['endpoint_evidence']
            result = summary['result']
            if endpoint['hit_by_budget']:
                proof = result['milestone_witnesses'][reg['milestone_stop']['name']]['replay']
                assert proof['diagnostics']['named_progress']['first_seen'][reg['milestone_stop']['name']] is not None
            auxiliary = 2 * (result['witness']['physical_suffix_frames'] + sum(
                x['replay']['physical_suffix_frames'] for x in result['milestone_witnesses'].values()))
            costs.append({'id': record['id'], 'admitted_search_frames': result['frames_emulated'],
                          'known_auxiliary_frames': auxiliary, 'endpoint': endpoint,
                          'cpu_seconds': summary['cpu_seconds'], 'elapsed_seconds': summary['elapsed_seconds']})
            records.append(record)
    scores = {name: score_screen(records, {'screen': rule}) for name, rule in reg['comparisons'].items()}
    decisions = [v['decision'] for v in scores.values()]
    search = sum(c['admitted_search_frames'] for c in costs)
    auxiliary = sum(c['known_auxiliary_frames'] for c in costs)
    if failure:
        decision = 'stop_measurement_failure'
    elif auxiliary > reg['known_auxiliary_limit']:
        decision = 'stop_auxiliary_limit'
    elif any(d.startswith('fail') for d in decisions):
        decision = 'stop_action_correlation_line'
    elif all(d == 'pass' for d in decisions):
        assert len(costs) == 12
        decision = 'earns_independent_confirmation'
    else:
        decision = 'continue_registered_panel'
    out = {'format': 'action-correlation-development-analysis-v1',
           'recorded_utc': datetime.now(timezone.utc).isoformat(),
           'registration_sha256': sha(args.registration), 'scorer_sha256': sha(Path(__file__)),
           'decision': decision, 'comparisons': scores, 'panels': panels, 'cells': costs,
           'actual_admitted_search_frames': search, 'known_auxiliary_frames': auxiliary,
           'limits': 'Allocation gates, not significance or a boss/Wily result. Setup, unadmitted work and unfinished cells can add unmeasured physical work.'}
    assert not args.out.exists()
    args.out.write_text(json.dumps(out, indent=2) + '\n')
    print(json.dumps({k: out[k] for k in ('decision', 'actual_admitted_search_frames', 'known_auxiliary_frames')}))


if __name__ == '__main__':
    main()
