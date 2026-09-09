#!/usr/bin/env python3
"""Run an immutable paired panel with bounded Linux process-tree ownership."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import time


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def processes():
    result = {}
    for path in Path('/proc').glob('[0-9]*/stat'):
        try:
            # comm can contain spaces/parentheses; fields after its last ')' are stable.
            fields = path.read_text().rsplit(')', 1)[1].split()
            result[int(path.parent.name)] = (int(fields[1]), int(fields[2]), fields[19], fields[0])
        except (FileNotFoundError, ProcessLookupError):
            continue
    return result


def update_owned(root_pid, owned):
    current = processes()
    if not owned and root_pid in current:
        owned[root_pid] = current[root_pid][2]
    live = {pid for pid, birth in owned.items() if pid in current and current[pid][2] == birth}
    while True:
        children = {pid for pid, row in current.items() if row[0] in live}
        added = children - live
        if not added:
            break
        for pid in added:
            owned[pid] = current[pid][2]
        live.update(added)
    return current, live


def kill_owned(root_pid, owned):
    current, live = update_owned(root_pid, owned)
    groups = {current[pid][1] for pid in live if current[pid][3] != 'Z'}
    # A group leader may already have exited, but its identity was observed while owned.
    assert all(group in owned and (group not in current or current[group][2] == owned[group])
               for group in groups), 'refuse to signal an unowned or reused process group'
    killed = []
    for group in sorted(groups, reverse=True):
        try:
            os.killpg(group, signal.SIGKILL)
            killed.append(group)
        except ProcessLookupError:
            pass
    return killed


def bounded(command, log, seconds):
    start = time.monotonic()
    with log.open('xb') as stream:
        child = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
        owned = {}
        killed = []
        try:
            while child.poll() is None:
                update_owned(child.pid, owned)
                if time.monotonic() - start >= seconds:
                    break
                time.sleep(1)
        finally:
            # Also clean up observed session-creating descendants after an early runner exit.
            killed = kill_owned(child.pid, owned)
            child.wait(timeout=10)
    return {'exit_code': child.returncode, 'elapsed_seconds': time.monotonic() - start,
            'watchdog_killed_process_groups': killed, 'wall_bound_seconds': seconds}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('plan', type=Path)
    parser.add_argument('--check-only', action='store_true')
    args = parser.parse_args()
    plan = json.loads(args.plan.read_text())
    root = Path(plan['remote_root'])
    build = root / 'builds' / plan['build']
    assert sha(Path(plan['assets'])) == plan['assets_sha256']
    assert sha(build / 'nes-eval') == plan['binary_sha256']
    assert sha(build / 'build-info.json') == plan['build_info_sha256']
    assert len(plan['cells']) == 20
    assert len({cell['label'] for cell in plan['cells']}) == 20
    assert len({cell['seed'] for cell in plan['cells']}) == 10
    assert {cell['seed'] for cell in plan['cells']} == set(plan['seeds'])
    assert len({cpu for group in plan['cpu_sets'] for cpu in group}) == 16
    for seed in {cell['seed'] for cell in plan['cells']}:
        pair = [cell for cell in plan['cells'] if cell['seed'] == seed]
        assert {cell['arm'] for cell in pair} == {'control', 'corrected'}
        requests = [dict(cell['suite']['search']) for cell in pair]
        for request in requests:
            request.pop('metroid_terminal')
        assert requests[0] == requests[1]
        assert all(cell['suite']['seeds'] == [seed] for cell in pair)
    if args.check_only:
        print('validated immutable panel structure and binary identity')
        return
    output = root / 'validation' / plan['id']
    output.mkdir(parents=True, exist_ok=False)
    (output / 'plan.json').write_bytes(args.plan.read_bytes())
    (output / 'launcher.py').write_bytes(Path(__file__).read_bytes())
    deadline = datetime.fromisoformat(plan['deadline_utc']).timestamp()
    pending = queue.Queue()
    for cell in plan['cells']:
        pending.put(cell)

    def worker(cpus):
        while True:
            try:
                cell = pending.get_nowait()
            except queue.Empty:
                return
            label = cell['label']
            remaining = deadline - time.time()
            if remaining < 300:
                result = {'status': 'not_started_before_deadline'}
            else:
                manifest = output / (label + '.json')
                manifest.write_text(json.dumps(cell['suite'], indent=2) + '\n')
                command = ['taskset', '-c', ','.join(map(str, cpus)), 'python3',
                           str(root / 'benchmarks/search/eval.py'), 'run', str(manifest),
                           '--assets', plan['assets'], '--binary', str(build / 'nes-eval'),
                           '--build-info', str(build / 'build-info.json'), '--out', str(root / 'runs' / label),
                           '--jobs', '1', '--cpus', '4', '--memory-capacity-mib', '10240',
                           '--finish-seconds', '240', '--disk-limit-gib', '4']
                result = bounded(command, output / (label + '.log'), min(5400, remaining))
                result['status'] = 'runner_complete' if result['exit_code'] == 0 and not result['watchdog_killed_process_groups'] else 'incomplete_or_error'
            result.update(seed=cell['seed'], arm=cell['arm'], cpu_set=cpus)
            (output / (label + '-execution.json')).write_text(json.dumps(result, indent=2) + '\n')
            print(json.dumps({'label': label, **result}), flush=True)

    with ThreadPoolExecutor(max_workers=4) as pool:
        futures = [pool.submit(worker, cpus) for cpus in plan['cpu_sets']]
        for future in futures:
            future.result()
    (output / 'finished.json').write_text(json.dumps({'time_utc': datetime.now(timezone.utc).isoformat()}))


if __name__ == '__main__':
    main()
