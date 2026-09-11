import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
from datetime import datetime, timezone

root = Path('/root/harmony-depth-transfer-20260909')
inputs = root / 'progress-word-build-input-001'
source = root / 'source-progress-word-001'
build = root / 'builds/progress-word-x86-001'
target = root / 'build-cache-progress-word-001'
metadata = json.loads((inputs / 'source.json').read_text())
archive = inputs / 'source.tar.gz'
def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()
assert digest(archive) == metadata['archive_sha256']
assert not source.exists() and not build.exists() and not target.exists()
source.mkdir()
build.mkdir()
subprocess.run(['tar', '-xzf', str(archive), '-C', str(source)], check=True)
env = os.environ.copy()
env['PATH'] = '/root/.cargo/bin:' + env.get('PATH', '')
env['CARGO_TARGET_DIR'] = str(target)
command = ['cargo', 'build', '--locked', '--release', '--manifest-path',
           str(source / 'workloads/nes/Cargo.toml'), '--bin', 'metroid-archive-challenge',
           '--features', 'metroid-retention-progress', '-j', '4']
record = dict(metadata, format='metroid-progress-word-build-v1', command=command,
              started_utc=datetime.now(timezone.utc).isoformat(), source=str(source),
              target=str(target), host='ms02', emulator_executed=False,
              compile_timeout_seconds=540,
              rustc=subprocess.check_output(['rustc', '-Vv'], env=env, text=True),
              cargo=subprocess.check_output(['cargo', '--version'], env=env, text=True))
with (build / 'cargo.log').open('wb') as log:
    process = subprocess.Popen(command, cwd=source, env=env, stdout=log,
                               stderr=subprocess.STDOUT, start_new_session=True)
    try:
        status = process.wait(timeout=540)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        status = process.wait()
        record['timed_out'] = True
record['exit_code'] = status
record['ended_utc'] = datetime.now(timezone.utc).isoformat()
if status == 0:
    binary = build / 'metroid-archive-challenge'
    shutil.copy2(target / 'release/metroid-archive-challenge', binary)
    record['binary_sha256'] = digest(binary)
    record['binary_bytes'] = binary.stat().st_size
(build / 'build-info.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record), flush=True)
raise SystemExit(status != 0)
