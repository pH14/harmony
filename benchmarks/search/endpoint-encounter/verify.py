#!/usr/bin/env python3
"""Check registered short fixtures and extract narrow semantic projections; no emulation."""
import argparse
from copy import deepcopy
import hashlib
import json
from pathlib import Path

OLD_DIGEST = 'metroid-semantic-postcard-1.1.3-sha256-hex-motion-v5'
NEW_DIGEST = 'metroid-semantic-postcard-1.1.3-sha256-hex-motion-endpoint-context-v6'
POLICY = 'metroid-live-endpoint-boss-slots-v1'


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def canonical(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def policies(value, core, audit):
    value = deepcopy(value)
    backend = value['emulator_backend']
    assert backend.endswith(';sha256=' + core)
    expected = NEW_DIGEST if audit else OLD_DIGEST
    assert ';result_digest=' + expected + ';' in backend
    value['emulator_backend'] = backend.replace(expected, OLD_DIGEST)[:-64] + '0' * 64
    if audit:
        assert value.pop('endpoint_encounter_observation') == POLICY
    else:
        assert 'endpoint_encounter_observation' not in value
    return value


def project_stream(lines, core, audit):
    header, *events = deepcopy(lines)
    expected = 'metroid-quicknes-campaign-stream-' + ('endpoint-context-v5' if audit else 'v4')
    assert header['format'] == expected
    header = policies(header, core, audit)
    header['format'] = 'metroid-quicknes-campaign-stream-v4'
    for event in events:
        assert event['event'] == 'job' and len(event.pop('result_sha256')) == 64
    return canonical([header, *events]), canonical(events)


def project_checkpoint(value, core, audit):
    value = deepcopy(value)
    assert value['format'] == 'metroid-quicknes-snapshot-checkpoint-' + ('endpoint-context-v5' if audit else 'v4')
    value['format'] = 'metroid-quicknes-snapshot-checkpoint-v4'
    for entry in value['entries']:
        snapshot = entry['snapshot']
        assert not snapshot['failed']
        state = snapshot['emulator_state']
        assert bytes(state[:8]) == b'HQNESST2'
        assert bytes(state[48:112]).decode() == core
        state[48:112] = list(b'0' * 64)
        observation = snapshot['observation']
        if audit:
            assert observation.pop('endpoint_boss_slots') == (None if entry['id'] == 0 else 0)
        else:
            assert 'endpoint_boss_slots' not in observation
    return canonical(value)


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
    rows = []
    for record, cell in zip(panel['records'], reg['cells']):
        assert record['checks_passed'] and record['exit_code'] == 0
        s = record['summary']
        assert s['identity'] == {**s['identity'], **cell['expected_identity']}
        assert s['build']['binary_sha256'] == reg['binary_sha256']
        assert s['status'] == 'complete'
        p = args.root / reg['output'] / record['id'] / s['cell'] / 'campaign'
        result = json.loads((p / 'result.json').read_text())
        assert result == s['result']
        assert result['verification'] == 'campaign' and result['stop_reason'] == 'execution_limit'
        assert result['frames_emulated'] == 284283 and result['executions'] == 2000
        assert 'endpoint_encounter_witness' not in result
        assert not (p / 'first-endpoint-encounter.json').exists()
        hashes = {name: sha(p / name) for name in ('stream.jsonl', 'campaign.json', 'checkpoint.json')}
        for name, expected in cell.get('expected_artifact_sha256', {}).items():
            assert hashes[name] == expected, name
        audit = reg['audited']
        counts = s['last_progress']['workload_diagnostics'].get('endpoint_encounters')
        if audit:
            assert counts['format'] == 'metroid-live-endpoint-encounters-v1'
            counts = counts['counts']
            assert counts['actions_observed'] >= counts['live_endpoints'] > 0
            assert counts['classified_endpoints'] == 0 and counts['first'] is None
        else:
            assert counts is None
        core = s['identity']['core_sha256']
        lines = [json.loads(x) for x in (p / 'stream.jsonl').read_text().splitlines()]
        stream_projection, event_projection = project_stream(lines, core, audit)
        checkpoint = json.loads((p / 'checkpoint.json').read_text())
        checkpoint_projection = project_checkpoint(checkpoint, core, audit)
        campaign = policies(json.loads((p / 'campaign.json').read_text()), core, audit)
        assert campaign.pop('stream_sha256') == hashes['stream.jsonl']
        memory = {k: campaign.pop(k) for k in ('resident_memory_bytes', 'resident_snapshot_bytes')}
        witnesses = {'main': result['witness'], **{k: v['replay'] for k, v in result['milestone_witnesses'].items()}}
        witness_frames = 2 * sum(w['physical_suffix_frames'] for w in witnesses.values())
        for witness in witnesses.values():
            witness.pop('snapshot_sha256')
            if audit:
                observed = witness['diagnostics'].pop('endpoint_encounters')['counts']
                assert observed['classified_endpoints'] == 0 and observed['first'] is None
        rows.append({'id': record['id'], 'artifact_sha256': hashes,
                     'normalized_stream_sha256': stream_projection, 'normalized_events_sha256': event_projection,
                     'normalized_checkpoint_sha256': checkpoint_projection,
                     'normalized_campaign_sha256': canonical(campaign),
                     'normalized_witnesses_sha256': canonical(witnesses),
                     'snapshot_count': len(checkpoint['entries']), 'memory': memory,
                     'endpoint_counts': counts, 'summary_sha256': record['summary_sha256'],
                     'admitted_qualification_frames': result['frames_emulated'],
                     'inferred_full_replay_frames': result['frames_emulated'],
                     'twice_replayed_witness_frames': witness_frames,
                     'cpu_seconds': s['cpu_seconds'], 'elapsed_seconds': s['elapsed_seconds']})
    charge = sum(r[k] for r in rows for k in ('admitted_qualification_frames', 'inferred_full_replay_frames', 'twice_replayed_witness_frames'))
    assert charge <= reg['known_auxiliary_limit']
    for first, second in reg.get('buffer_pairs', []):
        a, b = (next(r for r in rows if r['id'] == name) for name in (first, second))
        assert a['artifact_sha256'] == b['artifact_sha256']
        assert a['endpoint_counts'] == b['endpoint_counts']
    report = {'format': 'metroid-endpoint-native-qualification-v1', 'decision': 'pass',
              'registration_sha256': sha(args.registration), 'panel_sha256': sha(panel_path),
              'source_commit': reg['source_commit'], 'binary_sha256': reg['binary_sha256'],
              'cells': rows, 'known_auxiliary_frames': charge,
              'limits': ['Reused short fixture, no positive native episode or fresh-search sample.',
                         'Full replay cost inferred from admitted work; setup, reconstruction and unadmitted work are additional unknowns.',
                         'Semantic job hashes are excluded only from cross-feature projection; full replay checks them within each build.']}
    assert not args.out.exists()
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'decision': 'pass', 'known_auxiliary_frames': charge}))


if __name__ == '__main__':
    main()
