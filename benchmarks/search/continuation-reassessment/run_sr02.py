#!/usr/bin/env python3
"""Frozen SR02 controller and individually constrained cells, on ms02 only."""
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
from score_sr02 import assess, panel_score

PROPERTIES = ['LoadState', 'ActiveState', 'SubState', 'MainPID', 'Result', 'ExecMainStatus',
              'InvocationID', 'MemoryMax', 'MemoryPeak', 'MemorySwapMax', 'MemorySwapPeak',
              'TasksMax', 'AllowedCPUs', 'CPUAffinity', 'RuntimeMaxUSec', 'CPUUsageNSec']


def state(unit):
    # Retry observation of the same unit; an observation timeout is not a
    # terminal process and never authorizes a duplicate launch.
    for attempt in range(3):
        try:
            result = subprocess.run(['systemctl', 'show', unit, '--property=' + ','.join(PROPERTIES)],
                                    capture_output=True, text=True, timeout=10)
            break
        except subprocess.TimeoutExpired:
            if attempt == 2:
                raise
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
                  cpu_seconds=usage.ru_utime + usage.ru_stime,
                  cpu_microseconds=round((usage.ru_utime + usage.ru_stime) * 1_000_000),
                  peak_rss_kib=usage.ru_maxrss,
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
    # Report completion separately from gate validity, preserving terminal state.
    return 0


def launch(master, registration, cell):
    assert state(cell['service'])['LoadState'] == 'not-found'
    cpu_list = str(cell['cpus'][0]) + '-' + str(cell['cpus'][-1])
    command = ['systemd-run', '--quiet', '--unit=' + cell['service'],
               '--property=RemainAfterExit=yes', '--property=MemoryMax=4G',
               '--property=MemorySwapMax=0', '--property=TasksMax=64',
               '--property=RuntimeMaxSec=' + str(master['cell_service_runtime_seconds']),
               '--property=AllowedCPUs=' + cpu_list, '--property=CPUAffinity=' + cpu_list,
               '--property=KillMode=control-group', '--property=TimeoutStopSec=5',
               '/usr/bin/python3', str(Path(master['protocol']) / 'run_sr02.py'), str(registration), cell['id']]
    subprocess.run(command, check=True, capture_output=True, timeout=15)



def finish_cell(unit, out, terminal):
    if not (out / 'service-terminal.json').exists():
        save(out / 'service-terminal.json', terminal)
    stopped = subprocess.run(['systemctl', 'stop', unit], capture_output=True, text=True, timeout=15)
    after = state(unit)
    if not (out / 'service-stop-command.json').exists():
        save(out / 'service-stop-command.json', dict(returncode=stopped.returncode, stderr=stopped.stderr))
    assert after.get('MainPID') == '0' and after.get('ActiveState') in ['inactive', 'failed']
    if not (out / 'service-stopped.json').exists():
        save(out / 'service-stopped.json', after)
    journal = subprocess.check_output(['journalctl', '-u', unit, '--no-pager', '-o', 'json'], timeout=15)
    (out / 'service-journal.jsonl').write_bytes(journal)


def controller(registration):
    master, start = preflight(registration)
    deadline = datetime.fromisoformat(master['deadline_utc'])
    assert (deadline - now()).total_seconds() >= master['controller_service_runtime_seconds']
    out = Path(master['output'])
    out.mkdir()
    save(out / 'start.json', start)
    rows, completed, launched = [], [], []
    decision = dict(status='invalid', reason='Controller did not complete a seed quartet.')

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
                    finish_cell(unit, cell_out, terminal)
                    del active[unit]
                    record = json.loads((cell_out / 'process.json').read_text())
                    assert terminal['Result'] == 'success' and terminal['ExecMainStatus'] == '0'
                    assert terminal['InvocationID'] == record['invocation_id']
                    rows.append(record)
                    completed.append(cell['id'])
                    assert record['valid'] is True
                    assert sum(r['physical_frames']['total'] for r in rows) <= master['physical_frame_allocation']
                    print(json.dumps({k: record[k] for k in ['cell', 'reached', 'restricted_interval', 'wall_seconds', 'admitted_frames', 'alternative_admissions']}), flush=True)
                if active:
                    time.sleep(1)
            # Each fixed seed quartet uses two batches of two concurrent cells.
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
            cell = next(c for c in master['cells'] if c['service'] == unit)
            cell_out = out / cell['id']
            cell_out.mkdir(exist_ok=True)
            if not (cell_out / 'service-stopped.json').exists():
                finish_cell(unit, cell_out, state(unit))
    # Include every launched cell, even a failed/partially collected one. Missing
    # or invalid physical receipts stay unknown instead of disappearing from cost.
    all_rows = []
    for cell in master['cells']:
        if cell['service'] not in launched:
            continue
        path = out / cell['id'] / 'process.json'
        row = json.loads(path.read_text()) if path.exists() else dict(cell=cell['id'], valid=False)
        all_rows.append(row)
    accounted = [r for r in all_rows if r.get('valid') is True]
    unknown = [r['cell'] for r in all_rows if r.get('valid') is not True]
    cg = Path('/sys/fs/cgroup') / start['cgroup'].lstrip('/')
    summary = dict(start, ended_utc=now().isoformat(), decision=decision, completed_cells=completed,
                   launched_cells=[c['id'] for c in master['cells'] if c['service'] in launched],
                   unrun_cells=[c['id'] for c in master['cells'] if c['service'] not in launched],
                   admitted_frames=sum(r['admitted_frames'] for r in accounted),
                   auxiliary_frames=sum(r['auxiliary_frames'] for r in accounted),
                   measured_physical_frames=sum(r['physical_frames']['total'] for r in accounted),
                   unresolved_cost_cells=unknown,
                   total_scope='All launched cells listed; validated complete costs summed, unresolved costs explicitly unknown.',
                   cgroup_before_exit={name: (cg / name).read_text().strip() for name in
                                      ['memory.peak', 'memory.swap.peak', 'cpu.stat', 'memory.events']},
                   cells=all_rows)
    save(out / 'complete.json', summary)
    print(json.dumps({k: summary[k] for k in ['decision', 'completed_cells', 'unrun_cells', 'measured_physical_frames']}), flush=True)
    return 0  # The complete report's decision carries qualification, not exit status.


if __name__ == '__main__':
    path = Path(sys.argv[1])
    raise SystemExit(run_cell(path, sys.argv[2]) if len(sys.argv) == 3 else controller(path))
