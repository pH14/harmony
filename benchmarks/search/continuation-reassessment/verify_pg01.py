#!/usr/bin/env python3
"""Verify frozen PG01 receipts, replay claims, activation gate and closed ledger offline."""
from datetime import datetime
import gzip
import hashlib
import json
from pathlib import Path
from run_pg01 import qualified

ROOT = Path(__file__).resolve().parent


def digest(data):
    return hashlib.sha256(data).hexdigest()


def verify():
    reg_bytes = (ROOT / 'pg01-registration.json').read_bytes()
    reg = json.loads(reg_bytes)
    manifest = json.loads((ROOT / 'pg01-evidence-manifest.json').read_text())
    assert manifest['registration_sha256'] == digest(reg_bytes)
    native_bytes = (ROOT / 'pg01-native-manifest.json').read_bytes()
    assert digest(native_bytes) == manifest['remote_manifest_sha256']
    native = {i['path']: i for i in json.loads(native_bytes)}
    data = {}
    for item in manifest['files']:
        compressed = (ROOT / item['gzip']).read_bytes()
        assert len(compressed) == item['gzip_bytes'] and digest(compressed) == item['gzip_sha256']
        raw = gzip.decompress(compressed)
        assert len(raw) == item['raw_bytes'] and digest(raw) == item['raw_sha256']
        assert item['path'] not in data
        assert native[item['path']] == {k: item[k] for k in ['path', 'raw_bytes', 'raw_sha256']}
        data[item['path']] = raw
    assert set(native) == set(data)
    for name, expected in reg['protocol_sha256'].items():
        assert digest((ROOT / name).read_bytes()) == expected, name
    build_bytes = (ROOT / 'pg01-build-info.json').read_bytes()
    assert digest(build_bytes) == reg['build_info_sha256']
    build = json.loads(build_bytes)
    assert build['commit'] == reg['source_commit'] and build['binary_sha256'] == reg['binary_sha256']
    assert build['exit_code'] == 0 and build['emulator_executed'] is False
    assert build['command'][build['command'].index('--features') + 1] == 'metroid-retention-progress'
    value = lambda path: json.loads(data[path])
    start, complete = value('start.json'), value('complete.json')
    service, stopped = value('service-terminal.json'), value('service-stopped.json')
    assert start['hostname'] == reg['host'] == 'ms02'
    assert complete['registration_sha256'] == start['registration_sha256'] == digest(reg_bytes)
    assert start['invocation_id'] == complete['invocation_id'] == service['InvocationID']
    assert start['cpus'] == reg['cpus'] and start['cgroup_limits'] == reg['cgroup_limits']
    assert service['Result'] == 'success' and service['MainPID'] == '0'
    assert service['MemoryMax'] == reg['cgroup_limits']['memory.max']
    assert service['MemorySwapMax'] == reg['cgroup_limits']['memory.swap.max']
    assert service['TasksMax'] == reg['cgroup_limits']['pids.max']
    assert service['AllowedCPUs'] == service['CPUAffinity'] == '0-3'
    assert service['RuntimeMaxUSec'] == '7min 30s'
    assert stopped['MainPID'] == '0' and stopped['ActiveState'] == 'inactive' and stopped['SubState'] == 'dead'
    assert int(service['MemoryPeak']) <= int(reg['cgroup_limits']['memory.max'])
    assert service['MemorySwapPeak'] == '0'
    stamp = datetime.fromisoformat
    assert stamp(reg['registered_utc']) <= stamp(start['started_utc']) <= stamp(complete['ended_utc']) <= stamp(reg['deadline_utc'])
    assert (stamp(complete['ended_utc']) - stamp(start['started_utc'])).total_seconds() <= reg['service_runtime_seconds']
    assert len(complete['cells']) == complete['cells_completed']
    rows, admitted, auxiliary = [], 0, 0
    ordinary = json.loads((ROOT / reg['cells'][0]['request']).read_text())
    for cell, recorded in zip(reg['cells'], complete['cells']):
        name = cell['id']
        process = value(name + '-process.json')
        request = json.loads((ROOT / cell['request']).read_text())
        assert {k: v for k, v in request.items() if k != 'slot_retention'} == {k: v for k, v in ordinary.items() if k != 'slot_retention'}
        assert recorded['cell'] == process['cell'] == name
        assert process['exit_code'] == 0 and process['killed'] is None
        assert 0 <= process['wall_seconds'] <= cell['process_runtime_seconds']
        assert stamp(reg['registered_utc']) <= stamp(process['started_utc']) <= stamp(process['ended_utc']) <= stamp(reg['deadline_utc'])
        for artifact in ['result', 'campaign', 'usage']:
            assert process[artifact] == value(name + '/' + artifact + '.json')
            assert process[artifact + '_sha256'] == digest(data[name + '/' + artifact + '.json'])
        progress_bytes = data[name + '/progress.jsonl']
        assert progress_bytes.endswith(b'\n') and digest(progress_bytes) == process['progress_sha256']
        progress = json.loads(progress_bytes.splitlines()[-1])
        assert process['progress'] == progress
        result, campaign, usage = process['result'], process['campaign'], process['usage']
        passed = qualified(cell, request, result, campaign, usage, progress)
        assert passed == process['qualified'] == recorded['qualified']
        assert digest(data[name + '/stream.jsonl']) == result['stream_sha256']
        header = json.loads(data[name + '/stream.jsonl'].splitlines()[0])
        assert header['format'] == 'metroid-quicknes-campaign-stream-scoped-progress-v6'
        assert header.get('slot_retention') == request['slot_retention']
        for kind in ['local', 'full']:
            assert digest(data[name + '/witness-' + kind + '.json']) == result['witness'][kind + '_input_sha256']
        assert not result['witness']['dead']
        cost = usage['cost']
        assert cost['direct_setup_frames'] <= cost['direct_physical_frames']
        admitted += cost['admitted_search_frames']
        auxiliary += cost['direct_physical_frames'] + cost['campaign_replay_admitted_frames']
        rows.append(dict(cell=name, qualified=passed, frames=result['frames'], executions=result['executions'],
                         alternative_admissions=progress['retention_diagnostics']['alternative_admissions'],
                         living_defeat=result['milestone_reached_within_budget'],
                         wall_seconds=process['wall_seconds'], cpu_seconds=process['cpu_seconds'],
                         peak_rss_kib=process['peak_rss_kib'], cost=cost))
    overall = len(rows) == len(reg['cells']) and all(r['qualified'] for r in rows)
    assert complete['qualified'] == overall
    if not overall:
        assert rows and not rows[-1]['qualified']
        assert all(r['qualified'] for r in rows[:-1])
        for cell in reg['cells'][len(rows):]:
            assert not any(p.startswith(cell['id'] + '/') for p in data)
    assert complete['known_admitted_frames'] == admitted and complete['known_auxiliary_frames'] == auxiliary
    ledger = json.loads((ROOT / 'ledger-after-pg01.json').read_text())
    assert ledger['before'] == reg['ledger_before']
    assert ledger['pg01'] == dict(admitted_search=admitted, known_auxiliary=auxiliary)
    assert ledger['after'] == {k: ledger['before'][k] + ledger['pg01'][k] for k in ledger['before']}
    return dict(format='retention-pg01-offline-verification-v1', verified=True, qualified=overall,
                files=len(data), cells=rows, ledger=ledger['after'],
                scope='Implementation qualification only; complete native replay receipts are checked without rerunning the emulator. No fresh-search efficacy claim.')


if __name__ == '__main__':
    print(json.dumps(verify(), indent=2))
