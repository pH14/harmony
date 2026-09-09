#!/usr/bin/env python3
"""Verify registered native compatibility or buffering checks without emulation."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--registration', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    reg = json.loads(args.registration.read_text())
    assert sha(Path(__file__)) == reg['qualification_checker_sha256']
    panel_path = args.root / reg['output'] / 'results.json'
    panel = json.loads(panel_path.read_text())
    assert panel['registration_sha256'] == sha(args.registration)
    assert panel['execution_complete'] and panel['allocation_stop'] is None
    assert [r['id'] for r in panel['records']] == [c['id'] for c in reg['cells']]
    rows, paths = [], {}
    for record, cell in zip(panel['records'], reg['cells']):
        assert record['checks_passed'] and record['exit_code'] == 0
        summary = record['summary']
        path = args.root / reg['output'] / record['id'] / summary['cell'] / 'campaign'
        paths[record['id']] = path
        result = json.loads((path / 'result.json').read_text())
        assert result == summary['result']
        assert result['verification'] == 'campaign' and result['status'] == 'complete'
        assert summary['build']['binary_sha256'] == reg['binary_sha256']
        for key, value in cell['expected_identity'].items():
            assert summary['identity'][key] == value, key
        hashes = {name: sha(path / name) for name in
                  ('stream.jsonl', 'campaign.json', 'checkpoint.json')}
        for name, expected in cell.get('expected_artifact_sha256', {}).items():
            assert hashes[name] == expected, 'legacy compatibility: ' + record['id'] + '/' + name
        witness = 2 * (result['witness']['physical_suffix_frames'] + sum(
            x['replay']['physical_suffix_frames'] for x in result['milestone_witnesses'].values()))
        rows.append({'id': record['id'], 'admitted_qualification_frames': result['frames_emulated'],
                     'inferred_full_replay_admitted_frames': result['frames_emulated'],
                     'twice_replayed_witness_frames': witness, 'artifact_sha256': hashes,
                     'summary_sha256': record['summary_sha256'],
                     'cpu_seconds': summary['cpu_seconds'], 'elapsed_seconds': summary['elapsed_seconds']})
    for first, second in reg.get('buffer_pairs', []):
        for name in ('stream.jsonl', 'campaign.json', 'checkpoint.json'):
            assert sha(paths[first] / name) == sha(paths[second] / name), 'buffering changed ' + name
    charge = sum(row[k] for row in rows for k in ('admitted_qualification_frames',
                 'inferred_full_replay_admitted_frames', 'twice_replayed_witness_frames'))
    assert charge <= reg['known_auxiliary_limit']
    report = {'format': 'action-correlation-native-qualification-v1', 'decision': 'pass',
              'recorded_utc': datetime.now(timezone.utc).isoformat(),
              'registration_sha256': sha(args.registration), 'panel_sha256': sha(panel_path),
              'source_commit': reg['source_commit'], 'binary_sha256': reg['binary_sha256'],
              'cells': rows, 'known_auxiliary_frames': charge,
              'limitations': ['Reused qualification seed; no performance sample.',
                  'Full replay cost is inferred from admitted work; setup, reconstruction and unadmitted work remain additional unknowns.']}
    assert not args.out.exists()
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'decision': 'pass', 'cells': len(rows), 'known_auxiliary_frames': charge}))


if __name__ == '__main__':
    main()
