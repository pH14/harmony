#!/usr/bin/env python3
"""Run one frozen PC01 process under external resource and time guards."""
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def now():
    return datetime.now(timezone.utc)


def save(path, value):
    with path.open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def main(registration):
    master = json.loads(registration.read_text())
    protocol = Path(master['protocol'])
    deadline = datetime.fromisoformat(master['deadline_utc'])
    assert datetime.fromisoformat(master['registered_utc']) <= now()
    assert (deadline - now()).total_seconds() >= master['service_runtime_seconds']
    for name, digest in master['protocol_sha256'].items():
        assert sha(protocol / name) == digest, name
    assert sha(master['binary']) == master['binary_sha256']
    assert sha(master['build_info']) == master['build_info_sha256']
    assert sorted(os.sched_getaffinity(0)) == master['cpus']
    cgroup = next(line.split('::',1)[1] for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    cg = Path('/sys/fs/cgroup') / cgroup.lstrip('/')
    assert {name:(cg/name).read_text().strip() for name in master['cgroup_limits']} == master['cgroup_limits']
    out = Path(master['output'])
    out.mkdir()
    request = protocol / 'pc01-request.json'
    started = time.monotonic()
    record = dict(format='retention-pc01-process-v1', registration_sha256=sha(registration),
                  started_utc=now().isoformat(), hostname=socket.gethostname(),
                  invocation_id=os.environ.get('INVOCATION_ID'), cpus=master['cpus'],
                  cgroup=cgroup, cgroup_limits=master['cgroup_limits'], admitted_search_frames=0,
                  command=[master['binary'],'run',str(request),str(out/'probe')])
    save(out/'start.json',record)
    with (out/'probe.log').open('xb') as log:
        process = subprocess.Popen(record['command'], stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        killed = None
        while True:
            pid, status, usage = os.wait4(process.pid, os.WNOHANG)
            if pid:
                process.returncode = os.waitstatus_to_exitcode(status)
                break
            size = sum(p.stat().st_size for p in out.rglob('*') if p.is_file())
            if killed is None and (size > master['output_limit_bytes'] or
                    time.monotonic()-started > master['process_runtime_seconds'] or now() >= deadline):
                killed = 'output, process or allocation deadline'
                try:
                    os.killpg(process.pid,signal.SIGKILL)
                except ProcessLookupError:
                    pass
            time.sleep(0.1)
    record.update(exit_code=process.returncode, wall_seconds=time.monotonic()-started,
                  cpu_seconds=usage.ru_utime+usage.ru_stime, peak_rss_kib=usage.ru_maxrss,
                  killed=killed, ended_utc=now().isoformat())
    for name in ['usage','complete']:
        path = out/'probe'/f'{name}.json'
        if path.exists():
            record[name] = json.loads(path.read_text())
            record[f'{name}_sha256'] = sha(path)
    log = out/'probe'/'trials.jsonl'
    if log.exists():
        for line in log.read_bytes().splitlines(keepends=True):
            if line.endswith(b'\n'):
                record['last_cost_receipt'] = json.loads(line)['cost']
    record['decision'] = 'complete' if (process.returncode == 0 and
            record.get('complete',{}).get('episodes') == 64 and record.get('usage',{}).get('complete')
            and record['usage']['physical_frames_known'] <= 1100000) else 'failed_no_retry'
    save(out/'process.json', record)
    print(json.dumps(record),flush=True)
    return record['decision'] != 'complete'


if __name__ == '__main__':
    raise SystemExit(main(Path(sys.argv[1])))
