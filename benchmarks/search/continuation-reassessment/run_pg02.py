#!/usr/bin/env python3
"""Frozen PG02 controller and individually constrained cells, on ms02 only."""
from datetime import datetime
import json
import os
from pathlib import Path
import resource
import signal
import socket
import subprocess
import sys
import time
from run_pg01 import sha, now, save
from score_pg02 import assess, panel_score

PROPERTIES = ['LoadState', 'ActiveState', 'SubState', 'MainPID', 'Result', 'ExecMainStatus',
              'InvocationID', 'MemoryMax', 'MemoryPeak', 'MemorySwapMax', 'MemorySwapPeak',
              'TasksMax', 'AllowedCPUs', 'CPUAffinity', 'RuntimeMaxUSec', 'CPUUsageNSec']


def state(unit):
    result = subprocess.run(['systemctl', 'show', unit, '--property=' + ','.join(PROPERTIES)],
                            capture_output=True, text=True, timeout=10)
    values = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
    assert values.get('LoadState') is not None, result.stderr
    return values


def preflight(registration, cell=None):
    master = json.loads(registration.read_text())
    assert socket.gethostname() == master['host'] == 'ms02'
    assert datetime.fromisoformat(master['registered_utc']) <= now() < datetime.fromisoformat(master['deadline_utc'])
    protocol = Path(master['protocol'])
    for name, digest in master['protocol_sha256'].items():
        assert sha(protocol / name) == digest, name
    assert sha(master['binary']) == master['binary_sha256']
    assert sha(master['build_info']) == master['build_info_sha256']
    limits = master['cell_cgroup_limits'] if cell else master['controller_cgroup_limits']
    cpus = cell['cpus'] if cell else master['controller_cpus']
    assert sorted(os.sched_getaffinity(0)) == cpus
    cgroup = next(line.split('::', 1)[1] for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    cg = Path('/sys/fs/cgroup') / cgroup.lstrip('/')
    assert {name: (cg / name).read_text().strip() for name in limits} == limits
    return master, dict(registration_sha256=sha(registration), hostname=socket.gethostname(),
                        started_utc=now().isoformat(), invocation_id=os.environ.get('INVOCATION_ID'),
                        cpus=cpus, cgroup=cgroup, cgroup_limits=limits)


def run_cell(registration, cell_id):
    master = json.loads(registration.read_text())
    cell = next(c for c in master['cells'] if c['id'] == cell_id)
    master, record = preflight(registration, cell)
    qpath = Path(master['protocol']) / cell['request']
    q = json.loads(qpath.read_text())
    for kind in ['core', 'rom', 'input']:
        assert sha(q[kind]) == q[kind + '_sha256']
    out = Path(master['output']) / cell_id
    out.mkdir()
    record.update(cell=cell_id, command=[master['binary'], 'run', str(qpath), str(out / 'campaign')])
    save(out / 'start.json', record)
    started = time.monotonic()
    deadline = datetime.fromisoformat(master['deadline_utc'])

    def limits():
        resource.setrlimit(resource.RLIMIT_FSIZE, (master['file_limit_bytes'], master['file_limit_bytes']))

    with (out / 'cell.log').open('xb') as log:
        process = subprocess.Popen(record['command'], stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True, preexec_fn=limits)
        killed = None
        while True:
            pid, status, usage = os.wait4(process.pid, os.WNOHANG)
            if pid:
                process.returncode = os.waitstatus_to_exitcode(status)
                break
            size = sum(p.stat().st_size for p in out.rglob('*') if p.is_file())
            if killed is None and (size > master['cell_output_limit_bytes'] or
                    time.monotonic() - started >= master['process_runtime_seconds'] or now() >= deadline):
                killed = 'Output, process or allocation deadline'
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            time.sleep(0.25)
    record.update(exit_code=process.returncode, wall_seconds=time.monotonic() - started,
                  cpu_seconds=usage.ru_utime + usage.ru_stime, peak_rss_kib=usage.ru_maxrss,
                  killed=killed, ended_utc=now().isoformat(), valid=False)
    if process.returncode == 0 and killed is None:
        try:
            record.update(assess(master, cell, out / 'campaign'))
        except (OSError, ValueError, KeyError, TypeError, AssertionError) as error:
            record['assessment_error'] = repr(error)
    if not record['valid']:
        record['unknown'] = 'Inspect available usage; missing or interrupted work is unknown, never zero. No retry.'
    save(out / 'process.json', record)
    print(json.dumps(record), flush=True)
    return not record['valid']


def launch(master, registration, cell):
    assert state(cell['service'])['LoadState'] == 'not-found'
    cpu_list = str(cell['cpus'][0]) + '-' + str(cell['cpus'][-1])
    command = ['systemd-run', '--quiet', '--collect', '--unit=' + cell['service'],
               '--property=RemainAfterExit=yes', '--property=MemoryMax=4G',
               '--property=MemorySwapMax=0', '--property=TasksMax=64',
               '--property=RuntimeMaxSec=' + str(master['cell_service_runtime_seconds']),
               '--property=AllowedCPUs=' + cpu_list, '--property=CPUAffinity=' + cpu_list,
               '--property=KillMode=control-group', '--property=TimeoutStopSec=5',
               '/usr/bin/python3', str(Path(master['protocol']) / 'run_pg02.py'), str(registration), cell['id']]
    subprocess.run(command, check=True, capture_output=True, timeout=15)


def controller(registration):
    master, start = preflight(registration)
    deadline = datetime.fromisoformat(master['deadline_utc'])
    assert (deadline - now()).total_seconds() >= master['controller_service_runtime_seconds']
    out = Path(master['output'])
    out.mkdir()
    save(out / 'start.json', start)
    rows, completed, launched = [], [], []
    decision = dict(status='invalid', reason='Controller did not complete a seed triple.')

    def interrupted(signum, frame):
        raise RuntimeError('Controller interrupted: ' + str(signum))

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        for batch_index, batch in enumerate(master['batches']):
            assert (deadline - now()).total_seconds() > master['cell_service_runtime_seconds'] + 15
            cells = [next(c for c in master['cells'] if c['id'] == key) for key in batch]
            assert 1 <= len(cells) <= 2
            assert len({cpu for c in cells for cpu in c['cpus']}) == 4 * len(cells)
            active = {}
            for cell in cells:
                launched.append(cell['service'])
                launch(master, registration, cell)
                active[cell['service']] = cell
            batch_started = time.monotonic()
            while active:
                assert now() < deadline
                assert time.monotonic() - batch_started <= master['cell_service_runtime_seconds'] + 15
                assert sum(p.stat().st_size for p in out.rglob('*') if p.is_file()) <= master['output_limit_bytes']
                for unit, cell in list(active.items()):
                    terminal = state(unit)
                    if terminal.get('MainPID') != '0' or terminal.get('SubState') == 'start':
                        continue
                    cell_out = out / cell['id']
                    cell_out.mkdir(exist_ok=True)
                    save(cell_out / 'service-terminal.json', terminal)
                    subprocess.run(['systemctl', 'stop', unit], check=True, capture_output=True, timeout=15)
                    save(cell_out / 'service-stopped.json', state(unit))
                    del active[unit]
                    record = json.loads((cell_out / 'process.json').read_text())
                    assert terminal['Result'] == 'success' and terminal['ExecMainStatus'] == '0'
                    assert terminal['InvocationID'] == record['invocation_id']
                    assert record['valid'] is True
                    assert sum(r['physical_frames']['total'] for r in rows) + record['physical_frames']['total'] <= master['physical_frame_allocation']
                    rows.append(record)
                    completed.append(cell['id'])
                    print(json.dumps({k: record[k] for k in ['cell', 'reached', 'restricted_cost', 'wall_seconds', 'admitted_frames', 'alternative_admissions']}), flush=True)
                if active:
                    time.sleep(1)
            # Each fixed pair uses two batches (two concurrent cells, then one).
            if batch_index % 2 == 1:
                decision = panel_score(rows)
                save(out / ('decision-' + str(batch_index // 2) + '.json'), decision)
                print(json.dumps(decision), flush=True)
                if decision['status'] != 'continue':
                    break
    except (OSError, ValueError, KeyError, TypeError, AssertionError, RuntimeError, subprocess.SubprocessError) as error:
        decision = dict(status='invalid', reason=repr(error),
                        unknown='Preserve partial cells and available receipts; interrupted or missing work is not zero. No retry.')
    finally:
        for unit in launched:
            # Stop only units belonging to this frozen registration.
            subprocess.run(['systemctl', 'stop', unit], capture_output=True, timeout=15)
    summary = dict(start, ended_utc=now().isoformat(), decision=decision, completed_cells=completed,
                   unrun_cells=[c['id'] for c in master['cells'] if c['service'] not in launched],
                   admitted_frames=sum(r['admitted_frames'] for r in rows),
                   auxiliary_frames=sum(r['auxiliary_frames'] for r in rows),
                   measured_physical_frames=sum(r['physical_frames']['total'] for r in rows),
                   total_scope='Validated completed cells; any invalid or interrupted cells require separate ledger closure.',
                   cells=rows)
    save(out / 'complete.json', summary)
    print(json.dumps({k: summary[k] for k in ['decision', 'completed_cells', 'unrun_cells', 'measured_physical_frames']}), flush=True)
    return decision['status'] == 'invalid'


if __name__ == '__main__':
    path = Path(sys.argv[1])
    raise SystemExit(run_cell(path, sys.argv[2]) if len(sys.argv) == 3 else controller(path))
