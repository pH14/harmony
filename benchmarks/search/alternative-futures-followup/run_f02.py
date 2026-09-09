#!/usr/bin/env python3
"""Run immutable F02 compatibility and two equal-work probes with host watchdogs."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

root = Path('/root/harmony-alternative-futures-followup-20260909')
old = Path('/root/harmony-alternative-futures-20260908')
assets = json.loads(Path('/root/harmony-search-eval/assets.json').read_text())

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

binary = root / 'builds/f02/metroid-retention-probe'
assert sha(binary) == '2d4af7703af055d39a2c0e76d7c3b25e97f96b68e819f2f3cddd15eeaa6ce2a4'
proof = {'format': 'F02-execution-v1', 'build': json.loads((binary.parent / 'build-info.json').read_text()), 'runs': []}
for label, trials, budget in [('compatibility', 64, None), ('matched-a', 4096, 100000), ('matched-b', 4096, 100000)]:
    out = root / ('f02-' + label)
    command = [str(binary), assets['core']['path'], assets['metroid']['path'], str(old / 'probes/p02-audit.json'), str(out), str(trials), '24', 'death_or_bcd_underflow_or_ending_v3']
    if budget:
        command.append(str(budget))
    started = time.monotonic()
    with (root / ('f02-' + label + '.log')).open('xb') as log:
        child = subprocess.Popen(['timeout', '--signal=TERM', '--kill-after=5', '300', 'taskset', '-c', '16', *command], stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        _, status, usage = os.wait4(child.pid, 0)
    run = {'label': label, 'exit_code': os.waitstatus_to_exitcode(status), 'wall_bound_seconds': 300, 'elapsed_seconds': time.monotonic() - started, 'cpu_seconds': usage.ru_utime + usage.ru_stime, 'max_rss_kib': usage.ru_maxrss}
    proof['runs'].append(run)
    (root / 'f02-execution-partial.json').write_text(json.dumps(proof, indent=2) + '\n')
    if run['exit_code'] != 0:
        raise RuntimeError('probe incomplete: ' + label)
    run['summary'] = json.loads((out / 'summary.json').read_text())
    run['outcomes_sha256'] = sha(out / 'outcomes.jsonl')
    run['suffixes_sha256'] = sha(out / 'suffixes.json')
    if budget is None:
        assert run['outcomes_sha256'] == 'c79df7524bbb28d0dc3ebc8a86f7028b60380469f5df03a29f4b66cd137bc945'
        assert run['suffixes_sha256'] == '4e408f2397801eb5a3946a6b7e5ebfda8954669b87f87ec65478e7a7cb94646d'
    if label == 'matched-b':
        assert run['summary'] == proof['runs'][1]['summary']
        assert run['outcomes_sha256'] == proof['runs'][1]['outcomes_sha256']
    print(json.dumps({k:v for k,v in run.items() if k!='summary'}), flush=True)
with (root / 'f02-execution.json').open('x') as f:
    json.dump(proof, f, indent=2)
    f.write('\n')
