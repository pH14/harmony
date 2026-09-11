#!/usr/bin/env python3
"""Archived PW01 runner; the checked-in allocation is cancelled and cannot run."""
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
from score_pw01 import assess, first_word


def main(registration):
    master = json.loads(registration.read_text())
    if master.get('status') != 'active' or master.get('executable') is not True:
        raise ValueError('PW01 allocation is cancelled or not explicitly executable')
    protocol = Path(master['protocol'])
    deadline = datetime.fromisoformat(master['deadline_utc'])
    assert socket.gethostname() == master['host'] == 'ms02'
    assert datetime.fromisoformat(master['registered_utc']) <= now()
    assert (deadline - now()).total_seconds() >= master['service_runtime_seconds']
    assert sorted(os.sched_getaffinity(0)) == master['cpus']
    for name, digest in master['protocol_sha256'].items():
        assert sha(protocol / name) == digest, name
    assert sha(master['binary']) == master['binary_sha256']
    assert sha(master['build_info']) == master['build_info_sha256']
    cgroup = next(line.split('::', 1)[1] for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    cg = Path('/sys/fs/cgroup') / cgroup.lstrip('/')
    assert {name: (cg / name).read_text().strip() for name in master['cgroup_limits']} == master['cgroup_limits']
    out = Path(master['output'])
    out.mkdir()
    start = time.monotonic()
    save(out / 'start.json', dict(started_utc=now().isoformat(), registration_sha256=sha(registration),
                                host=socket.gethostname(), invocation_id=os.environ.get('INVOCATION_ID'),
                                cgroup=cgroup, cpus=master['cpus']))
    rows = []
    for cell in master['cells']:
        assert now() < deadline
        qpath = protocol / cell['request']
        q = json.loads(qpath.read_text())
        for kind in ['core', 'rom', 'input']:
            assert sha(q[kind]) == q[kind + '_sha256']
        cell_out = out / cell['id']
        cell_out.mkdir()
        command = [master['binary'], 'run', str(qpath), str(cell_out / 'campaign')]
        record = dict(cell=cell['id'], command=command, started_utc=now().isoformat())
        save(cell_out / 'start.json', record)
        started = time.monotonic()

        def limits():
            resource.setrlimit(resource.RLIMIT_FSIZE, (master['file_limit_bytes'], master['file_limit_bytes']))

        with (cell_out / 'cell.log').open('xb') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                       start_new_session=True, preexec_fn=limits)
            killed = None
            while True:
                pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                if pid:
                    process.returncode = os.waitstatus_to_exitcode(status)
                    break
                size = sum(p.stat().st_size for p in out.rglob('*') if p.is_file())
                if killed is None and (size > master['output_limit_bytes'] or
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
        try:
            if process.returncode == 0 and killed is None:
                record.update(assess(master, cell, cell_out / 'campaign'))
        except (OSError, ValueError, KeyError, TypeError, AssertionError) as error:
            record['assessment_error'] = repr(error)
        if not record['valid']:
            record['unknown'] = 'Failed/interrupted work remains unknown unless surviving usage closes it; never zero.'
        save(cell_out / 'process.json', record)
        rows.append(record)
        print(json.dumps(record), flush=True)
        if not record['valid']:
            break
        assert sum(r['physical_frames']['total'] for r in rows) <= master['physical_frame_ceiling']
    activation = dict(activated=False, reason='Required cells incomplete')
    if len(rows) == len(master['cells']) and all(r['valid'] for r in rows):
        try:
            activation = first_word(out / 'fresh-control/campaign/stream.jsonl',
                                      out / 'reuse/campaign/stream.jsonl')
        except (OSError, ValueError, KeyError, TypeError, AssertionError) as error:
            activation = dict(activated=False, error=repr(error))
    qualified = (len(rows) == len(master['cells']) and all(r['valid'] for r in rows)
                 and activation['activated'] and all(r['alternative_admissions'] > 0
                     for r in rows))
    result = dict(format='progress-word-pw01-result-v1', qualified=qualified, cells=rows,
                  activation=activation, ended_utc=now().isoformat(), wall_seconds=time.monotonic() - start,
                  cgroup_before_exit={name: (cg / name).read_text().strip() for name in
                                     ['memory.peak', 'memory.swap.peak', 'cpu.stat', 'memory.events']},
                  scope='Implementation qualification only; no efficacy or fresh-search claim. No retries.')
    save(out / 'summary.json', result)
    # A complete report is distinct from a passing qualification. RemainAfterExit
    # keeps all terminal resource receipts observable even for a negative result.
    print(json.dumps(dict(qualified=qualified, completed_cells=len(rows))), flush=True)


if __name__ == '__main__':
    main(Path(sys.argv[1]))
