#!/usr/bin/env python3
"""Dispatch exactly one identity-pinned, externally bounded challenge cell."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--root', type=Path, required=True)
p.add_argument('--protocol', type=Path, required=True)
p.add_argument('--registration', required=True)
p.add_argument('--cell', required=True)
a = p.parse_args()
r = json.loads((a.protocol / a.registration).read_text())
c = next(c for c in r['cells'] if c['id'] == a.cell)
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
assert sha(a.protocol / 'run_archive_challenge.py') == r['launcher_sha256']
binary = a.root / r['binary']
assert sha(binary) == r['binary_sha256']
assert sha(binary.with_name('build-info.json')) == r['build_info_sha256']
request = a.protocol / c['request']
assert sha(request) == c['request_sha256']
q = json.loads(request.read_text())
assert q['input_sha256'] == r['input_sha256'] and sha(Path(q['input'])) == r['input_sha256']
assert q['core_sha256'] == r['core_sha256'] and sha(Path(q['core'])) == r['core_sha256']
assert q['rom_sha256'] == r['rom_sha256'] and sha(Path(q['rom'])) == r['rom_sha256']
assert (datetime.fromisoformat(r['deadline_utc']) - datetime.now(timezone.utc)).total_seconds() >= c['max_wall_seconds']
output = a.root / c['output']
assert not output.exists()

def child_limits():
    resource.setrlimit(resource.RLIMIT_FSIZE, (c['file_limit_bytes'], c['file_limit_bytes']))

start = time.monotonic()
reason = None
with (a.protocol / f'{a.cell}.log').open('wb') as log:
    child = subprocess.Popen(['taskset', '-c', c['cpu'], str(binary), c['mode'], str(request), str(output)],
                             stdout=log, stderr=subprocess.STDOUT,
                             start_new_session=True, preexec_fn=child_limits)
    while child.poll() is None:
        size = sum(f.stat().st_size for f in output.rglob('*') if f.is_file()) if output.exists() else 0
        if time.monotonic() - start > c['max_wall_seconds']:
            reason = 'wall_limit'
        elif size > c['output_limit_bytes']:
            reason = 'output_limit'
        if reason:
            os.killpg(child.pid, signal.SIGKILL)
            break
        time.sleep(1)
    code = child.wait()
usage = resource.getrusage(resource.RUSAGE_CHILDREN)
result = {'cell': a.cell, 'returncode': code, 'stop_reason': reason,
          'wall_seconds': time.monotonic() - start, 'maxrss_kib': usage.ru_maxrss,
          'user_seconds': usage.ru_utime, 'system_seconds': usage.ru_stime}
(a.protocol / f'{a.cell}-process.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result), flush=True)
raise SystemExit(1 if reason or code else 0)
