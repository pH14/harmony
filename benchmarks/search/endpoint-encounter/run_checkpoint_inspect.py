#!/usr/bin/env python3
"""One frozen, externally bounded inspection; no retries or action execution."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import resource
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('root', type=Path)
p.add_argument('protocol', type=Path)
a = p.parse_args()
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
r = json.loads((a.protocol / 'ci01-registration.json').read_text())
assert sha(Path(__file__)) == r['launcher_sha256']
binary = a.root / r['binary']
assert sha(binary) == r['binary_sha256']
assert sha(binary.with_name('build-info.json')) == r['build_info_sha256']
request = a.protocol / 'ci01-request.json'
assert sha(request) == r['request_sha256']
q = json.loads(request.read_text())
for key in ('core', 'rom', 'checkpoint', 'origin'):
    assert sha(Path(q[key])) == q[key + '_sha256']
assert (datetime.fromisoformat(r['deadline_utc']) - datetime.now(timezone.utc)).total_seconds() >= r['wall_seconds']
output = a.protocol / 'ci01-context.json'
assert not output.exists()
resource.setrlimit(resource.RLIMIT_FSIZE, (r['file_limit_bytes'], r['file_limit_bytes']))
start = time.monotonic()
code, reason = None, None
try:
    with (a.protocol / 'ci01.log').open('wb') as log:
        code = subprocess.run(['taskset', '-c', r['cpu'], str(binary), 'inspect', str(request), str(output)],
                              stdout=log, stderr=subprocess.STDOUT, timeout=r['wall_seconds']).returncode
except subprocess.TimeoutExpired:
    reason = 'wall_limit'
u = resource.getrusage(resource.RUSAGE_CHILDREN)
report = {'returncode': code, 'stop_reason': reason, 'wall_seconds': time.monotonic() - start,
          'maxrss_kib': u.ru_maxrss, 'user_seconds': u.ru_utime, 'system_seconds': u.ru_stime}
(a.protocol / 'ci01-process.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report), flush=True)
raise SystemExit(1 if reason or code else 0)
