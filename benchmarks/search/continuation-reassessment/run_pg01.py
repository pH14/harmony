#!/usr/bin/env python3
"""Run the three frozen PG01 qualification cells; no retries or utility claim."""
import hashlib
import json
import os
from pathlib import Path
import resource
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
    with path.open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def qualified(cell, request, result, campaign, usage, progress):
    """Activation is required in each optional arm, independent of boss outcome."""
    alternatives = progress.get('retention_diagnostics', {}).get('alternative_admissions', 0)
    return (result.get('complete') is True
            and result.get('full_campaign_replay') is True
            and result.get('root_local_witness_replays') == 2
            and result.get('complete_prefix_witness_replays') == 2
            and result.get('root', {}).get('snapshot_sha256') == request['expected_snapshot_sha256']
            and result.get('root', {}).get('qualified_retention_progress') == request['expected_retention_progress']
            and campaign.get('slot_retention') == request['slot_retention']
            and progress.get('executions') == result.get('executions')
            and progress.get('frames_emulated') == result.get('frames')
            and (alternatives > 0 if cell['require_alternate'] else alternatives == 0)
            and usage.get('completed_execution') is True
            and result['cost'] == usage['cost']
            and result['cost']['admitted_search_frames'] == result['frames']
            and result['cost']['campaign_replay_admitted_frames'] == result['frames']
            and result['cost']['direct_physical_frames'] <= request['direct_frame_limit']
            and result['frames'] <= request['frames'] + cell['max_drain_frames'])


def main(registration):
    master = json.loads(registration.read_text())
    protocol = Path(master['protocol'])
    deadline = datetime.fromisoformat(master['deadline_utc'])
    assert socket.gethostname() == master['host']
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
    start = dict(format='retention-pg01-process-v1', registration_sha256=sha(registration),
                 started_utc=now().isoformat(), hostname=socket.gethostname(),
                 invocation_id=os.environ.get('INVOCATION_ID'), cpus=master['cpus'],
                 cgroup=cgroup, cgroup_limits=master['cgroup_limits'])
    save(out / 'start.json', start)
    records = []
    for cell in master['cells']:
        assert (deadline - now()).total_seconds() >= cell['process_runtime_seconds']
        request_path = protocol / cell['request']
        request = json.loads(request_path.read_text())
        for kind in ['core', 'rom', 'input']:
            assert sha(request[kind]) == request[kind + '_sha256']
        command = [master['binary'], 'run', str(request_path), str(out / cell['id'])]
        record = dict(cell=cell['id'], command=command, started_utc=now().isoformat())
        save(out / (cell['id'] + '-start.json'), record)
        started = time.monotonic()

        def child_limits():
            resource.setrlimit(resource.RLIMIT_FSIZE, (master['file_limit_bytes'], master['file_limit_bytes']))

        with (out / (cell['id'] + '.log')).open('xb') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                       start_new_session=True, preexec_fn=child_limits)
            killed = None
            while True:
                pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                if pid:
                    process.returncode = os.waitstatus_to_exitcode(status)
                    break
                size = sum(p.stat().st_size for p in out.rglob('*') if p.is_file())
                if killed is None and (size > master['output_limit_bytes'] or
                        time.monotonic() - started > cell['process_runtime_seconds'] or now() >= deadline):
                    killed = 'output, process or allocation deadline'
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                time.sleep(0.25)
        record.update(exit_code=process.returncode, wall_seconds=time.monotonic() - started,
                      cpu_seconds=usage.ru_utime + usage.ru_stime, peak_rss_kib=usage.ru_maxrss,
                      killed=killed, ended_utc=now().isoformat())
        for name in ['usage', 'result', 'campaign']:
            path = out / cell['id'] / (name + '.json')
            if path.exists():
                record[name] = json.loads(path.read_text())
                record[name + '_sha256'] = sha(path)
        path = out / cell['id'] / 'progress.jsonl'
        if path.exists():
            for line in path.read_bytes().splitlines(keepends=True):
                if line.endswith(b'\n'):
                    record['progress'] = json.loads(line)
            record['progress_sha256'] = sha(path)
        record['qualified'] = bool(process.returncode == 0 and killed is None
                                  and qualified(cell, request, record.get('result', {}),
                                                record.get('campaign', {}), record.get('usage', {}),
                                                record.get('progress', {})))
        save(out / (cell['id'] + '-process.json'), record)
        records.append(record)
        print(json.dumps({k: record[k] for k in ['cell', 'exit_code', 'wall_seconds', 'qualified']}), flush=True)
        if not record['qualified']:
            break
    summary = dict(start, ended_utc=now().isoformat(), cells_completed=len(records),
                   qualified=len(records) == len(master['cells']) and all(r['qualified'] for r in records),
                   known_admitted_frames=sum(r.get('usage', {}).get('cost', {}).get('admitted_search_frames', 0) for r in records),
                   known_auxiliary_frames=sum(r.get('usage', {}).get('cost', {}).get('direct_physical_frames', 0)
                                              + r.get('usage', {}).get('cost', {}).get('campaign_replay_admitted_frames', 0) for r in records),
                   unknown='Engine setup, unadmitted and reconstruction frames are not fully exposed. Missing receipts after interruption are unknown, not zero.',
                   cells=[{k: r[k] for k in ['cell', 'exit_code', 'wall_seconds', 'cpu_seconds', 'peak_rss_kib', 'qualified']} for r in records])
    save(out / 'complete.json', summary)
    print(json.dumps(summary), flush=True)
    return not summary['qualified']


if __name__ == '__main__':
    raise SystemExit(main(Path(sys.argv[1])))
