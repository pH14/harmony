#!/usr/bin/env python3
"""Recompute the fixed endpoint qualification gate from published compact evidence."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def score(root):
    master = json.loads((root/'q01-registration.json').read_text())
    rows, cost = [], 0
    for sub in master['subregistrations']:
        path = root/sub['file']
        assert sha(path) == sub['sha256']
        analysis_path = root/(path.stem+'-analysis.json')
        analysis = json.loads(analysis_path.read_text())
        assert analysis['decision'] == 'pass' and analysis['registration_sha256'] == sub['sha256']
        raw = gzip.decompress((root/(path.stem+'-results.json.gz')).read_bytes())
        assert hashlib.sha256(raw).hexdigest() == analysis['panel_sha256']
        panel = json.loads(raw)
        assert panel['registration_sha256'] == sub['sha256']
        assert panel['execution_complete'] and panel['allocation_stop'] is None
        assert len(panel['records']) == len(analysis['cells']) == sub['cells']
        for record, cell in zip(panel['records'], analysis['cells']):
            assert record['checks_passed'] and record['exit_code'] == 0
            assert record['summary']['result']['frames_emulated'] == cell['admitted_qualification_frames']
            assert record['summary']['build']['binary_sha256'] == analysis['binary_sha256']
            rows.append({'host':sub['host'],'registration':path.name,**cell})
        cost += analysis['known_auxiliary_frames']
    assert len(rows) == 4 and cost <= master['known_auxiliary_ceiling']
    baseline = rows[0]
    assert baseline['endpoint_counts'] is None
    for row in rows[1:]:
        for key in baseline:
            if key.startswith('normalized_'):
                assert row[key] == baseline[key], (key, row['host'])
        assert row['snapshot_count'] == baseline['snapshot_count']
        delta = row['memory']['resident_snapshot_bytes'] - baseline['memory']['resident_snapshot_bytes']
        assert 0 <= delta <= 16 * row['snapshot_count'] and delta % row['snapshot_count'] == 0
        assert row['memory']['resident_memory_bytes'] - baseline['memory']['resident_memory_bytes'] == delta
    assert rows[2]['artifact_sha256'] == rows[3]['artifact_sha256']
    assert all(r['endpoint_counts'] == rows[1]['endpoint_counts'] for r in rows[1:])
    assert cost == sum(r[k] for r in rows for k in ('admitted_qualification_frames','inferred_full_replay_frames','twice_replayed_witness_frames'))
    return {'format':'metroid-endpoint-q01-score-v1','decision':'pass',
            'master_registration_sha256':sha(root/'q01-registration.json'),
            'known_auxiliary_frames':cost,'cells':rows,
            'memory_delta_per_snapshot':(rows[1]['memory']['resident_snapshot_bytes']-baseline['memory']['resident_snapshot_bytes'])//baseline['snapshot_count'],
            'claim':'Short-fixture default compatibility, full replay, audited buffering and projected cross-host physical/decision equality.',
            'unqualified':'Positive native encounters, fresh-search performance and neutrality at memory/wall limits.'}


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root',type=Path,default=Path(__file__).parent)
    parser.add_argument('--out',type=Path,required=True)
    args=parser.parse_args()
    assert not args.out.exists()
    args.out.write_text(json.dumps(score(args.root),indent=2)+'\n')
