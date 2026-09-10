#!/usr/bin/env python3
"""Verify PG02's invalid closure and both native receipts without emulation."""
from datetime import datetime
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
from audit_pg02 import compatible_cell_gate
from score_pg02 import cell_gate, restricted_cost

ROOT = Path(__file__).resolve().parent
digest = lambda data: hashlib.sha256(data).hexdigest()


def verify():
    reg_bytes = (ROOT / 'pg02-registration.json').read_bytes()
    reg = json.loads(reg_bytes)
    for name, expected in reg['protocol_sha256'].items():
        assert digest((ROOT / name).read_bytes()) == expected, name
    build_bytes = (ROOT / 'pg02-build-info.json').read_bytes()
    assert digest(build_bytes) == reg['build_info_sha256']
    build = json.loads(build_bytes)
    assert build['commit'] == reg['source_commit'] and build['binary_sha256'] == reg['binary_sha256']
    assert build['exit_code'] == 0 and build['emulator_executed'] is False
    manifest = json.loads((ROOT / 'pg02-evidence-manifest.json').read_text())
    assert manifest['registration_sha256'] == digest(reg_bytes)
    native_bytes = (ROOT / 'pg02-native-manifest.json').read_bytes()
    assert digest(native_bytes) == manifest['remote_manifest_sha256']
    native = {item['path']: item for item in json.loads(native_bytes)}
    rows = []
    with tempfile.TemporaryDirectory(prefix='harmony-pg02-verify-') as temp:
        out = Path(temp)
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
            dest = out / path
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(raw)
        assert set(native) == paths
        value = lambda name: json.loads((out / (name + '.json')).read_text())
        start, summary = value('start'), value('complete')
        stamp = datetime.fromisoformat
        assert start['registration_sha256'] == summary['registration_sha256'] == digest(reg_bytes)
        assert summary['hostname'] == start['hostname'] == 'ms02'
        assert stamp(reg['registered_utc']) <= stamp(start['started_utc']) <= stamp(summary['ended_utc']) <= stamp(reg['deadline_utc'])
        assert summary['cpus'] == start['cpus'] == reg['controller_cpus']
        assert summary['cgroup_limits'] == start['cgroup_limits'] == reg['controller_cgroup_limits']
        assert summary['decision']['status'] == 'invalid'
        assert summary['completed_cells'] == ['p0-progress']
        launched = [c for c in reg['cells'] if (out / c['id']).exists()]
        assert [c['id'] for c in launched] == reg['batches'][0]
        assert summary['unrun_cells'] == [c['id'] for c in reg['cells'] if c not in launched]
        assert not list(out.glob('decision-*.json'))  # No complete triple, no paired decision.
        for cell in launched:
            cell_out = out / cell['id']
            read = lambda name: json.loads((cell_out / (name + '.json')).read_text())
            record = read('process')
            q = json.loads((ROOT / cell['request']).read_text())
            result, campaign, usage = [read('campaign/' + name) for name in ['result', 'campaign', 'usage']]
            progress_bytes = (cell_out / 'campaign/progress.jsonl').read_bytes()
            assert progress_bytes.endswith(b'\n')
            progress = json.loads(progress_bytes.splitlines()[-1])
            args = q, result, campaign, usage, progress, reg['accepted_drain_frames']
            if cell['arm'] == 'ordinary':
                assert 'slot_retention' not in campaign
                assert record['valid'] is False and record['assessment_error'] == "KeyError('slot_retention')"
                try:
                    cell_gate(*args)
                except KeyError as error:
                    assert error.args == ('slot_retention',)
                else:
                    raise AssertionError('The frozen defect must still reproduce')
            else:
                assert record['valid'] is True and cell_gate(*args)
            assert compatible_cell_gate(*args)
            assert result['stream_sha256'] == campaign['stream_sha256'] == digest((cell_out / 'campaign/stream.jsonl').read_bytes())
            for name, field in [('witness-local.json', 'local_input_sha256'), ('witness-full.json', 'full_input_sha256')]:
                assert digest((cell_out / 'campaign' / name).read_bytes()) == result['witness'][field]
            assert record['registration_sha256'] == digest(reg_bytes)
            assert record['cpus'] == cell['cpus'] and record['cgroup_limits'] == reg['cell_cgroup_limits']
            assert record['exit_code'] == 0 and record['killed'] is None
            assert 0 < record['wall_seconds'] <= reg['process_runtime_seconds']
            assert stamp(start['started_utc']) <= stamp(record['started_utc']) <= stamp(record['ended_utc']) <= stamp(summary['ended_utc'])
            journal = [json.loads(line) for line in (cell_out / 'service-journal.jsonl').read_bytes().splitlines()]
            assert any(j.get('INVOCATION_ID') == record['invocation_id'] for j in journal)
            if cell['arm'] == 'ordinary':
                # --collect removed the failed wrapper before systemctl show.
                # Its apparent default properties are not terminal receipts.
                terminal = read('service-terminal')
                assert terminal['LoadState'] == 'not-found'
                resources = next(j for j in journal if 'CPU_USAGE_NSEC' in j)
                cpu_ns, peak = int(resources['CPU_USAGE_NSEC']), int(resources['MEMORY_PEAK'])
                assert resources['MEMORY_SWAP_PEAK'] == '0'
            else:
                terminal = read('service-terminal')
                assert terminal['InvocationID'] == record['invocation_id']
                assert terminal['MainPID'] == '0' and terminal['Result'] == 'success'
                assert terminal['MemoryMax'] == reg['cell_cgroup_limits']['memory.max']
                assert terminal['MemorySwapMax'] == terminal['MemorySwapPeak'] == '0'
                assert terminal['TasksMax'] == reg['cell_cgroup_limits']['pids.max']
                assert terminal['CPUAffinity'] == terminal['AllowedCPUs'] == '4-7'
                assert terminal['RuntimeMaxUSec'] == '3min 30s'
                cpu_ns, peak = int(terminal['CPUUsageNSec']), int(terminal['MemoryPeak'])
            assert peak <= int(reg['cell_cgroup_limits']['memory.max'])
            closed = read('service-closed')
            assert closed['MainPID'] == '0' and closed['ActiveState'] == 'inactive' and closed['SubState'] == 'dead'
            physical = result['physical_frames']
            rows.append(dict(cell=cell['id'], seed=q['seed'], arm=cell['arm'],
                             native_completed=True, frozen_wrapper_valid=record['valid'],
                             schema_compatible_native_checks=True,
                             reached=result['milestone_reached_within_budget'],
                             descriptive_restricted_cost=restricted_cost(result['milestone_reached_within_budget'], physical['search_engine'], reg['physical_restricted_horizon']),
                             physical_frames=physical, admitted_frames=result['frames'],
                             auxiliary_frames=physical['total'] - result['frames'],
                             jobs=result['executions'], alternate_admissions=progress['retention_diagnostics']['alternative_admissions'],
                             wall_seconds=record['wall_seconds'], process_cpu_seconds=record['cpu_seconds'],
                             process_peak_rss_kib=record['peak_rss_kib'], service_cpu_ns=cpu_ns, service_peak_bytes=peak,
                             started_utc=record['started_utc'], ended_utc=record['ended_utc']))
        a, b = rows
        assert stamp(a['started_utc']) < stamp(b['ended_utc']) and stamp(b['started_utc']) < stamp(a['ended_utc'])
        assert set(launched[0]['cpus']).isdisjoint(launched[1]['cpus'])
        assert sum(r['physical_frames']['total'] for r in rows) <= reg['physical_frame_allocation']
        assert summary['measured_physical_frames'] == rows[1]['physical_frames']['total']
        closed = value('service-closed')
        assert closed['MainPID'] == '0' and closed['ActiveState'] == 'inactive' and closed['SubState'] == 'dead'
    ledger = json.loads((ROOT / 'ledger-after-pg02.json').read_text())
    assert ledger['before'] == reg['ledger_before']
    assert ledger['pg02'] == dict(admitted_search=sum(r['admitted_frames'] for r in rows), known_auxiliary=sum(r['auxiliary_frames'] for r in rows))
    assert ledger['after'] == {k: ledger['before'][k] + ledger['pg02'][k] for k in ledger['before']}
    assert ledger['pg02_total_measured_physical'] == sum(r['physical_frames']['total'] for r in rows)
    return dict(format='retention-pg02-invalid-closure-verification-v1', verified=True, files=len(paths),
                panel_status='invalid; no complete seed triple, no gate decision', cells=rows,
                measured_physical_frames=ledger['pg02_total_measured_physical'], ledger=ledger['after'],
                started_utc=start['started_utc'], ended_utc=summary['ended_utc'], unrun_cells=summary['unrun_cells'],
                controller_resource_unknown='Controller CPU/final cgroup peak were lost on failure collection. They are not zero. Native physical receipts for both binary calls are complete.',
                scope='Post-failure schema diagnosis and full cost closure only; frozen scorer remains unchanged, no emulator rerun, efficacy pass or fresh-search claim.')


if __name__ == '__main__':
    print(json.dumps(verify(), indent=2))
