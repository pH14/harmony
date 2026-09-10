#!/usr/bin/env python3
"""Check supplied-root qualification evidence without emulation."""
import argparse
import hashlib
import json
from pathlib import Path

from score_s01 import raw


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read(root, name):
    return json.loads(raw(root, name))


def prepare(protocol, evidence):
    q = read(protocol, 'aq01-prepare.json')
    result = read(evidence / 'prepare', 'result.json')
    snapshot = read(evidence / 'prepare', 'root-snapshot.json')
    usage = read(evidence / 'prepare', 'usage.json')
    assert usage['completed_execution'] and usage['error'] is None
    assert usage['request_sha256'] == digest(raw(protocol, 'aq01-prepare.json'))
    assert result['verified_prefix_replays'] == 2
    assert result['endpoint'] == q['expected_endpoint'] == snapshot['observation']['decoded']
    assert result['context'] == q['expected_context']
    assert not snapshot['failed'] and not snapshot['observation']['dead']
    assert snapshot['observation']['frame_count'] == 117875
    assert digest(bytes(snapshot['emulator_state'])) == result['emulator_sha256'] == q['expected_emulator_sha256']
    assert result['input_sha256'] == q['input_sha256']
    assert result['cost'] == usage['cost']
    assert result['cost'] == {'direct_physical_frames': 237608, 'direct_setup_frames': 1858,
                              'admitted_search_frames': 0, 'campaign_replay_admitted_frames': 0}
    # The pinned native implementation computes the postcard snapshot digest;
    # Python independently checks the exported emulator bytes and observation.
    assert len(result['snapshot_sha256']) == 64
    return {'decision': 'pass', 'snapshot_sha256': result['snapshot_sha256'],
            'root_json_sha256': digest(raw(evidence / 'prepare', 'root-snapshot.json')),
            'known_auxiliary_frames': 237608}


def compare(protocol, evidence):
    qualification = prepare(protocol, evidence)
    roots = [evidence / name for name in ['one-slot', 'two-slots']]
    reports = [read(root, 'result.json') for root in roots]
    costs = []
    prefix = read(protocol, 'first-encounter-input.json')['actions']
    for index, (root, report) in enumerate(zip(roots, reports), 1):
        request_name = f'aq01-slots-{index}.json'
        q = read(protocol, request_name)
        usage = read(root, 'usage.json')
        assert usage['completed_execution'] and usage['error'] is None
        assert usage['request_sha256'] == digest(raw(protocol, request_name))
        assert q['expected_snapshot_sha256'] == qualification['snapshot_sha256']
        assert report['root']['snapshot_sha256'] == qualification['snapshot_sha256']
        assert report['complete'] and report['stop_reason'] == 'execution_limit'
        assert report['executions'] == q['executions'] == 16
        assert not report['milestone_reached_within_budget'] and report['first_milestone'] is None
        assert report['origin']['kind'] == 'snapshot_root'
        assert report['full_campaign_replay']
        assert report['root_local_witness_replays'] == report['complete_prefix_witness_replays'] == 2
        assert digest(raw(root, 'stream.jsonl')) == report['stream_sha256']
        local = read(root, 'witness-local.json')['actions']
        full = read(root, 'witness-full.json')['actions']
        assert full == prefix + local
        assert digest(raw(root, 'witness-local.json')) == report['witness']['local_input_sha256']
        assert digest(raw(root, 'witness-full.json')) == report['witness']['full_input_sha256']
        local_frames = sum(a['hold_frames'] for a in local)
        cost = report['cost']
        assert cost == usage['cost']
        assert cost['direct_setup_frames'] == 6 * 929
        assert cost['direct_physical_frames'] == 6 * 929 + 4 * 117875 + 4 * local_frames
        assert cost['admitted_search_frames'] == cost['campaign_replay_admitted_frames'] == report['frames']
        assert cost['direct_physical_frames'] <= q['direct_frame_limit']
        costs.append(sum(cost[k] for k in ['direct_physical_frames', 'admitted_search_frames', 'campaign_replay_admitted_frames']))
    for name in ['stream.jsonl', 'checkpoint.bin', 'origin.bin', 'campaign.json',
                 'root-snapshot.json', 'witness-local.json', 'witness-full.json']:
        assert raw(roots[0], name) == raw(roots[1], name), name
    assert reports[0] == reports[1]
    total = qualification['known_auxiliary_frames'] + sum(costs)
    assert total <= read(protocol, 'aq01-prepare-registration.json')['block_known_auxiliary_ceiling']
    return {'decision': 'pass', 'root': qualification, 'cell_known_auxiliary_frames': costs,
            'known_auxiliary_frames': total, 'native_executions_per_cell': 16,
            'native_admitted_frames_per_cell': reports[0]['frames'],
            'buffer_artifacts_identical': True,
            'scope': 'Supplied-root integration qualification, not an archive capability or performance result.',
            'unknown': reports[0]['unknown']}


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--protocol', type=Path, default=Path(__file__).parent)
    p.add_argument('--evidence', type=Path, required=True)
    p.add_argument('--phase', choices=['prepare', 'compare'], required=True)
    p.add_argument('--out', type=Path, required=True)
    a = p.parse_args()
    result = (prepare if a.phase == 'prepare' else compare)(a.protocol, a.evidence)
    a.out.write_text(json.dumps(result, indent=2) + '\n')
