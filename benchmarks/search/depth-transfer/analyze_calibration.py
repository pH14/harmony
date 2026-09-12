#!/usr/bin/env python3
"""Check the frozen four-control endpoint calibration without emulation."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'continuation-yield'))
from named_milestone_endpoint import named_milestone_endpoint, validate_named_panel


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--registration', type=Path, required=True)
    parser.add_argument('--results', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    registration = json.loads(args.registration.read_text())
    panel = json.loads(args.results.read_text())
    condition = validate_named_panel(registration)
    rule = registration['calibration_gate']
    assert condition['name'] == rule['endpoint']
    assert panel['registration_sha256'] == sha(args.registration)
    assert panel['execution_complete'] and panel['allocation_stop'] is None
    assert len(panel['records']) == len(registration['cells']) == rule['total_cells'] == 4
    rows = []
    for record, cell in zip(panel['records'], registration['cells']):
        assert record['id'] == cell['id'] and record['checks_passed'] and record['exit_code'] == 0
        summary = record['summary']
        result = summary['result']
        for key, value in cell['expected_identity'].items():
            assert summary['identity'][key] == value, key
        assert summary['build']['binary_sha256'] == registration['binary_sha256']
        endpoint = named_milestone_endpoint(summary, rule['horizon_frames'], condition)
        assert endpoint == record['endpoint_evidence']
        assert endpoint['restricted_cost_interval'] is not None
        if endpoint['hit_by_budget']:
            witness = result['milestone_witnesses'][condition['name']]['replay']
            assert witness['diagnostics']['named_progress']['first_seen'][condition['name']] is not None
        auxiliary = 2 * (result['witness']['physical_suffix_frames'] + sum(
            x['replay']['physical_suffix_frames'] for x in result['milestone_witnesses'].values()))
        rows.append({'id': record['id'], 'seed': summary['identity']['seed'],
                     'hit': endpoint['hit_by_budget'], 'endpoint': endpoint,
                     'actual_admitted_frames': result['frames_emulated'],
                     'frames_after_event_or_horizon': result['frames_emulated'] - endpoint['restricted_cost_interval'][1],
                     'twice_replayed_witness_frames': auxiliary,
                     'cpu_seconds': summary['cpu_seconds'], 'elapsed_seconds': summary['elapsed_seconds'],
                     'summary_sha256': record['summary_sha256']})
    hits = sum(row['hit'] for row in rows)
    report = {'format': 'depth-transfer-c01-analysis-v1',
              'analyzed_utc': datetime.now(timezone.utc).isoformat(),
              'registration_sha256': sha(args.registration), 'results_sha256': sha(args.results),
              'analyzer_sha256': sha(Path(__file__)),
              'decision': 'pass' if hits >= rule['minimum_hits'] else 'fail',
              'hits': hits, 'controls': len(rows), 'cells': rows,
              'actual_admitted_search_frames': sum(row['actual_admitted_frames'] for row in rows),
              'known_auxiliary_frames': sum(row['twice_replayed_witness_frames'] for row in rows),
              'interpretation': 'Endpoint allocation calibration, not a candidate comparison or a power guarantee.',
              'unmeasured_work': 'Setup and unadmitted emulator work are additional; no full physical-frame accounting claim.'}
    assert not args.out.exists()
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({k: report[k] for k in ('decision', 'hits', 'actual_admitted_search_frames', 'known_auxiliary_frames')}))


if __name__ == '__main__':
    main()
