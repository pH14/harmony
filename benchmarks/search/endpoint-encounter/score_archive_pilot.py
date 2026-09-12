#!/usr/bin/env python3
"""Classify the fixed AP01 horizon separately from native process completion."""
import argparse
import hashlib
import json
from pathlib import Path

from score_s01 import raw


def read(root, name):
    return json.loads(raw(root, name))


def sha(data):
    return hashlib.sha256(data).hexdigest()


def classify(first, executions, frames, horizon):
    success = bool(first and 0 < first['execution'] <= executions
                   and 0 < first['frames_emulated'] <= horizon)
    complete = success or frames >= horizon
    return success, complete, ('conditional_capability' if success else
                               'no_defeat_at_horizon' if complete else 'incomplete')


def score(protocol, output, request_name='ap01-request.json', registration_name='ap01-registration.json'):
    q = read(protocol, request_name)
    registration = read(protocol, registration_name)
    report = read(output, 'result.json')
    usage = read(output, 'usage.json')
    assert usage['completed_execution'] and usage['error'] is None
    assert usage['request_sha256'] == sha(raw(protocol, request_name))
    assert report['root']['snapshot_sha256'] == q['expected_snapshot_sha256']
    assert sha(raw(output, 'root-snapshot.json')) == read(protocol, 'aq01-prepare-analysis.json')['root_json_sha256']
    assert report['origin']['kind'] == 'snapshot_root'
    assert report['milestone'] == q['milestone'] == 'ridley_defeated'
    assert report['full_campaign_replay']
    assert report['root_local_witness_replays'] == report['complete_prefix_witness_replays'] == 2
    assert report['stream_sha256'] == sha(raw(output, 'stream.jsonl'))
    campaign = read(output, 'campaign.json')
    assert campaign['executions_completed'] == report['executions']
    assert campaign['frames_emulated'] == report['frames']
    assert campaign.get('first_milestone') == report['first_milestone']
    assert campaign['origin'] == report['origin']
    assert report['executions'] <= q['executions']
    assert report['frame_budget_overshoot'] == max(0, report['frames'] - q['frames'])
    assert report['frame_budget_overshoot'] <= q['workers'] * 6 * 120
    prefix = read(protocol, 'first-encounter-input.json')['actions']
    local = read(output, 'witness-local.json')['actions']
    assert read(output, 'witness-full.json')['actions'] == prefix + local
    assert all(1 <= a['hold_frames'] <= 120 for a in prefix + local)
    witness = report['witness']
    assert witness['local_input_sha256'] == sha(raw(output, 'witness-local.json'))
    assert witness['full_input_sha256'] == sha(raw(output, 'witness-full.json'))
    cost = usage['cost']
    assert cost == report['cost']
    assert cost['direct_setup_frames'] == 6 * 929
    assert cost['direct_physical_frames'] == 6 * 929 + 4 * sum(a['hold_frames'] for a in prefix + local)
    assert cost['admitted_search_frames'] == cost['campaign_replay_admitted_frames'] == report['frames']
    assert cost['direct_physical_frames'] <= q['direct_frame_limit']
    known = sum(cost[k] for k in ['direct_physical_frames', 'admitted_search_frames', 'campaign_replay_admitted_frames'])
    assert known <= registration['block_known_auxiliary_ceiling']
    first = report['first_milestone']
    success, complete_horizon, decision = classify(first, report['executions'], report['frames'], q['frames'])
    assert report['milestone_reached_within_budget'] == success
    if success:
        assert not witness['dead'] and witness['context']['memory']['ridley_status'] & 2
    return {'format': 'metroid-archive-capability-ap01-analysis-v1',
            'registration_sha256': sha(raw(protocol, registration_name)),
            'decision': decision,
            'fixed_horizon_completed': complete_horizon, 'milestone_reached': success,
            'observed_first_milestone': first, 'stop_reason': report['stop_reason'],
            'executions': report['executions'], 'admitted_frames': report['frames'],
            'admitted_drain_frames': report['frame_budget_overshoot'],
            'known_auxiliary_frames': known, 'cost': cost,
            'witness_endpoint': witness['endpoint'],
            'artifacts_sha256': {name: sha(raw(output, name)) for name in
                                 ['result.json', 'stream.jsonl', 'checkpoint.bin', 'origin.bin',
                                  'campaign.json', 'witness-local.json', 'witness-full.json']},
            'scope': 'One supplied original encounter and reused diagnostic seed; no matched-control or fresh-discovery inference.',
            'unknown': report['unknown']}


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--protocol', type=Path, default=Path(__file__).parent)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    a = p.parse_args()
    a.out.write_text(json.dumps(score(a.protocol, a.output), indent=2) + '\n')
