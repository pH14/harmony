#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Measure the three-member etcd case in a local comparison image."""

import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import time
import urllib.request

root = Path(os.environ.get('HARMONY_GO_EVIDENCE', '/evidence'))
clock = os.sysconf('SC_CLK_TCK')

def cpu(proc):
    text = Path(f'/proc/{proc.pid}/stat').read_text().rsplit(')', 1)[1].split()
    return (int(text[11]) + int(text[12])) / clock

def callbacks(control):
    control.sendall(struct.pack('<5Q', 4, 0, 0, 0, 0))
    data = b''
    while len(data) != 40:
        chunk = control.recv(40 - len(data))
        if not chunk:
            raise RuntimeError('etcd closed the event control channel')
        data += chunk
    response = struct.unpack('<5Q', data)
    assert response[0] == 4
    return response[3]

def sample(mode):
    directory = root / mode
    directory.mkdir(exist_ok=True)
    shutil.rmtree('/tmp/etcd', ignore_errors=True)
    subprocess.run(['/opt/harmony/setup.sh'], check=True)
    procs, controls, logs = [], [], []
    writer = None
    binary = '/opt/etcd/etcd' if mode == 'instrumented' else '/opt/etcd/etcd-plain'
    server_digest = hashlib.sha256(Path(binary).read_bytes()).hexdigest()
    runtime_digest = hashlib.sha256(Path('/usr/lib/libvoidstar.so').read_bytes()).hexdigest()
    try:
        for index in range(3):
            env = os.environ.copy()
            descriptors = ()
            if mode == 'instrumented':
                parent, child = socket.socketpair()
                report, report_child = socket.socketpair()
                env['HARMONY_EVENT_KILL_FD'] = str(child.fileno())
                env['HARMONY_EVENT_REPORT_FD'] = str(report_child.fileno())
                descriptors = (child.fileno(), report_child.fileno())
            name = f'member-{index + 1}'
            client, peer = 2379 + index * 2, 2380 + index * 2
            log = (directory / f'{name}.log').open('wb')
            logs.append(log)
            proc = subprocess.Popen([binary, f'--name={name}', f'--data-dir=/tmp/etcd/data/{name}',
                f'--listen-client-urls=http://127.0.0.1:{client}',
                f'--advertise-client-urls=http://127.0.0.1:{client}',
                f'--listen-peer-urls=http://127.0.0.1:{peer}',
                f'--initial-advertise-peer-urls=http://127.0.0.1:{peer}',
                '--initial-cluster=member-1=http://127.0.0.1:2380,member-2=http://127.0.0.1:2382,member-3=http://127.0.0.1:2384',
                '--initial-cluster-state=new', '--initial-cluster-token=harmony-etcd-v35', '--enable-pprof'],
                env=env, pass_fds=descriptors, stdout=log, stderr=log)
            procs.append(proc)
            if mode == 'instrumented':
                child.close()
                report_child.close()
                report.settimeout(30)
                data = b''
                while len(data) != 16:
                    chunk = report.recv(16-len(data))
                    if not chunk:
                        raise RuntimeError('etcd exited before event runtime readiness')
                    data += chunk
                assert struct.unpack('<2Q', data) == (0x4841524d4f4e5945, 5)
                report.close()
                parent.settimeout(30)
                controls.append(parent)
        deadline = time.monotonic() + 45
        while subprocess.run(['/opt/harmony/ready.sh'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            assert time.monotonic() < deadline
            time.sleep(.1)
        writer = subprocess.Popen(['/opt/harmony/etcd-writer'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(2)
        count_before = sum(callbacks(c) for c in controls)
        cpu_before = sum(cpu(p) for p in procs)
        rows_before = len(Path('/tmp/etcd/journal/acked').read_text().splitlines())
        start = time.monotonic()
        def profile(index):
            url = f'http://127.0.0.1:{2379 + index*2}/debug/pprof/profile?seconds=20'
            data = urllib.request.urlopen(url, timeout=30).read()
            (directory / f'{index}.pprof').write_bytes(data)
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            list(pool.map(profile, range(3)))
        elapsed = time.monotonic() - start
        count = sum(callbacks(c) for c in controls) - count_before
        cpu_elapsed = sum(cpu(p) for p in procs) - cpu_before
        rows = len(Path('/tmp/etcd/journal/acked').read_text().splitlines()) - rows_before
        result = {'mode':mode, 'seconds':elapsed, 'server_cpu_seconds':cpu_elapsed,
            'server_path':binary, 'server_sha256':server_digest, 'runtime_sha256':runtime_digest,
            'callbacks':count, 'acknowledged_puts':rows, 'puts_per_second':rows/elapsed,
            'server_cpu_ns_per_put':cpu_elapsed*1e9/rows,
            'server_cpu_ns_per_callback':cpu_elapsed*1e9/count if count else None}
        (directory / 'summary.json').write_text(json.dumps(result, indent=2)+'\n')
        print(json.dumps(result), flush=True)
    finally:
        if writer:
            writer.terminate()
            writer.wait(timeout=10)
        for p in procs: p.terminate()
        for p in procs:
            try: p.wait(timeout=10)
            except subprocess.TimeoutExpired: p.kill(); p.wait()
        for c in controls: c.close()
        for log in logs: log.close()

if __name__ == '__main__':
    for mode in ('plain', 'instrumented'):
        sample(mode)
