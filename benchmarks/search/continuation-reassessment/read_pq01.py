#!/usr/bin/env python3
"""Two frozen paused checkpoint reads; no search, continuations or retries."""
from datetime import datetime
import gzip
import json
import os
from pathlib import Path
import resource
import signal
import socket
import subprocess
import sys
import time
from run_pg01 import now, save, sha


def paused_gate(q, inventory, report):
    rows = [{k: v for k, v in row.items() if k != 'context'} for row in report['entries']]
    return (report['format'] == 'metroid-paused-checkpoint-context-v1'
            and report['checkpoint_sha256'] == inventory['checkpoint_sha256'] == q['checkpoint_sha256']
            and report['origin_sha256'] == q['origin_sha256']
            and report['positive_control'] == q['expected_root_context']
            and report['direct_physical_frames'] == report['setup_frames'] == 929
            and report['continuation_frames'] == 0
            and report['verified_restores'] == len(inventory['entries']) + 1
            and rows == inventory['entries'])


def main(registration):
    master = json.loads(registration.read_text())
    protocol = Path(master['protocol'])
    deadline = datetime.fromisoformat(master['deadline_utc'])
    assert socket.gethostname() == master['host'] == 'ms02'
    assert datetime.fromisoformat(master['registered_utc']) <= now()
    assert (deadline - now()).total_seconds() >= master['service_runtime_seconds']
    for name, digest in master['protocol_sha256'].items():
        assert sha(protocol / name) == digest, name
    assert sha(master['binary']) == master['binary_sha256']
    assert sha(master['build_info']) == master['build_info_sha256']
    assert sorted(os.sched_getaffinity(0)) == master['cpus']
    cgroup = next(line.split('::', 1)[1] for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    cg = Path('/sys/fs/cgroup') / cgroup.lstrip('/')
    assert {name: (cg / name).read_text().strip() for name in master['cgroup_limits']} == master['cgroup_limits']
    out = Path(master['output'])
    out.mkdir()
    record = dict(registration_sha256=sha(registration), started_utc=now().isoformat(),
                  hostname=socket.gethostname(), cpus=master['cpus'], cgroup=cgroup,
                  cgroup_limits=master['cgroup_limits'], invocation_id=os.environ.get('INVOCATION_ID'))
    save(out / 'start.json', record)
    cells = []
    error = None
    try:
        for cell in master['cells']:
            qpath = protocol / cell['request']
            q = json.loads(qpath.read_text())
            inventory = json.loads(gzip.decompress((protocol / cell['inventory']).read_bytes()))
            for kind in ['core', 'rom', 'checkpoint', 'origin']:
                assert sha(q[kind]) == q[kind + '_sha256']
            command = [master['binary'], 'inspect', str(qpath), str(out / (cell['id'] + '.json'))]
            start = time.monotonic()
            current = dict(cell=cell['id'], command=command, started_utc=now().isoformat(), qualified=False)
            cells.append(current)
            save(out / (cell['id'] + '-start.json'), current)

            def limits():
                resource.setrlimit(resource.RLIMIT_FSIZE, (master['file_limit_bytes'], master['file_limit_bytes']))

            with (out / (cell['id'] + '.log')).open('xb') as log:
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                           start_new_session=True, preexec_fn=limits)
                killed = None
                while True:
                    pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                    if pid:
                        process.returncode = os.waitstatus_to_exitcode(status)
                        break
                    if killed is None and (time.monotonic() - start >= master['process_runtime_seconds']
                            or now() >= deadline
                            or sum(p.stat().st_size for p in out.rglob('*') if p.is_file()) > master['output_limit_bytes']):
                        killed = 'Process, allocation or output limit'
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                    time.sleep(0.1)
            current.update(exit_code=process.returncode, killed=killed, wall_seconds=time.monotonic() - start,
                           cpu_seconds=usage.ru_utime + usage.ru_stime, peak_rss_kib=usage.ru_maxrss,
                           ended_utc=now().isoformat())
            if process.returncode == 0 and killed is None:
                result = json.loads((out / (cell['id'] + '.json')).read_text())
                current['qualified'] = paused_gate(q, inventory, result) and result['request_sha256'] == sha(qpath)
                current['physical_frames'] = result['direct_physical_frames']
            if not current['qualified']:
                current['unknown'] = 'A missing native receipt is unknown, not zero. No retry or next read.'
            save(out / (cell['id'] + '-process.json'), current)
            print(json.dumps(current), flush=True)
            if not current['qualified']:
                break
    except (OSError, ValueError, KeyError, TypeError, AssertionError, subprocess.SubprocessError) as failure:
        error = repr(failure)
    # Capture service counters while they still exist. A successful reporting
    # process means the evidence was saved; only qualified certifies the query.
    record.update(ended_utc=now().isoformat(), cells=cells, error=error,
                  qualified=error is None and len(cells) == len(master['cells']) and all(c['qualified'] for c in cells),
                  measured_physical_frames=sum(c.get('physical_frames', 0) for c in cells),
                  incomplete_physical_receipts=[c['cell'] for c in cells if 'physical_frames' not in c],
                  cgroup_cpu_stat=(cg / 'cpu.stat').read_text(), cgroup_memory_peak=(cg / 'memory.peak').read_text().strip())
    save(out / 'complete.json', record)
    print(json.dumps(record), flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main(Path(sys.argv[1])))
