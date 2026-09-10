#!/usr/bin/env python3
"""One frozen PM01 accounting qualification; no retry or efficacy allocation."""
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
from run_pc01 import sha, now, save
from run_pg01 import qualified as activation_gate


def accounting_gate(q, result):
    cost = result['cost']
    physical = result.get('physical_frames', {})
    totals = []
    for phase, count, admitted in [('search_engine', q['workers'] + 1, cost['admitted_search_frames']),
                                   ('replay_engine', 1, cost['campaign_replay_admitted_frames'])]:
        receipt = cost.get(phase, {})
        if (receipt.get('construction_attempts') != count or receipt.get('targets_created') != count
                or receipt.get('targets_closed') != count or receipt.get('invalid_counter') is not False
                or receipt.get('constructor_frames') != count * 929
                or receipt.get('closed_post_constructor_frames') != admitted):
            return False
        total = receipt['constructor_frames'] + admitted
        if physical.get(phase) != total:
            return False
        totals.append(total)
    return (result.get('format') == 'metroid-archive-challenge-physical-result-v2'
            and physical.get('total') == cost['direct_physical_frames'] + sum(totals)
            and physical.get('direct_helpers') == cost['direct_physical_frames']
            and physical.get('search_outside_admission') == (q['workers'] + 1) * 929
            and physical.get('replay_outside_admission') == 929)


def assess(master, q, output):
    value = lambda name: json.loads((output / (name + '.json')).read_text())
    result, campaign, usage = value('result'), value('campaign'), value('usage')
    progress_bytes = (output / 'progress.jsonl').read_bytes()
    assert progress_bytes.endswith(b'\n')
    progress = json.loads(progress_bytes.splitlines()[-1])
    matched = {name: sha(output / name) == digest for name, digest in master['expected_artifacts'].items()}
    passed = (activation_gate(master['cell'], q, result, campaign, usage, progress)
              and accounting_gate(q, result) and all(matched.values())
              and result['physical_frames']['total'] == master['expected_physical_frames'])
    return dict(qualified=passed, matched_artifacts=matched,
                physical_frames=result.get('physical_frames'), cost=usage['cost'],
                alternative_admissions=progress['retention_diagnostics']['alternative_admissions'],
                admitted_search_frames=result['frames'],
                known_auxiliary_frames=result['physical_frames']['total'] - result['frames'])


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
    qpath = protocol / 'pm01-request.json'
    q = json.loads(qpath.read_text())
    for kind in ['core', 'rom', 'input']:
        assert sha(q[kind]) == q[kind + '_sha256']
    out = Path(master['output'])
    out.mkdir()
    record = dict(format='retention-pm01-process-v1', registration_sha256=sha(registration),
                  started_utc=now().isoformat(), hostname=socket.gethostname(),
                  invocation_id=os.environ.get('INVOCATION_ID'), cpus=master['cpus'],
                  cgroup=cgroup, cgroup_limits=master['cgroup_limits'],
                  command=[master['binary'], 'run', str(qpath), str(out / 'cell')])
    save(out / 'start.json', record)

    def child_limits():
        resource.setrlimit(resource.RLIMIT_FSIZE, (master['file_limit_bytes'], master['file_limit_bytes']))

    started = time.monotonic()
    with (out / 'cell.log').open('xb') as log:
        process = subprocess.Popen(record['command'], stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True, preexec_fn=child_limits)
        killed = None
        while True:
            pid, status, usage = os.wait4(process.pid, os.WNOHANG)
            if pid:
                process.returncode = os.waitstatus_to_exitcode(status)
                break
            size = sum(p.stat().st_size for p in out.rglob('*') if p.is_file())
            if killed is None and (size > master['output_limit_bytes'] or
                    time.monotonic() - started > master['process_runtime_seconds'] or now() >= deadline):
                killed = 'output, process or allocation deadline'
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            time.sleep(0.25)
    record.update(exit_code=process.returncode, wall_seconds=time.monotonic() - started,
                  cpu_seconds=usage.ru_utime + usage.ru_stime, peak_rss_kib=usage.ru_maxrss,
                  killed=killed, ended_utc=now().isoformat(), qualified=False)
    usage_path = out / 'cell' / 'usage.json'
    if usage_path.exists():
        record['usage_sha256'] = sha(usage_path)
    if process.returncode == 0 and killed is None:
        try:
            record.update(assess(master, q, out / 'cell'))
        except (OSError, ValueError, KeyError, TypeError, AssertionError) as error:
            record['qualification_error'] = str(error)
    else:
        record['unknown'] = 'Failed or interrupted work is not zero; inspect available usage and lifetime receipts. No retry.'
    save(out / 'process.json', record)
    print(json.dumps(record), flush=True)
    return not record['qualified']


if __name__ == '__main__':
    raise SystemExit(main(Path(sys.argv[1])))
