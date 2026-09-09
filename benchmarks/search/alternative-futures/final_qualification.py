"""Run registered integrated-build gates after the frozen validation releases ms02."""
from pathlib import Path
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import time

root = Path('/root/harmony-alternative-futures-20260908')
spec = importlib.util.spec_from_file_location('bounded_processes', root / 'validation_panel.py')
watch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(watch)
build = root / 'builds/integrated-036'
assert watch.sha(build / 'nes-eval') == 'e5c363cc1fd6ae9ef6a8be44688c5e3daa512324581f5c9061849709fa51091b'
assert json.loads((build / 'build-info.json').read_text())['source_tree_sha256'] == 'cd549ed7bdbd6b3b1c8a22b52c8c7e10d3f060851a2f2ffaade7a9792d380447'
release_deadline = datetime.fromisoformat('2026-09-09T06:52:00+00:00').timestamp()
while not (root / 'validation/t04-terminal-validation/finished.json').exists():
    if time.time() >= release_deadline:
        raise RuntimeError('validation did not release the host before the registered handover; inspect, do not run competing gates')
    time.sleep(10)
# Do not compete with surviving or unrelated native campaigns for the sole-host reference.
for path in Path('/proc').glob('[0-9]*/cmdline'):
    try:
        args = path.read_bytes().split(b'\0')
    except FileNotFoundError:
        continue
    if args and args[0].endswith(b'/nes-eval'):
        raise RuntimeError('a native campaign remains active: ' + path.parent.name)

proof = {'format': 'alternative-futures-q36-gates-v1',
         'started_utc': datetime.now(timezone.utc).isoformat(),
         'build': json.loads((build / 'build-info.json').read_text()), 'gates': []}
# All settings and references are frozen existing fixtures. Only labels and evaluator change.
for old_label, label, cpus, capacity, finish, bound in [
    ('t02b-compat-legacy-s3', 'q36-metroid-compat', 4, 10240, 60, 300),
    ('m01-qualification-control', 'q36-mm2-compat', 4, 10240, 60, 300),
    ('g01-companion-control', 'q36-companions', 2, 2048, 60, 300),
    ('g01-smb', 'q36-smb', 24, 4096, 120, 750),
]:
    manifest = json.loads((root / 'manifests/alternative-futures' / (old_label + '.json')).read_text())
    manifest['id'] = label
    path = root / 'manifests/alternative-futures' / (label + '.json')
    with path.open('x') as stream:
        stream.write(json.dumps(manifest, indent=2) + '\n')
    command = ['taskset', '-c', f'0-{cpus - 1}', 'python3', str(root / 'benchmarks/search/eval.py'),
               'run', str(path), '--assets', '/root/harmony-search-eval/assets.json',
               '--binary', str(build / 'nes-eval'), '--build-info', str(build / 'build-info.json'),
               '--out', str(root / 'runs' / label), '--jobs', '1', '--cpus', str(cpus),
               '--memory-capacity-mib', str(capacity), '--finish-seconds', str(finish), '--disk-limit-gib', '4']
    execution = watch.bounded(command, root / (label + '.log'), bound)
    gate = {'label': label, 'reference': old_label, 'execution': execution, 'cells': []}
    proof['gates'].append(gate)
    (root / 'q36-gates-progress.json').write_text(json.dumps(proof, indent=2) + '\n')
    assert execution['exit_code'] == 0 and not execution['watchdog_killed_process_groups'], gate
    for summary_path in sorted((root / 'runs' / label).glob('*/summary.json')):
        new = json.loads(summary_path.read_text())
        old_path = root / 'runs' / old_label / summary_path.parent.name / 'summary.json'
        old = json.loads(old_path.read_text())
        fields = ['executions', 'frames_emulated', 'stream_sha256', 'solved']
        assert new['status'] == old['status'] == 'complete'
        assert all(new['result'][key] == old['result'][key] for key in fields), (label, summary_path.parent.name)
        gate['cells'].append({'cell': new['cell'], 'summary_sha256': watch.sha(summary_path),
                              'reference_summary_sha256': watch.sha(old_path),
                              'exact_match': {key: new['result'][key] for key in fields}})
    assert len(gate['cells']) == len(manifest['cases'])
    (root / 'q36-gates-progress.json').write_text(json.dumps(proof, indent=2) + '\n')
    print(json.dumps(gate), flush=True)
proof['finished_utc'] = datetime.now(timezone.utc).isoformat()
with (root / 'q36-gates.json').open('x') as stream:
    stream.write(json.dumps(proof, indent=2) + '\n')
print('all registered integrated-build gates passed', flush=True)
