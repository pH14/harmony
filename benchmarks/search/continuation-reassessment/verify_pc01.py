#!/usr/bin/env python3
"""Verify all published PC01 receipts and rescore without emulator execution."""
import gzip
import hashlib
import json
from pathlib import Path
from datetime import datetime
from score_pc01 import score

HERE = Path(__file__).resolve().parent


def sha(bytes_):
    return hashlib.sha256(bytes_).hexdigest()


def verify():
    load = lambda name:json.loads((HERE/name).read_text())
    master = load('pc01-registration.json')
    for name, digest in master['protocol_sha256'].items():
        assert sha((HERE/name).read_bytes()) == digest, name
    build = load('pc01-build-info.json')
    assert sha((HERE/'pc01-build-info.json').read_bytes()) == master['build_info_sha256']
    assert build['exit_code'] == 0 and build['emulator_executed'] is False
    assert build['source_commit'] == master['source_commit']
    assert build['binary_sha256'] == master['binary_sha256']
    extract = load('pc01-pair/manifest.json')
    assert extract['emulator_constructed'] is False and extract['physical_frames'] == 0
    assert extract['selected_ordinal'] == 692 and extract['verified_complete_records'] == 3770
    assert extract['request_sha256'] == sha((HERE/'pc01-extract-request.json').read_bytes())
    for item in extract['files']:
        raw = (HERE/'pc01-pair'/item['file']).read_bytes()
        assert len(raw) == item['bytes'] and sha(raw) == item['sha256']
    native = {}
    manifest = load('pc01-evidence-manifest.json')
    for name, item in manifest['files'].items():
        path = (HERE/item['gzip']).resolve()
        assert HERE in path.parents
        compressed = path.read_bytes()
        assert len(compressed) == item['gzip_bytes'] and sha(compressed) == item['gzip_sha256']
        # Bound decoding as well as compressed-file reading.
        with gzip.open(path,'rb') as stream:
            raw = stream.read(item['raw_bytes']+1)
        assert len(raw) == item['raw_bytes'] and sha(raw) == item['raw_sha256']
        native[name] = raw
    process = json.loads(native['process.json'])
    complete = json.loads(native['probe/complete.json'])
    usage = json.loads(native['probe/usage.json'])
    assert process['complete'] == complete and process['usage'] == usage
    assert process['complete_sha256'] == sha(native['probe/complete.json'])
    assert process['usage_sha256'] == sha(native['probe/usage.json'])
    assert process['decision'] == 'complete' and process['exit_code'] == 0 and process['killed'] is None
    assert process['command'] == [master['binary'],'run',master['protocol']+'/pc01-request.json',master['output']+'/probe']
    assert process['hostname'] == 'ms02' and process['cpus'] == master['cpus']
    assert process['cgroup_limits'] == master['cgroup_limits']
    assert process['registration_sha256'] == sha((HERE/'pc01-registration.json').read_bytes())
    assert datetime.fromisoformat(master['registered_utc']) < datetime.fromisoformat(process['started_utc'])
    assert datetime.fromisoformat(process['ended_utc']) < datetime.fromisoformat(master['deadline_utc'])
    assert process['wall_seconds'] < 120 and process['peak_rss_kib'] * 1024 < 4*1024**3
    assert usage['complete'] and usage['error'] is None
    assert usage['request_sha256'] == sha((HERE/'pc01-request.json').read_bytes())
    rows = [json.loads(line) for line in native['probe/trials.jsonl'].splitlines()]
    bank = load('pc01-bank.json')
    assert len(bank['seeds']) == 32 and len(set(bank['seeds'])) == 32
    assert all(len(suffix) == 128 for suffix in bank['suffixes'])
    assert bank['seeds'] == [int.from_bytes(hashlib.sha256(f'harmony:pc01:record692:tail:{i}'.encode()).digest()[:8],'little') for i in range(32)]
    continuation = 0
    assert rows[0]['cost'] == dict(constructor_started=True,setup=929,continuation=0,held_verification=0)
    for row in rows[1:]:
        o = row['outcome']
        suffix = bank['suffixes'][row['trial']]
        frames = 0
        assert len(o['executed']) == len(o['points'])
        for i, (action,point) in enumerate(zip(o['executed'],o['points'])):
            original = suffix[i]
            assert action['buttons'] == original['buttons']
            assert 1 <= action['hold_frames'] <= original['hold_frames']
            assert point['complete'] == (action['hold_frames'] == original['hold_frames'])
            if not point['complete']:
                assert i == len(o['points'])-1 and o['stop_reason'] in ('death','area_exit')
            frames += action['hold_frames']
            assert point['frames'] == frames and point['action'] == i+1
        assert o['frames'] == frames <= 8192
        continuation += frames
        assert row['cost'] == dict(constructor_started=True,setup=929,continuation=continuation,held_verification=continuation)
    assert complete['cost'] == usage['cost'] == rows[-1]['cost'] == process['last_cost_receipt']
    physical = 929 + 2 * continuation
    assert physical == usage['physical_frames_known'] == complete['physical_frames'] <= master['physical_frame_ceiling']
    assert complete['episodes'] == 64 and complete['trials'] == 32
    analysis = score(rows, bank)
    assert analysis == load('pc01-analysis.json')
    props = lambda name:dict(line.split('=',1) for line in native[name].decode().splitlines())
    terminal = props('service-terminal.txt')
    assert terminal['MainPID'] == '0' and terminal['ActiveState'] == 'inactive' and terminal['SubState'] == 'dead'
    service = props('service-complete.txt')
    assert service['InvocationID'] == process['invocation_id'] and service['MainPID'] == '0'
    assert service['Result'] == 'success' and service['ExecMainStatus'] == '0'
    assert service['MemoryMax'] == '4294967296' and service['MemorySwapMax'] == '0' and service['TasksMax'] == '64'
    assert service['AllowedCPUs'] == service['CPUAffinity'] == '0-3'
    assert int(service['MemoryPeak']) < int(service['MemoryMax'])
    ledger = load('ledger-after-pc01.json')
    prior = load('ledger-after-rr02.json')
    assert ledger['prior_ledger_sha256'] == sha((HERE/'ledger-after-rr02.json').read_bytes())
    assert ledger['known_auxiliary_frames'] == physical and ledger['admitted_search_frames'] == 0
    assert ledger['cumulative_since_user_resumption_known_auxiliary_frames'] == prior['cumulative_since_user_resumption_known_auxiliary_frames']+physical
    assert ledger['cumulative_since_user_resumption_admitted_search_frames'] == prior['cumulative_since_user_resumption_admitted_search_frames']
    assert ledger['registration_sha256'] == process['registration_sha256']
    assert ledger['analysis_sha256'] == sha((HERE/'pc01-analysis.json').read_bytes())
    assert ledger['evidence_manifest_sha256'] == sha((HERE/'pc01-evidence-manifest.json').read_bytes())
    return dict(format='retention-pc01-closure-verification-v1',status='verified',
                evidence_files=len(native),episodes=len(rows)-1,held_boundaries=sum(r['outcome']['held_boundaries_verified'] for r in rows[1:]),
                physical_frames=physical,decision=analysis['decision'],emulator_executed=False)


if __name__ == '__main__':
    print(json.dumps(verify(),indent=2))
