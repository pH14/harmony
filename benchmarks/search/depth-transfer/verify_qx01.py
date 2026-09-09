#!/usr/bin/env python3
"""Verify x86 replay, the same-host prefix contract, and qualification cost."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read(path):
    return json.loads(path.read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    root = args.root
    registration = read(root / 'qx01-registration.json')
    panel_path = root / registration['output'] / 'results.json'
    panel = read(panel_path)
    assert panel['registration_sha256'] == sha(root / 'qx01-registration.json')
    assert panel['execution_complete'] and panel['allocation_stop'] is None
    assert [r['id'] for r in panel['records']] == [c['id'] for c in registration['cells']]
    paths, rows = {}, []
    for record, cell in zip(panel['records'], registration['cells']):
        assert record['checks_passed'] and record['exit_code'] == 0
        path = root / registration['output'] / record['id'] / record['summary']['cell'] / 'campaign'
        paths[record['id']] = path
        result = read(path / 'result.json')
        assert result == record['summary']['result']
        assert result['status'] == 'complete' and result['verification'] == 'campaign'
        for key, value in cell['expected_result'].items():
            assert result[key] == value, key
        for key, value in cell['expected_identity'].items():
            assert record['summary']['identity'][key] == value, key
        witness_frames = 2 * (result['witness']['physical_suffix_frames'] + sum(
            x['replay']['physical_suffix_frames'] for x in result['milestone_witnesses'].values()))
        rows.append({'id': record['id'], 'qualification_admitted_frames': result['frames_emulated'],
                     'inferred_full_replay_admitted_frames': result['frames_emulated'],
                     'twice_replayed_witness_frames': witness_frames,
                     'artifact_sha256': {name: sha(path / name) for name in
                                         ('stream.jsonl', 'campaign.json', 'checkpoint.json', 'result.json')}})
    baseline = paths['default'].joinpath('stream.jsonl').read_bytes().splitlines()[1:]
    for key in ('morph-one-slot', 'morph-two-slots'):
        path = paths[key]
        lines = path.joinpath('stream.jsonl').read_bytes().splitlines()[1:]
        assert lines == baseline[:len(lines)], 'pre-event prefix changed'
        jobs = [json.loads(line) for line in lines if json.loads(line)['event'] == 'job']
        assert len(jobs) == 155
        report = read(path / 'campaign.json')
        cost = report['bootstrap_frames'] + sum(x['frames'] for x in jobs if x['sequence'] <= 148)
        assert cost == 22586
        result = read(path / 'result.json')
        assert result['frames_emulated'] == 23883
        assert result['milestone_witnesses']['morph_ball']['replay']['diagnostics']['named_progress']['first_seen']['morph_ball'] is not None
    for name in ('stream.jsonl', 'campaign.json', 'checkpoint.json'):
        assert sha(paths['morph-one-slot'] / name) == sha(paths['morph-two-slots'] / name)
    charge = sum(row[k] for row in rows for k in
                 ('qualification_admitted_frames', 'inferred_full_replay_admitted_frames', 'twice_replayed_witness_frames'))
    assert charge <= registration['additional_auxiliary_limit']
    output = {'format': 'depth-transfer-qx01-qualification-v1', 'decision': 'pass',
              'verified_utc': datetime.now(timezone.utc).isoformat(),
              'registration_sha256': sha(root / 'qx01-registration.json'),
              'panel_sha256': sha(panel_path), 'checker_sha256': sha(Path(__file__)),
              'source_sha256': registration['source_tree_sha256'],
              'binary_sha256': registration['binary_sha256'], 'core_sha256': registration['core_sha256'],
              'cells': rows, 'known_auxiliary_frame_charge': charge,
              'launch_commit_label': panel['registration_commit'],
              'limitations': ['Reused short fixture, not performance evidence or universal cross-platform identity.',
                              'See qx01-registration-binding.json for the preserved launch-label correction.',
                              'Full-replay cost is inferred from admitted work; setup and unadmitted physical work remain unmeasured.']}
    assert not args.out.exists()
    args.out.write_text(json.dumps(output, indent=2) + '\n')
    print(json.dumps({'decision': 'pass', 'known_auxiliary_frames': charge}))


if __name__ == '__main__':
    main()
