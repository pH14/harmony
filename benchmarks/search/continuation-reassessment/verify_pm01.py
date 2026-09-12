#!/usr/bin/env python3
"""Verify PM01's exact native artifacts, closed physical ledger and source gate offline."""
from datetime import datetime
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
from run_pm01 import assess

ROOT = Path(__file__).resolve().parent
digest = lambda data: hashlib.sha256(data).hexdigest()


def verify():
    reg_bytes = (ROOT / 'pm01-registration.json').read_bytes()
    reg = json.loads(reg_bytes)
    q = json.loads((ROOT / 'pm01-request.json').read_text())
    old_q = json.loads((ROOT / 'pg01-progress-request.json').read_text())
    assert q['measure_physical_work'] is True
    assert {k: v for k, v in q.items() if k not in ['input', 'measure_physical_work']} == {k: v for k, v in old_q.items() if k != 'input'}
    for name, expected in reg['protocol_sha256'].items():
        assert digest((ROOT / name).read_bytes()) == expected, name
    build_bytes = (ROOT / 'pm01-build-info.json').read_bytes()
    assert digest(build_bytes) == reg['build_info_sha256']
    build = json.loads(build_bytes)
    assert build['commit'] == reg['source_commit'] and build['binary_sha256'] == reg['binary_sha256']
    assert build['exit_code'] == 0 and build['emulator_executed'] is False
    manifest = json.loads((ROOT / 'pm01-evidence-manifest.json').read_text())
    assert manifest['registration_sha256'] == digest(reg_bytes)
    native_bytes = (ROOT / 'pm01-native-manifest.json').read_bytes()
    assert digest(native_bytes) == manifest['remote_manifest_sha256']
    native = {i['path']: i for i in json.loads(native_bytes)}
    with tempfile.TemporaryDirectory(prefix='harmony-pm01-verify-') as temp:
        output = Path(temp)
        paths = set()
        for item in manifest['files']:
            path = Path(item['path'])
            assert not path.is_absolute() and '..' not in path.parts
            assert item['path'] not in paths
            paths.add(item['path'])
            compressed = (ROOT / item['gzip']).read_bytes()
            assert len(compressed) == item['gzip_bytes'] and digest(compressed) == item['gzip_sha256']
            raw = gzip.decompress(compressed)
            assert len(raw) == item['raw_bytes'] and digest(raw) == item['raw_sha256']
            assert native[item['path']] == {k: item[k] for k in ['path', 'raw_bytes', 'raw_sha256']}
            dest = output / path
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(raw)
        assert set(native) == paths
        value = lambda name: json.loads((output / (name + '.json')).read_text())
        start, process = value('start'), value('process')
        checked = assess(reg, q, output / 'cell')
        assert checked['qualified'] is True
        for key, expected in checked.items():
            assert process[key] == expected
        assert process['exit_code'] == 0 and process['killed'] is None
        assert process['usage_sha256'] == digest((output / 'cell/usage.json').read_bytes())
        assert start['registration_sha256'] == process['registration_sha256'] == digest(reg_bytes)
        assert process['hostname'] == start['hostname'] == reg['host'] == 'ms02'
        assert process['cgroup_limits'] == start['cgroup_limits'] == reg['cgroup_limits']
        assert process['cpus'] == reg['cpus']
        assert 0 <= process['wall_seconds'] <= reg['process_runtime_seconds']
        stamp = datetime.fromisoformat
        assert stamp(reg['registered_utc']) <= stamp(start['started_utc']) <= stamp(process['ended_utc']) <= stamp(reg['deadline_utc'])
        service, stopped = value('service-terminal'), value('service-stopped')
        assert service['InvocationID'] == process['invocation_id'] == start['invocation_id']
        assert service['MainPID'] == '0' and service['Result'] == 'success'
        assert service['MemoryMax'] == reg['cgroup_limits']['memory.max']
        assert service['MemorySwapMax'] == reg['cgroup_limits']['memory.swap.max']
        assert service['TasksMax'] == reg['cgroup_limits']['pids.max']
        assert service['AllowedCPUs'] == service['CPUAffinity'] == '0-3'
        assert service['RuntimeMaxUSec'] == '3min'
        assert int(service['MemoryPeak']) <= int(service['MemoryMax']) and service['MemorySwapPeak'] == '0'
        assert stopped['MainPID'] == '0' and stopped['ActiveState'] == 'inactive' and stopped['SubState'] == 'dead'
    # Bind every comparison to the original committed PG01 file, not just to
    # a duplicated list of expected hashes in a new registration.
    for name, expected in reg['expected_artifacts'].items():
        assert digest(gzip.decompress((ROOT / 'pg01-output/progress' / (name + '.gz')).read_bytes())) == expected
    ledger = json.loads((ROOT / 'ledger-after-pm01.json').read_text())
    assert ledger['before'] == reg['ledger_before']
    assert ledger['pm01'] == dict(admitted_search=checked['admitted_search_frames'], known_auxiliary=checked['known_auxiliary_frames'])
    assert ledger['after'] == {k: ledger['before'][k] + ledger['pm01'][k] for k in ledger['before']}
    assert ledger['pm01_total_measured_physical'] == checked['physical_frames']['total'] == reg['expected_physical_frames']
    assert ledger['pm01_total_measured_physical'] <= reg['physical_frame_allocation']
    return dict(format='retention-pm01-offline-verification-v1', verified=True, files=len(paths),
                assessment=checked, process={k: process[k] for k in ['started_utc', 'ended_utc', 'wall_seconds', 'cpu_seconds', 'peak_rss_kib']},
                service=service, ledger=ledger['after'],
                scope='Complete native accounting qualification and unchanged behavior verified from receipts; no emulator rerun or efficacy claim.')


if __name__ == '__main__':
    print(json.dumps(verify(), indent=2))
