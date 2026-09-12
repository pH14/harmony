#!/usr/bin/env python3
"""Run only the two frozen RR01 replay stages; never generate search work."""
import gzip
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
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def now():
    return datetime.now(timezone.utc)


def save(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def main(protocol):
    master_path = protocol / 'rr01-registration.json'
    master = json.loads(master_path.read_text())
    deadline = datetime.fromisoformat(master['deadline_utc'])
    assert datetime.fromisoformat(master['registered_utc']) <= now()
    assert (deadline - now()).total_seconds() >= master['service_runtime_seconds']
    for name, digest in master['protocol_sha256'].items():
        assert sha(protocol / name) == digest, name
    assert sha(master['binary']) == master['binary_sha256']
    assert sha(master['build_info']) == master['build_info_sha256']
    affinity = sorted(os.sched_getaffinity(0))
    assert affinity == master['cpus'], affinity
    cgroup = next(line.split('::', 1)[1] for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    cg = Path('/sys/fs/cgroup') / cgroup.lstrip('/')
    limits = {name: (cg / name).read_text().strip() for name in master['cgroup_limits']}
    assert limits == master['cgroup_limits'], limits
    root = Path(master['output'])
    root.mkdir()
    inputs = Path(master['inputs'])
    inputs.mkdir()
    for item in master['saved_inputs']:
        with gzip.open(item['gzip_source'], 'rb') as stream:
            data = stream.read(32 * 1024 * 1024 + 1)
        assert len(data) == item['bytes']
        assert hashlib.sha256(data).hexdigest() == item['sha256']
        with (inputs / item['name']).open('xb') as stream:
            stream.write(data)
    result = dict(format='retention-replay-rr01-process-v1', registration_sha256=sha(master_path),
                  started_utc=now().isoformat(), hostname=socket.gethostname(), cpus=affinity,
                  cgroup=cgroup, cgroup_limits=limits, invocation_id=os.environ.get('INVOCATION_ID'),
                  phases=[], decision='incomplete', admitted_search_frames=0)

    def phase(name, request_path):
        assert sha(master['binary']) == master['binary_sha256']
        request = json.loads(request_path.read_text())
        remaining = (deadline - now()).total_seconds()
        assert remaining >= request['wall_seconds'] + 15
        started = time.monotonic()
        command = [master['binary'], str(request_path), str(root / name)]
        with (root / (name + '.log')).open('xb') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                       start_new_session=True)
            killed = None
            while True:
                pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                if pid:
                    process.returncode = os.waitstatus_to_exitcode(status)
                    break
                size = sum(p.stat().st_size for p in root.rglob('*') if p.is_file())
                if killed is None and (size > master['output_limit_bytes']
                        or time.monotonic() - started > request['wall_seconds'] + 10
                        or now() >= deadline):
                    killed = 'external output, process or allocation deadline'
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                time.sleep(0.2)
        record = dict(phase=name, command=command, request_sha256=sha(request_path),
                      exit_code=process.returncode, wall_seconds=time.monotonic() - started,
                      cpu_seconds=usage.ru_utime + usage.ru_stime, peak_rss_kib=usage.ru_maxrss,
                      killed=killed, ended_utc=now().isoformat())
        report_path = root / name / 'result.json'
        if report_path.exists():
            record['result_sha256'] = sha(report_path)
            record['result'] = json.loads(report_path.read_text())
        result['phases'].append(record)
        save(root / (name + '-process.json'), record)
        print(json.dumps(record), flush=True)
        return record

    try:
        qualification = phase('qualify', protocol / 'rr01-qualify-request.json')
        if qualification['exit_code'] != 0:
            result['decision'] = 'qualification_failed_inspection_unrun'
            raise RuntimeError('qualification failed; inspection remains unrun')
        report = qualification.get('result', {})
        assert report.get('status') == 'complete' and report.get('phase') == 'qualify'
        assert report.get('verified_jobs') == 4 and report.get('verified_job_frames') == 865
        assert report['binding']['executable_sha256'] == master['binary_sha256']
        inspection = json.loads((protocol / 'rr01-inspect-template.json').read_text())
        assert inspection['qualification'] is None
        inspection['qualification'] = dict(path=str(root / 'qualify/result.json'),
                                           sha256=qualification['result_sha256'])
        inspection_path = root / 'inspect-request.json'
        save(inspection_path, inspection)
        inspected = phase('inspect', inspection_path)
        result['decision'] = 'ready_for_offline_scoring' if inspected['exit_code'] == 0 else 'inspection_failed'
    finally:
        result['ended_utc'] = now().isoformat()
        result['known_auxiliary_frames'] = sum(
            p['result']['known_replay_frames'] + p['result']['known_inspector_setup_frames']
            for p in result['phases'] if p.get('result', {}).get('status') == 'complete')
        result['accounting_gap'] = 'Replay constructor counters and unsuccessful partial replay work are unavailable, not zero.'
        result['files'] = {str(p.relative_to(root)): sha(p) for p in root.rglob('*') if p.is_file()}
        save(root / 'rr01-process.json', result)
    if result['decision'] != 'ready_for_offline_scoring':
        raise SystemExit(1)


if __name__ == '__main__':
    main(Path(sys.argv[1]))
