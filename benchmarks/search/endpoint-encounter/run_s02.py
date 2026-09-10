#!/usr/bin/env python3
"""Run one registered conditional diagnostic after identity/resource checks."""
import argparse
from datetime import datetime, timezone
import hashlib
import gzip
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
a = p.parse_args()
r = json.loads((a.protocol / 's02-registration.json').read_text())

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

for name, expected in r['protocol_sha256'].items():
    assert sha(a.protocol / name) == expected, name
request_path = a.protocol / 's02-request.json'
request = json.loads(request_path.read_text())
assert request['trial_seeds'] == r['trial_seeds']
assert request['total_frame_limit'] == r['auxiliary_frame_ceiling']
assert request['actions'] == r['actions'] and request['frames_per_arm'] == r['frames_per_arm']
assert request['expected_suffix_sha256'] == r['suffix_sha256']
assert sha(Path(request['input'])) == request['input_sha256']
previous = json.loads(gzip.decompress((a.protocol / 's01-summary.json.gz').read_bytes()))
witness_bytes = gzip.decompress((a.protocol / 's01-damage-witness.json.gz').read_bytes())
entry = next(w for w in previous['witnesses'] if w['file'] == 'ordinary-surviving-damage.json')
assert entry['verified_held_replays'] == 2
assert hashlib.sha256(witness_bytes).hexdigest() == entry['sha256']
witness = json.loads(witness_bytes)
assert json.loads(Path(request['input']).read_text()) == witness['input']
assert request['expected_endpoint'] == witness['endpoint']
assert request['expected_context'] == witness['context']
assert request['expected_emulator_sha256'] == witness['emulator_sha256']
assert request['trial_seeds'] == [row['seed'] for row in previous['trials'] if row['arm'] == 'ordinary']
assert request['expected_suffix_sha256'] == r['unchanged_s01_suffix_sha256']
binary = a.root / r['binary']
assert sha(binary) == r['binary_sha256']
assert sha(binary.with_name('build-info.json')) == r['build_info_sha256']
assets = json.loads((a.root / 'assets.json').read_text())
core, rom = (Path(assets[k]['path']) for k in ('core', 'metroid'))
assert sha(core) == request['core_sha256'] and sha(rom) == request['rom_sha256']
assert (datetime.fromisoformat(r['deadline_utc']) - datetime.now(timezone.utc)).total_seconds() >= r['max_wall_seconds']
output = a.root / r['output']
assert not output.exists()

def child_limits():
    resource.setrlimit(resource.RLIMIT_FSIZE, (r['file_limit_bytes'], r['file_limit_bytes']))

start = time.monotonic()
reason = None
with (a.protocol / 's02-native.log').open('wb') as log:
    child = subprocess.Popen(['taskset', '-c', r['cpu'], str(binary), 'run', str(core),
                              str(rom), str(request_path), str(output)],
                             stdout=log, stderr=subprocess.STDOUT,
                             start_new_session=True, preexec_fn=child_limits)
    while child.poll() is None:
        size = sum(f.stat().st_size for f in a.protocol.rglob('*') if f.is_file())
        if time.monotonic() - start > r['max_wall_seconds']:
            reason = 'wall_limit'
        elif size > r['output_limit_bytes']:
            reason = 'output_limit'
        if reason:
            os.killpg(child.pid, signal.SIGKILL)
            break
        time.sleep(1)
    code = child.wait()
usage = resource.getrusage(resource.RUSAGE_CHILDREN)
report = {'returncode': code, 'stop_reason': reason,
          'wall_seconds': time.monotonic() - start, 'maxrss_kib': usage.ru_maxrss,
          'user_seconds': usage.ru_utime, 'system_seconds': usage.ru_stime}
(a.protocol / 's02-process.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report), flush=True)
raise SystemExit(1 if reason or code else 0)
