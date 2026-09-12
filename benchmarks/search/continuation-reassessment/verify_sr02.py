#!/usr/bin/env python3
"""Verify SR02's native screen, interval decisions and costs without emulation."""
from datetime import datetime
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
from score_sr02 import assess, panel_score

ROOT = Path(__file__).resolve().parent

def digest(raw): return hashlib.sha256(raw).hexdigest()


def verify():
    reg_bytes = (ROOT / 'sr02-registration.json').read_bytes()
    reg = json.loads(reg_bytes)
    for name, expected in reg['protocol_sha256'].items():
        assert digest((ROOT / name).read_bytes()) == expected, name
    build_bytes = (ROOT / 'sr02-build-info.json').read_bytes()
    assert digest(build_bytes) == reg['build_info_sha256']
    build = json.loads(build_bytes)
    assert build['commit'] == reg['source_commit'] and build['binary_sha256'] == reg['binary_sha256']
    assert build['exit_code'] == 0 and build['emulator_executed'] is False
    evidence = json.loads((ROOT / 'sr02-evidence-manifest.json').read_text())
    assert evidence['registration_sha256'] == digest(reg_bytes)
    native_bytes = (ROOT / 'sr02-native-manifest.json').read_bytes()
    assert digest(native_bytes) == evidence['remote_manifest_sha256']
    native = {item['path']: item for item in json.loads(native_bytes)}
    with tempfile.TemporaryDirectory(prefix='harmony-sr02-verify-') as temp:
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
        start, summary, terminal = read('start'), read('complete'), read('service-terminal')
        stamp = datetime.fromisoformat
        assert start['registration_sha256'] == summary['registration_sha256'] == digest(reg_bytes)
        assert start['hostname'] == summary['hostname'] == reg['host'] == 'ms02'
        assert start['cpus'] == summary['cpus'] == reg['controller_cpus']
        assert start['cgroup_limits'] == summary['cgroup_limits'] == reg['controller_cgroup_limits']
        assert stamp(reg['registered_utc']) <= stamp(start['started_utc']) <= stamp(summary['ended_utc']) <= stamp(reg['deadline_utc'])
        assert terminal['ActiveState'] == 'active' and terminal['SubState'] == 'exited'
        assert terminal['MainPID'] == '0' and terminal['Result'] == 'success' and terminal['ExecMainStatus'] == '0'
        assert terminal['InvocationID'] == start['invocation_id']
        assert int(terminal['MemoryPeak']) <= int(reg['controller_cgroup_limits']['memory.max'])
        assert terminal['MemorySwapPeak'] == terminal['MemorySwapMax'] == '0'
        assert terminal['MemoryMax'] == reg['controller_cgroup_limits']['memory.max']
        assert terminal['TasksMax'] == reg['controller_cgroup_limits']['pids.max']
        assert terminal['AllowedCPUs'] == terminal['CPUAffinity'] == '8'
        assert terminal['RuntimeMaxUSec'] == '18min'
        resources = summary['cgroup_before_exit']
        assert int(resources['memory.peak']) <= int(terminal['MemoryPeak'])
        assert int(resources['memory.swap.peak']) == 0
        events = dict(line.split() for line in resources['memory.events'].splitlines())
        assert events['oom'] == events['oom_kill'] == '0' 
        stopped = read('service-stopped')
        assert stopped['ActiveState'] == 'inactive' and stopped['MainPID'] == '0'
        assert not summary['unresolved_cost_cells']
        assert summary['decision']['status'] in ['passed', 'failed', 'futile']
        cells = [c for c in reg['cells'] if (out / c['id']).exists()]
        assert [c['id'] for c in cells] == summary['launched_cells']
        assert set(summary['completed_cells']) == set(summary['launched_cells'])
        assert summary['unrun_cells'] == [c['id'] for c in reg['cells'] if c not in cells]
        assert len(cells) % 4 == 0 and len(cells) == len(summary['cells'])
        rows = []
        offline = dict(reg, protocol=str(ROOT))
        for cell, recorded in zip(cells, summary['cells']):
            assert recorded == read(cell['id'] + '/process')
            actual = assess(offline, cell, out / cell['id'] / 'campaign')
            assert all(recorded[k] == v for k, v in actual.items())
            assert recorded['valid'] is True and recorded['exit_code'] == 0 and recorded['killed'] is None
            assert recorded['cpu_microseconds'] == round(recorded['cpu_seconds'] * 1000000)
            assert 0 < recorded['wall_seconds'] <= reg['process_runtime_seconds']
            assert stamp(start['started_utc']) <= stamp(recorded['started_utc']) <= stamp(recorded['ended_utc']) <= stamp(summary['ended_utc'])
            assert recorded['registration_sha256'] == digest(reg_bytes)
            assert recorded['cpus'] == cell['cpus'] and recorded['cgroup_limits'] == reg['cell_cgroup_limits']
            state, after = read(cell['id'] + '/service-terminal'), read(cell['id'] + '/service-stopped')
            assert state['ActiveState'] == 'active' and state['SubState'] == 'exited' and state['MainPID'] == '0'
            assert state['Result'] == 'success' and state['ExecMainStatus'] == '0'
            assert state['InvocationID'] == recorded['invocation_id']
            assert state['AllowedCPUs'] == state['CPUAffinity'] == str(cell['cpus'][0])+'-'+str(cell['cpus'][-1])
            assert state['RuntimeMaxUSec'] == '2min' and state['TasksMax'] == '64'
            assert state['MemoryMax'] == reg['cell_cgroup_limits']['memory.max']
            assert int(state['MemoryPeak']) <= int(state['MemoryMax'])
            assert state['MemorySwapMax'] == state['MemorySwapPeak'] == '0'
            assert after['ActiveState'] == 'inactive' and after['MainPID'] == '0'
            actual.update(cpu_microseconds=recorded['cpu_microseconds'], peak_rss_kib=recorded['peak_rss_kib'],
                          wall_seconds=recorded['wall_seconds'], service_cpu_seconds=int(state['CPUUsageNSec']) / 1e9,
                          service_peak_bytes=int(state['MemoryPeak']))
            rows.append(actual)
        for batch in reg['batches'][:len(cells)//2]:
            pair = [read(c['id'] + '/process') for c in cells if c['id'] in batch]
            assert len(pair) == 2 and len({cpu for r in pair for cpu in r['cpus']}) == 8
        timeline = []
        previous_end = stamp(start['started_utc'])
        for batch in reg['batches'][:len(cells)//2]:
            pair = [read(c['id'] + '/process') for c in cells if c['id'] in batch]
            starts = [stamp(r['started_utc']) for r in pair]
            ends = [stamp(r['ended_utc']) for r in pair]
            assert min(starts) >= previous_end and max(starts) < min(ends)
            previous_end = max(ends)
            timeline.extend((t, 1) for t in starts)
            timeline.extend((t, -1) for t in ends)
        running = 0
        for _, delta in sorted(timeline):
            running += delta
            assert 0 <= running <= 2
        assert running == 0
        decisions = []
        for pair in range(len(cells)//4):
            decision = panel_score([r for r in rows if r['pair'] <= pair])
            assert decision == read('decision-' + str(pair))
            if pair < len(cells)//4 - 1: assert decision['status'] == 'continue'
            decisions.append(decision)
        assert decisions[-1] == summary['decision']
        physical = sum(r['physical_frames']['total'] for r in rows)
        admitted = sum(r['admitted_frames'] for r in rows)
        assert physical == summary['measured_physical_frames'] <= reg['physical_frame_allocation']
        assert admitted == summary['admitted_frames'] <= reg['admitted_frame_ceiling_before_drain'] + len(rows)*reg['accepted_drain_frames']
        assert physical - admitted == summary['auxiliary_frames']
        return dict(format='scoped-return-sr02-offline-verification-v1', verified=True, files=len(paths),
                    decision=summary['decision'], decisions=decisions, cells=rows,
                    unrun_cells=summary['unrun_cells'], physical_frames=physical,
                    admitted_frames=admitted, auxiliary_frames=physical-admitted,
                    wall_seconds=(stamp(summary['ended_utc'])-stamp(start['started_utc'])).total_seconds(),
                    controller_cpu_seconds=int(terminal['CPUUsageNSec'])/1e9,
                    controller_peak_bytes=int(terminal['MemoryPeak']),
                    scope='Conditional efficacy screen only. No fresh-search, confirmation or validation claim.')


if __name__ == '__main__': print(json.dumps(verify(), indent=2))
