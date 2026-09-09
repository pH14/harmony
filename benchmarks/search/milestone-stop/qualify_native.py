#!/usr/bin/env python3
"""Verify the frozen native milestone fixtures without executing an emulator."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''): h.update(block)
    return h.hexdigest()


def read(path):
    return json.loads(path.read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--experiment', type=Path, required=True)
    parser.add_argument('--registration', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    registration = read(args.registration)
    assert sha(Path(__file__)) == registration['qualification_checker_sha256']
    root = args.experiment
    panel_path = root / registration['output'] / 'results.json'
    panel = read(panel_path)
    assert panel['registration_sha256'] == sha(args.registration)
    assert panel['execution_complete'] and panel['allocation_stop'] is None
    assert [r['id'] for r in panel['records']] == [c['id'] for c in registration['cells']]
    paths, rows = {}, []
    for record in panel['records']:
        assert record['checks_passed'] and record['exit_code'] == 0
        paths[record['id']] = root / registration['output'] / record['id'] / record['summary']['cell'] / 'campaign'
        path = paths[record['id']]
        result = read(path / 'result.json')
        assert result == record['summary']['result']
        assert result['verification'] == 'campaign' and result['status'] == 'complete'
        witness_frames = 2 * (result['witness']['physical_suffix_frames'] + sum(
            x['replay']['physical_suffix_frames'] for x in result['milestone_witnesses'].values()))
        rows.append({'id': record['id'], 'qualification_admitted_frames': result['frames_emulated'],
                     'inferred_full_replay_admitted_frames': result['frames_emulated'],
                     'twice_replayed_witness_frames': witness_frames,
                     'stop_reason': result['stop_reason'], 'first_milestone': result.get('first_milestone'),
                     'artifact_sha256': {name: sha(path / name) for name in
                         ('stream.jsonl', 'campaign.json', 'checkpoint.json', 'result.json')}})
    for name, expected in registration['old_fixture_sha256'].items():
        assert sha(paths['default'] / name) == expected, 'default compatibility: ' + name
    streams = {key: path.joinpath('stream.jsonl').read_bytes().splitlines() for key, path in paths.items()}
    baseline = streams['default'][1:]
    expected_event = {'execution': 148, 'frames_emulated': 22586}
    assert streams['energy-unreached'][1:] == baseline
    for key in ('morph-one-slot', 'morph-two-slots', 'morph-beyond-budget'):
        records = [json.loads(line) for line in streams[key][1:]]
        assert streams[key][1:] == baseline[:len(records)], 'selection prefix changed: ' + key
        jobs = [x for x in records if x['event'] == 'job']
        result = read(paths[key] / 'result.json')
        report = read(paths[key] / 'campaign.json')
        assert result['first_milestone'] == expected_event
        assert 148 <= len(jobs) <= 155
        cost = report['bootstrap_frames'] + sum(x['frames'] for x in jobs if x['sequence'] <= 148)
        assert cost == expected_event['frames_emulated']
        assert result['milestone_within_budget'] is (key != 'morph-beyond-budget')
        assert result['stop_reason'] == ('frame_limit' if key == 'morph-beyond-budget' else 'milestone')
        proof = result['milestone_witnesses']['morph_ball']['replay']['diagnostics']['named_progress']
        assert proof['first_seen']['morph_ball'] is not None
    for name in ('stream.jsonl', 'campaign.json', 'checkpoint.json'):
        assert sha(paths['morph-one-slot'] / name) == sha(paths['morph-two-slots'] / name)
    origin = read(paths['origin-brinstar'] / 'result.json')
    assert origin['executions'] == 0 and origin['first_milestone'] == {'execution': 0, 'frames_emulated': 0}
    assert len(streams['origin-brinstar']) == 1
    prior_path = root / registration['prior_ledger']
    assert sha(prior_path) == registration['prior_ledger_sha256']
    prior = read(prior_path)
    charge = sum(row[k] for row in rows for k in ('qualification_admitted_frames',
        'inferred_full_replay_admitted_frames', 'twice_replayed_witness_frames'))
    assert charge <= registration['additional_auxiliary_limit']
    total = prior['known_auxiliary_frame_charges'] + charge
    assert total <= prior['nominal_limits']['auxiliary']
    output = {'format': 'milestone-stop-qualification-v1', 'decision': 'pass',
              'as_of_utc': datetime.now(timezone.utc).isoformat(),
              'registration_sha256': sha(args.registration), 'panel_sha256': sha(panel_path),
              'source_commit': registration['source_commit'], 'binary_sha256': registration['binary_sha256'],
              'cells': rows, 'additional_known_auxiliary_frames': charge,
              'cumulative_known_auxiliary_frames': total,
              'cumulative_search_admitted_frames': prior['completed_search_admitted_frames'],
              'limitations': prior['limitations'] + ['Reused qualification seed; no performance estimate.']}
    assert not args.out.exists()
    args.out.write_text(json.dumps(output, indent=2) + '\n')
    print(json.dumps({k: v for k, v in output.items() if k not in ('cells', 'limitations')}, indent=2))


if __name__ == '__main__':
    main()
