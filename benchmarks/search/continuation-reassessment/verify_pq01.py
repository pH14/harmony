#!/usr/bin/env python3
"""Verify PQ01's exact paused reads, fixed analysis and closed ledger offline."""
from datetime import datetime
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
from analyze_pq01 import analyze
from read_pq01 import paused_gate

ROOT = Path(__file__).resolve().parent
digest = lambda raw: hashlib.sha256(raw).hexdigest()


def verify():
    reg_bytes = (ROOT / 'pq01-registration.json').read_bytes()
    reg = json.loads(reg_bytes)
    for name, expected in reg['protocol_sha256'].items():
        assert digest((ROOT / name).read_bytes()) == expected, name
    for name, expected in reg['analysis_dependency_sha256'].items():
        assert digest((ROOT.parents[2] / name).read_bytes()) == expected, name
    build_bytes = (ROOT / 'pq01-build-info.json').read_bytes()
    assert digest(build_bytes) == reg['build_info_sha256']
    build = json.loads(build_bytes)
    assert build['commit'] == reg['source_commit'] and build['binary_sha256'] == reg['binary_sha256']
    assert build['exit_code'] == 0 and build['emulator_executed'] is False
    manifest = json.loads((ROOT / 'pq01-evidence-manifest.json').read_text())
    assert manifest['registration_sha256'] == digest(reg_bytes)
    native_bytes = (ROOT / 'pq01-native-manifest.json').read_bytes()
    assert digest(native_bytes) == manifest['remote_manifest_sha256']
    native = {item['path']: item for item in json.loads(native_bytes)}
    with tempfile.TemporaryDirectory(prefix='harmony-pq01-verify-') as temp:
        out, seen = Path(temp), set()
        for item in manifest['files']:
            path = Path(item['path'])
            assert not path.is_absolute() and '..' not in path.parts and item['path'] not in seen
            seen.add(item['path'])
            compressed = (ROOT / item['gzip']).read_bytes()
            assert len(compressed) == item['gzip_bytes'] and digest(compressed) == item['gzip_sha256']
            raw = gzip.decompress(compressed)
            assert len(raw) == item['raw_bytes'] and digest(raw) == item['raw_sha256']
            assert native[item['path']] == {k: item[k] for k in ['path', 'raw_bytes', 'raw_sha256']}
            (out / path).write_bytes(raw)
        assert seen == set(native)
        value = lambda name: json.loads((out / (name + '.json')).read_text())
        start, complete = value('start'), value('complete')
        stamp = datetime.fromisoformat
        assert start['registration_sha256'] == complete['registration_sha256'] == digest(reg_bytes)
        assert start['hostname'] == complete['hostname'] == reg['host'] == 'ms02'
        assert start['cpus'] == complete['cpus'] == reg['cpus'] == [8]
        assert start['cgroup_limits'] == complete['cgroup_limits'] == reg['cgroup_limits']
        assert complete['qualified'] is True and complete['error'] is None and not complete['incomplete_physical_receipts']
        assert stamp(reg['registered_utc']) <= stamp(start['started_utc']) <= stamp(complete['ended_utc']) <= stamp(reg['deadline_utc'])
        restores = 0
        for cell, record in zip(reg['cells'], complete['cells']):
            q_bytes = (ROOT / cell['request']).read_bytes()
            q = json.loads(q_bytes)
            inv = json.loads(gzip.decompress((ROOT / cell['inventory']).read_bytes()))
            result = value(cell['id'])
            assert paused_gate(q, inv, result)
            assert result['request_sha256'] == digest(q_bytes)
            assert record == value(cell['id'] + '-process')
            assert record['cell'] == cell['id'] and record['qualified'] is True
            assert record['exit_code'] == 0 and record['killed'] is None
            assert 0 <= record['wall_seconds'] <= reg['process_runtime_seconds']
            assert stamp(start['started_utc']) <= stamp(record['started_utc']) <= stamp(record['ended_utc']) <= stamp(complete['ended_utc'])
            restores += result['verified_restores']
        assert len(complete['cells']) == len(reg['cells']) == 2
        assert sum(c['physical_frames'] for c in complete['cells']) == complete['measured_physical_frames'] == reg['expected_physical_frames'] == 1858
        assert complete['measured_physical_frames'] <= reg['physical_frame_allocation']
        assert analyze(out) == json.loads(gzip.decompress((ROOT / 'pq01-analysis.json.gz').read_bytes()))
        terminal, stopped = value('service-terminal'), value('service-stopped')
        assert terminal['MainPID'] == '0' and terminal['Result'] == 'success' and terminal['SubState'] == 'exited'
        assert terminal['InvocationID'] == start['invocation_id'] == complete['invocation_id']
        assert terminal['MemoryMax'] == reg['cgroup_limits']['memory.max']
        assert terminal['MemorySwapMax'] == terminal['MemorySwapPeak'] == '0'
        assert terminal['TasksMax'] == reg['cgroup_limits']['pids.max']
        assert terminal['AllowedCPUs'] == terminal['CPUAffinity'] == '8'
        assert terminal['RuntimeMaxUSec'] == '2min'
        assert int(complete['cgroup_memory_peak']) <= int(terminal['MemoryPeak']) <= int(terminal['MemoryMax'])
        assert stopped['MainPID'] == '0' and stopped['ActiveState'] == 'inactive' and stopped['SubState'] == 'dead'
    ledger = json.loads((ROOT / 'ledger-after-pq01.json').read_text())
    assert ledger['before'] == reg['ledger_before']
    assert ledger['pq01'] == dict(admitted_search=0, known_auxiliary=1858)
    assert ledger['after'] == {k: ledger['before'][k] + ledger['pq01'][k] for k in ledger['before']}
    assert ledger['pq01_total_measured_physical'] == complete['measured_physical_frames']
    return dict(format='retention-pq01-verification-v1', verified=True, files=len(seen),
                verified_restores=restores, physical_frames=1858, ledger=ledger['after'],
                service=terminal, started_utc=start['started_utc'], ended_utc=complete['ended_utc'],
                scope='Exact paused-state read and cached-state history verified; no native actions, fresh-search gain or repaired PG02 gate.')


if __name__ == '__main__':
    print(json.dumps(verify(), indent=2))
