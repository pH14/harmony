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
    initial_registration_path = root / registration['initial_registration']
    assert sha(initial_registration_path) == registration['initial_registration_sha256']
    initial_registration = read(initial_registration_path)
    initial_panel_path = root / initial_registration['output'] / 'results.json'
    initial_panel = read(initial_panel_path)
    assert initial_panel['registration_sha256'] == sha(initial_registration_path)
    assert not initial_panel['execution_complete']
    assert initial_panel['allocation_stop'] == 'cell failure; no subsequent cells dispatched'
    assert [r['id'] for r in initial_panel['records']] == [c['id'] for c in initial_registration['cells']]
    failed = initial_panel['records'][-1]
    assert failed['id'] == 'origin-brinstar'
    assert failed['failure'] == 'frozen compatibility result differs: executions'
    panel_path = root / registration['output'] / 'results.json'
    panel = read(panel_path)
    assert panel['registration_sha256'] == sha(args.registration)
    assert panel['execution_complete'] and panel['allocation_stop'] is None
    assert [r['id'] for r in panel['records']] == [c['id'] for c in registration['cells']]
    paths, rows = {}, []
    records = [(record, initial_registration['output']) for record in initial_panel['records']]
    records += [(record, registration['output']) for record in panel['records']]
    for record, output in records:
        assert record['exit_code'] == 0
        if record['id'] != 'origin-brinstar': assert record['checks_passed']
        paths[record['id']] = root / output / record['id'] / record['summary']['cell'] / 'campaign'
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
    for key in ('origin-brinstar', 'first-admission-brinstar'):
        origin = read(paths[key] / 'result.json')
        assert origin['executions'] == 8 and origin['first_milestone'] == {'execution': 1, 'frames_emulated': 171}
        assert streams[key][1:] == baseline[:8]
    for name in ('stream.jsonl', 'campaign.json', 'checkpoint.json'):
        assert sha(paths['origin-brinstar'] / name) == sha(paths['first-admission-brinstar'] / name)
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
              'initial_panel_sha256': sha(initial_panel_path),
              'preserved_failed_expectation': 'Q01 assumed genesis had an observation. Metroid campaign genesis retains state without observing it; Brinstar first appears at admission 1. Q01r freezes that existing observation contract and rechecks the unchanged binary.',
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
