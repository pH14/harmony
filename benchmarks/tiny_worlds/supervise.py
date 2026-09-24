#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run one bounded tiny-world job in an isolated process group.

Reservations coordinate supervisors which share the same output parent. The
resource samples are guardrails, not hard memory or aggregate-disk limits.
"""
from __future__ import annotations

import argparse
import contextlib
import fcntl
import json
import math
import os
from pathlib import Path
import platform
import re
import resource
import shutil
import signal
import subprocess
import sys
import threading
import time
import uuid

GB = 1_000_000_000
CPU_CAP = 8
MEMORY_CAP = 40 * GB
DISK_CAP = 80 * GB
HOST_MEMORY_STOP = 44 * GB
HOST_DISK_STOP = 90 * GB
LOG_CAP = 1 * 1024 * 1024
TELEMETRY_CAP = 4 * 1024 * 1024
SAMPLE_SECONDS = 0.2
TERM_GRACE_SECONDS = 0.2
MAX_SECONDS = 3600


def _linux_memory():
    values = {}
    for line in Path('/proc/meminfo').read_text().splitlines():
        match = re.match(r'^(MemTotal|MemAvailable):\s+(\d+)\s+kB$', line)
        if match:
            values[match.group(1)] = int(match.group(2)) * 1024
    if not {'MemTotal', 'MemAvailable'} <= values.keys():
        raise RuntimeError('could not read Linux memory headroom')
    return values['MemTotal'], values['MemAvailable']


def _darwin_memory():
    pages = os.sysconf('SC_PHYS_PAGES')
    page_size = os.sysconf('SC_PAGE_SIZE')
    total = pages * page_size
    output = subprocess.check_output(['/usr/bin/vm_stat'], text=True, timeout=2)
    fields = dict((name.strip(), int(count.replace('.', ''))) for name, count in
                  re.findall(r'^Pages ([A-Za-z ]+):\s+(\d+)\.?$', output, re.M))
    available_pages = sum(fields.get(name, 0) for name in
                          ('free', 'inactive', 'speculative', 'purgeable'))
    return total, available_pages * page_size


def host_memory():
    if platform.system() == 'Linux':
        return _linux_memory()
    if platform.system() == 'Darwin':
        return _darwin_memory()
    raise RuntimeError('host memory probing supports macOS and Linux only')


def _linux_group_rss(pgid):
    total = 0
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            # comm may contain spaces and parentheses; later fields follow its final ')'.
            fields = (entry / 'stat').read_text().rsplit(')', 1)[1].split()
            if int(fields[2]) == pgid:
                total += int((entry / 'statm').read_text().split()[1]) * os.sysconf('SC_PAGE_SIZE')
        except (OSError, ValueError, IndexError):
            continue
    return total


def process_group_rss(pgid):
    """Return sampled RSS for the process group, or None when unavailable."""
    try:
        if platform.system() == 'Linux':
            return _linux_group_rss(pgid)
        if platform.system() == 'Darwin':
            output = subprocess.check_output(
                ['/bin/ps', '-axo', 'pgid=,rss='], text=True, timeout=2)
            return sum(int(rss) * 1024 for group, rss in
                       (line.split() for line in output.splitlines()) if int(group) == pgid)
    except (OSError, subprocess.SubprocessError, ValueError, IndexError):
        return None
    return None


def _parse_disk_roots(paths):
    roots = sorted({Path(path).resolve() for path in paths}, key=lambda p: (len(p.parts), str(p)))
    result = []
    for path in roots:
        if not any(path == parent or parent in path.parents for parent in result):
            result.append(path)
    return result


def disk_usage(paths):
    logical = allocated = 0
    seen = set()
    for root in _parse_disk_roots(paths):
        candidates = [root] if root.is_file() else []
        if root.is_dir():
            for current, dirs, files in os.walk(root, followlinks=False):
                dirs[:] = [name for name in dirs if not (Path(current) / name).is_symlink()]
                candidates.extend(Path(current) / name for name in files)
        for path in candidates:
            try:
                stat = path.lstat()
            except OSError:
                continue
            if path.is_symlink() or (stat.st_dev, stat.st_ino) in seen:
                continue
            seen.add((stat.st_dev, stat.st_ino))
            logical += stat.st_size
            allocated += getattr(stat, 'st_blocks', 0) * 512
    return {'logical_bytes': logical, 'allocated_bytes': allocated}


def _pid_identity(pid):
    if platform.system() == 'Linux':
        try:
            return (Path('/proc') / str(pid) / 'stat').read_text().rsplit(')', 1)[1].split()[19]
        except (OSError, IndexError):
            return None
    if platform.system() == 'Darwin':
        try:
            result = subprocess.check_output(['/bin/ps', '-p', str(pid), '-o', 'lstart='],
                                            text=True, timeout=2).strip()
            return result or None
        except (OSError, subprocess.SubprocessError):
            return None
    return None


def _alive(record):
    pid = record.get('pid')
    if type(pid) is int:
        try:
            os.kill(pid, 0)
            identity = record.get('pid_identity')
            actual = _pid_identity(pid)
            if identity is None or actual is None or actual == identity:
                return True
        except PermissionError:
            return True
        except ProcessLookupError:
            pass
    pgid = record.get('pgid')
    if type(pgid) is int and _group_exists(pgid):
        identity = record.get('pgid_identity')
        actual = _pid_identity(pgid)
        return identity is None or actual is None or identity == actual
    return False


def _read_state(path):
    try:
        value = json.loads(path.read_text())
    except FileNotFoundError:
        return {'reservations': []}
    if not isinstance(value, dict) or not isinstance(value.get('reservations'), list):
        raise RuntimeError('shared reservation ledger is invalid; refusing admission')
    return value


@contextlib.contextmanager
def _ledger_lock(state_dir):
    state_dir.mkdir(parents=True, exist_ok=True)
    with (state_dir / 'admission.lock').open('a+') as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        try:
            yield state_dir / 'reservations.json'
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def _write_json(path, value):
    path = Path(path)
    temp = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    temp.write_text(json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False) + '\n')
    temp.replace(path)


def _cpu_universe():
    if hasattr(os, 'sched_getaffinity'):
        return sorted(os.sched_getaffinity(0))
    return list(range(os.cpu_count() or 1))


def _host_disk(paths):
    # Probe every distinct filesystem that can receive output.
    available = []
    for root in sorted({Path(path).resolve() for path in paths}, key=str):
        path = root if root.exists() else root.parent
        while not path.exists():
            path = path.parent
        available.append(shutil.disk_usage(path).free)
    return min(available) if available else 0


def _host_pressure(total, available, task_disk):
    if total - available >= HOST_MEMORY_STOP:
        return 'host_memory_pressure'
    pressure_floor = max(512_000_000, min(1_000_000_000, total // 20))
    if available < pressure_floor:
        return 'host_memory_pressure'
    if task_disk >= HOST_DISK_STOP:
        return 'task_artifact_disk_pressure'
    return None


def _admit(args, state_dir, roots, initial_disk):
    if not (0 < args.cpu_slots <= CPU_CAP and math.isfinite(args.memory_gb)
            and math.isfinite(args.disk_gb) and math.isfinite(args.seconds)
            and 0 < args.memory_gb * GB < MEMORY_CAP and 0 < args.disk_gb * GB < DISK_CAP
            and 0 < args.seconds <= MAX_SECONDS):
        raise ValueError('limits must be positive and below the shared 8 CPU / 40 GB / 80 GB reservations')
    if process_group_rss(os.getpgrp()) is None:
        raise RuntimeError('process-tree RSS probe is unavailable; refusing an unmonitored run')
    cpus = _cpu_universe()
    token = uuid.uuid4().hex
    with _ledger_lock(state_dir) as ledger_path:
        state = _read_state(ledger_path)
        active = [entry for entry in state['reservations'] if _alive(entry)]
        total, available = host_memory()
        headroom = max(512_000_000, min(2_000_000_000, total // 20))
        if available < args.memory_gb * GB + headroom:
            raise RuntimeError('host memory headroom is below this reservation plus safety margin')
        free_disk = _host_disk(roots)
        disk_margin = max(64_000_000, min(1_000_000_000, int(args.disk_gb * GB / 10)))
        if free_disk < args.disk_gb * GB + disk_margin:
            raise RuntimeError('host disk headroom is below this reservation plus safety margin')
        load = os.getloadavg()[0] if hasattr(os, 'getloadavg') else 0
        load_available = max(0, len(cpus) - math.ceil(load))
        if args.cpu_slots > load_available:
            raise RuntimeError('host CPU headroom is below this reservation')
        used_cpus = {cpu for entry in active for cpu in entry.get('cpus', [])}
        cpu_ids = [cpu for cpu in cpus if cpu not in used_cpus][:args.cpu_slots]
        cpu_total = sum(entry['cpu_slots'] for entry in active) + args.cpu_slots
        memory_total = sum(entry['memory_bytes'] for entry in active) + int(args.memory_gb * GB)
        disk_total = sum(entry['disk_bytes'] for entry in active) + int(args.disk_gb * GB)
        if cpu_total > CPU_CAP or memory_total >= MEMORY_CAP or disk_total >= DISK_CAP:
            raise RuntimeError('shared task reservations would exceed the CPU, memory, or disk budget')
        if len(cpu_ids) != args.cpu_slots:
            raise RuntimeError('not enough unreserved host CPU slots')
        current_disk = disk_usage(roots)
        pressure = _host_pressure(total, available, current_disk['logical_bytes'])
        if pressure:
            raise RuntimeError(pressure)
        if current_disk['logical_bytes'] + disk_total >= DISK_CAP:
            raise RuntimeError('task artifacts plus active reservations would reach the 80 GB operating target')
        record = {'token': token, 'pid': os.getpid(), 'pid_identity': _pid_identity(os.getpid()),
                  'created': time.time(), 'cpu_slots': args.cpu_slots, 'cpus': cpu_ids,
                  'memory_bytes': int(args.memory_gb * GB), 'disk_bytes': int(args.disk_gb * GB)}
        _write_json(ledger_path, {'reservations': active + [record]})
    return token, cpu_ids, {'total_bytes': total, 'available_bytes': available,
                            'disk_available_bytes': free_disk, 'load_1m': load,
                            'cpu_slots_available': load_available}


def _release(state_dir, token):
    with _ledger_lock(state_dir) as ledger_path:
        state = _read_state(ledger_path)
        remaining = []
        for entry in state['reservations']:
            if entry.get('token') == token:
                if type(entry.get('pgid')) is int and _group_exists(entry['pgid']):
                    remaining.append(entry)
            elif _alive(entry):
                remaining.append(entry)
        _write_json(ledger_path, {'reservations': remaining})


def _attach_process_group(state_dir, token, pgid):
    with _ledger_lock(state_dir) as ledger_path:
        state = _read_state(ledger_path)
        for entry in state['reservations']:
            if entry.get('token') == token:
                entry['pgid'] = pgid
                entry['pgid_identity'] = _pid_identity(pgid)
                break
        _write_json(ledger_path, state)


class _BoundedLog:
    def __init__(self, pipe, path, limit):
        self.pipe, self.path, self.limit = pipe, path, limit
        self.seen = 0

    def drain(self):
        kept = 0
        with self.path.open('wb') as output:
            while True:
                block = self.pipe.read(65536)
                if not block:
                    break
                self.seen += len(block)
                remaining = self.limit - kept
                if remaining > 0:
                    output.write(block[:remaining])
                    kept += min(remaining, len(block))
            if self.seen > kept:
                pass


def _group_exists(pgid):
    try:
        os.killpg(pgid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def _stop_group(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    deadline = time.monotonic() + TERM_GRACE_SECONDS
    while time.monotonic() < deadline:
        process.poll()
        if not _group_exists(process.pid):
            return
        time.sleep(0.02)
    process.poll()
    if not _group_exists(process.pid):
        return
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=1)
    except subprocess.TimeoutExpired:
        pass


def _child_affinity(cpus):
    if platform.system() == 'Linux' and hasattr(os, 'sched_setaffinity'):
        os.sched_setaffinity(0, set(cpus))


def _run(args, out, state_dir, roots, token, cpu_ids, host):
    env = os.environ.copy()
    for name in ('OMP_NUM_THREADS', 'OPENBLAS_NUM_THREADS', 'MKL_NUM_THREADS',
                 'NUMEXPR_NUM_THREADS', 'VECLIB_MAXIMUM_THREADS', 'BLIS_NUM_THREADS',
                 'RAYON_NUM_THREADS', 'RUST_TEST_THREADS', 'CMAKE_BUILD_PARALLEL_LEVEL',
                 'CARGO_BUILD_JOBS', 'HARMONY_QUICKNES_BUILD_JOBS'):
        env[name] = str(args.cpu_slots)
    env['HARMONY_CPU_SLOTS'] = str(args.cpu_slots)
    out.mkdir(parents=True, exist_ok=False)
    stdout = _BoundedLog(None, out / 'stdout.log', LOG_CAP)
    stderr = _BoundedLog(None, out / 'stderr.log', LOG_CAP)
    telemetry_path = out / 'resources.jsonl'
    with telemetry_path.open('w') as telemetry:
        initial_disk = disk_usage(roots)
        started = time.monotonic()
        status = 'complete'
        reason = None
        peak_rss = None
        peak_disk = initial_disk
        host_peak = None
        sample_count = 0
        telemetry_bytes = 0
        telemetry_truncated = False
        deadline = started + args.seconds
        previous_sigterm = signal.getsignal(signal.SIGTERM)
        cancellation = [False]

        def request_cleanup(_signum, _frame):
            # Do not raise from a signal handler: Popen may already have made
            # the child, but not yet returned its Process object to this frame.
            # A flag also makes repeated SIGTERM harmless during group cleanup.
            cancellation[0] = True

        def child_limits():
            if platform.system() == 'Linux' and hasattr(os, 'sched_setaffinity'):
                _child_affinity(cpu_ids)
            resource.setrlimit(resource.RLIMIT_FSIZE, (256 * 1024 * 1024, 256 * 1024 * 1024))
            if hasattr(resource, 'RLIMIT_CORE'):
                resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        signal.signal(signal.SIGTERM, request_cleanup)
        process = None
        log_threads = []
        try:
            process = subprocess.Popen(args.command, cwd=args.cwd, env=env, stdin=subprocess.DEVNULL,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
                                       preexec_fn=child_limits)
            _attach_process_group(state_dir, token, process.pid)
            stdout.pipe, stderr.pipe = process.stdout, process.stderr
            for log in (stdout, stderr):
                thread = threading.Thread(target=log.drain, daemon=True)
                log_threads.append(thread)
                thread.start()

            while True:
                if cancellation[0]:
                    status, reason = 'incomplete', 'supervisor_cancelled'
                    _stop_group(process)
                    break
                elapsed = time.monotonic() - started
                current_rss = process_group_rss(process.pid)
                current_disk = disk_usage(roots)
                try:
                    total, available = host_memory()
                    host_peak = max(host_peak or 0, total - available)
                except (OSError, RuntimeError, subprocess.SubprocessError, ValueError):
                    total = available = None
                peak_disk = {key: max(peak_disk[key], current_disk[key]) for key in peak_disk}
                if current_rss is not None:
                    peak_rss = max(peak_rss or 0, current_rss)
                sample = {'elapsed_seconds': elapsed, 'process_group_rss_bytes_sampled': current_rss,
                          'task_artifact_disk': current_disk,
                          'host_memory_used_bytes_sampled': None if total is None else total - available}
                line = json.dumps(sample, separators=(',', ':'), allow_nan=False) + '\n'
                encoded = line.encode()
                if telemetry_bytes + len(encoded) <= TELEMETRY_CAP:
                    telemetry.write(line)
                    telemetry.flush()
                    telemetry_bytes += len(encoded)
                elif not telemetry_truncated:
                    marker = '{"truncated":true}\n'
                    if telemetry_bytes + len(marker.encode()) <= TELEMETRY_CAP:
                        telemetry.write(marker)
                        telemetry.flush()
                        telemetry_bytes += len(marker.encode())
                    telemetry_truncated = True
                sample_count += 1

                if current_rss is None:
                    status, reason = 'incomplete', 'process_tree_rss_unavailable'
                elif current_rss > int(args.memory_gb * GB):
                    status, reason = 'incomplete', 'sampled_process_tree_memory_limit'
                elif current_disk['logical_bytes'] - initial_disk['logical_bytes'] > int(args.disk_gb * GB):
                    status, reason = 'incomplete', 'sampled_task_artifact_disk_limit'
                elif current_disk['logical_bytes'] >= HOST_DISK_STOP:
                    status, reason = 'incomplete', 'task_artifact_disk_pressure'
                elif total is not None and _host_pressure(total, available, current_disk['logical_bytes']):
                    status, reason = 'incomplete', 'host_memory_pressure'
                elif time.monotonic() >= deadline:
                    status, reason = 'incomplete', 'wall_timeout'
                if reason:
                    _stop_group(process)
                    break
                return_code = process.poll()
                if return_code is not None:
                    if return_code == -getattr(signal, 'SIGXFSZ', 25):
                        status, reason = 'incomplete', 'per_file_size_limit'
                    if _group_exists(process.pid):
                        status, reason = 'incomplete', reason or 'orphan_processes_cleaned'
                        _stop_group(process)
                    break
                time.sleep(SAMPLE_SECONDS)
            # A signal can arrive while a resource probe or process poll is in
            # progress. Preserve the cancellation outcome even if the command
            # exits during that sample.
            if cancellation[0]:
                status, reason = 'incomplete', 'supervisor_cancelled'
                if process is not None and _group_exists(process.pid):
                    _stop_group(process)
        except BaseException:
            if process is not None:
                _stop_group(process)
            raise
        finally:
            signal.signal(signal.SIGTERM, previous_sigterm)
            for thread in log_threads:
                if thread.ident is not None:
                    thread.join(timeout=2)
            if process is not None:
                process.stdout.close()
                process.stderr.close()
        if process is None:
            raise RuntimeError('command process was not created')
        return_code = process.poll()
        final_disk = disk_usage(roots)
        peak_disk = {key: max(peak_disk[key], final_disk[key]) for key in peak_disk}
    summary = {'format': 'harmony-tiny-world-supervisor-v1', 'status': status,
               'incomplete_reason': reason, 'exit_code': return_code,
               'elapsed_seconds': time.monotonic() - started,
               'supervisor_elapsed_seconds': time.monotonic() - args.supervisor_started,
               'reservation': {'cpu_slots': args.cpu_slots, 'memory_gb': args.memory_gb,
                               'disk_gb': args.disk_gb, 'cpu_ids': cpu_ids},
               'host_at_admission': host,
               'cpu_enforcement': 'linux_process_affinity_and_thread_caps' if platform.system() == 'Linux'
                                  and hasattr(os, 'sched_setaffinity') else 'reservation_and_thread_caps_only',
               'rss_enforcement': 'sampled_process_group_rss; not a hard limit',
               'process_model': 'trusted foreground command in one POSIX session; daemonization and setsid escapes are unsupported',
               'peak_process_group_rss_bytes_sampled': peak_rss,
               'host_peak_memory_used_bytes_sampled': host_peak,
               'task_artifact_disk_baseline': initial_disk,
               'peak_task_artifact_disk_sampled': peak_disk,
               'final_task_artifact_disk': final_disk,
               'resource_samples': sample_count,
               'logs': {'stdout_bytes_seen': stdout.seen, 'stderr_bytes_seen': stderr.seen,
                        'per_file_cap_bytes': LOG_CAP,
                        'stdout_truncated': stdout.seen > LOG_CAP,
                        'stderr_truncated': stderr.seen > LOG_CAP},
               'telemetry_truncated': telemetry_truncated}
    _write_json(out / 'summary.json', summary)
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--artifact-root', action='append', type=Path, required=True,
                        help='task-owned output/build directory to include in disk samples; repeat as needed')
    parser.add_argument('--cpu-slots', required=True, type=int)
    parser.add_argument('--memory-gb', required=True, type=float)
    parser.add_argument('--disk-gb', required=True, type=float)
    parser.add_argument('--seconds', required=True, type=float)
    parser.add_argument('--cwd', type=Path)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.command and args.command[0] == '--':
        args.command = args.command[1:]
    if not args.command:
        parser.error('provide a command after --')
    args.supervisor_started = time.monotonic()
    args.out = args.out.resolve()
    args.cwd = args.cwd.resolve() if args.cwd else None
    artifact_roots = [path.resolve() for path in args.artifact_root]
    if any(not root.is_dir() for root in artifact_roots):
        parser.error('every --artifact-root must already be a directory')
    if args.out.exists():
        parser.error('--out must name a new directory')
    args.out.parent.mkdir(parents=True, exist_ok=True)
    state_dir = args.out.parent / '.tiny-worlds-supervisor'
    state_dir.mkdir(parents=True, exist_ok=True)
    args.out.mkdir()
    roots = _parse_disk_roots([args.out, state_dir, *artifact_roots])
    # Create bounded files before establishing the monitored task-artifact baseline.
    # _run requires a fresh output path, so remove this empty directory and let it own creation.
    args.out.rmdir()
    try:
        initial_disk = disk_usage(roots)
        token, cpu_ids, host = _admit(args, state_dir, roots, initial_disk)
    except (OSError, RuntimeError, ValueError) as error:
        args.out.mkdir(parents=True, exist_ok=True)
        _write_json(args.out / 'summary.json', {'format': 'harmony-tiny-world-supervisor-v1',
                                                'status': 'not_admitted', 'reason': str(error),
                                                'incomplete': True})
        print(json.dumps({'status': 'not_admitted', 'reason': str(error)}), file=sys.stderr)
        return 2
    try:
        summary = _run(args, args.out, state_dir, roots, token, cpu_ids, host)
    except KeyboardInterrupt:
        _write_json(args.out / 'summary.json', {'format': 'harmony-tiny-world-supervisor-v1',
                                                'status': 'incomplete', 'reason': 'interrupted',
                                                'incomplete': True})
        return 130
    except Exception as error:
        _write_json(args.out / 'summary.json', {'format': 'harmony-tiny-world-supervisor-v1',
                                                'status': 'infrastructure_error', 'reason': str(error),
                                                'incomplete': True})
        print(json.dumps({'status': 'infrastructure_error', 'reason': str(error)}), file=sys.stderr)
        return 1
    finally:
        _release(state_dir, token)
    print(json.dumps({'status': summary['status'], 'incomplete_reason': summary['incomplete_reason'],
                      'exit_code': summary['exit_code'], 'seconds': round(summary['elapsed_seconds'], 3)}))
    if summary['status'] == 'incomplete':
        return 2
    return 0 if summary['exit_code'] == 0 else 1


if __name__ == '__main__':
    raise SystemExit(main())
