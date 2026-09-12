#!/usr/bin/env python3
"""Verify the complete SR01 receipts offline. Never invokes an emulator."""
from datetime import datetime
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
from score_sr01 import assess, first_return

ROOT = Path(__file__).resolve().parent

def digest(raw): return hashlib.sha256(raw).hexdigest()


def verify():
    reg_bytes = (ROOT / 'sr01-registration.json').read_bytes()
    reg = json.loads(reg_bytes)
    for name, expected in reg['protocol_sha256'].items():
        assert digest((ROOT / name).read_bytes()) == expected, name
    build_bytes = (ROOT / 'sr01-build-info.json').read_bytes()
    assert digest(build_bytes) == reg['build_info_sha256']
    build = json.loads(build_bytes)
    assert build['commit'] == reg['source_commit'] and build['binary_sha256'] == reg['binary_sha256']
    assert build['exit_code'] == 0 and build['emulator_executed'] is False
    evidence = json.loads((ROOT / 'sr01-evidence-manifest.json').read_text())
    assert evidence['registration_sha256'] == digest(reg_bytes)
    native_bytes = (ROOT / 'sr01-native-manifest.json').read_bytes()
    assert digest(native_bytes) == evidence['remote_manifest_sha256']
    native = {item['path']: item for item in json.loads(native_bytes)}
    with tempfile.TemporaryDirectory(prefix='harmony-sr01-verify-') as temp:
        out = Path(temp)
        paths = set()
        for item in evidence['files']:
            path = Path(item['path'])
            assert not path.is_absolute() and '..' not in path.parts and item['path'] not in paths
            paths.add(item['path'])
            compressed = (ROOT / item['gzip']).read_bytes()
            assert len(compressed) == item['gzip_bytes'] and digest(compressed) == item['gzip_sha256']
            raw = gzip.decompress(compressed)
            assert len(raw) == item['raw_bytes'] and digest(raw) == item['raw_sha256']
            assert native[item['path']] == {k: item[k] for k in ['path', 'raw_bytes', 'raw_sha256']}
            dest = out / path
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(raw)
        assert set(native) == paths
        read = lambda name: json.loads((out / (name + '.json')).read_text())
        start, summary, terminal = read('start'), read('summary'), read('service-terminal')
        assert start['registration_sha256'] == digest(reg_bytes)
        assert start['host'] == reg['host'] == 'ms02' and start['cpus'] == reg['cpus']
        stamp = datetime.fromisoformat
        assert stamp(reg['registered_utc']) <= stamp(start['started_utc']) <= stamp(summary['ended_utc']) <= stamp(reg['deadline_utc'])
        assert terminal['ActiveState'] == 'active' and terminal['SubState'] == 'exited'
        assert terminal['MainPID'] == '0' and terminal['Result'] == 'success' and terminal['ExecMainStatus'] == '0'
        assert terminal['InvocationID'] == start['invocation_id']
        assert int(terminal['MemoryPeak']) <= int(reg['cgroup_limits']['memory.max'])
        assert terminal['MemorySwapPeak'] == terminal['MemorySwapMax'] == '0'
        assert terminal['MemoryMax'] == reg['cgroup_limits']['memory.max']
        assert terminal['TasksMax'] == reg['cgroup_limits']['pids.max']
        assert terminal['AllowedCPUs'] == terminal['CPUAffinity'] == '4-7'
        assert terminal['RuntimeMaxUSec'] == '7min'
        stopped = read('service-stopped')
        assert stopped['ActiveState'] == 'inactive' and stopped['SubState'] == 'dead' and stopped['MainPID'] == '0'
        resources = summary['cgroup_before_exit']
        assert int(resources['memory.peak']) <= int(terminal['MemoryPeak'])
        assert int(resources['memory.swap.peak']) == 0
        events = dict(line.split() for line in resources['memory.events'].splitlines())
        assert events['oom'] == events['oom_kill'] == '0' 
        rows = []
        # The frozen scorer is reused with only its local protocol directory
        # resolved; request bytes and expected native identities remain intact.
        offline = dict(reg, protocol=str(ROOT))
        assert len(summary['cells']) == len(reg['cells'])
        for cell, recorded in zip(reg['cells'], summary['cells']):
            assert recorded == read(cell['id'] + '/process')
            actual = assess(offline, cell, out / cell['id'] / 'campaign')
            assert all(recorded[k] == v for k, v in actual.items())
            assert recorded['valid'] is True and recorded['exit_code'] == 0 and recorded['killed'] is None
            assert 0 < recorded['wall_seconds'] <= reg['process_runtime_seconds']
            assert stamp(start['started_utc']) <= stamp(recorded['started_utc']) <= stamp(recorded['ended_utc']) <= stamp(summary['ended_utc'])
            rows.append(actual)
        activation = first_return(out / 'progress-control/campaign/stream.jsonl', out / 'progress-half/campaign/stream.jsonl')
        assert activation == summary['activation']
        qualified = activation['activated'] and all(r['alternative_admissions'] > 0 for r in rows if r['cell'] != 'ordinary-control')
        assert qualified == summary['qualified']
        physical = sum(r['physical_frames']['total'] for r in rows)
        admitted = sum(r['admitted_frames'] for r in rows)
        assert physical <= reg['physical_frame_ceiling']
        assert admitted <= reg['admitted_frame_ceiling_before_drain'] + len(rows) * reg['accepted_drain_frames']
        result = dict(format='scoped-return-sr01-offline-verification-v1', verified=True, qualified=qualified,
                      files=len(paths), cells=rows, activation=activation, physical_frames=physical,
                      admitted_frames=admitted, auxiliary_frames=physical - admitted,
                      wall_seconds=summary['wall_seconds'], service_cpu_seconds=int(terminal['CPUUsageNSec']) / 1e9,
                      service_peak_bytes=int(terminal['MemoryPeak']), scope='Implementation qualification only; no efficacy claim.')
        return result


if __name__ == '__main__': print(json.dumps(verify(), indent=2))
