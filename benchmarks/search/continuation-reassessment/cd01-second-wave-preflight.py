from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess

root = Path('/root/harmony-depth-transfer-20260909')
protocol = root / 'continuation-cd01-001'


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def properties(unit):
    names = ('LoadState', 'ActiveState', 'SubState', 'MainPID')
    command = ['systemctl', 'show', unit]
    for name in names:
        command += ['-p', name]
    return dict(line.split('=', 1) for line in subprocess.check_output(command, text=True).splitlines())


master = json.loads((protocol / 'cd01-registration.json').read_text())
assert sha(protocol / 'cd01-registration.json') == '68999f9437b2d43acab13d9411a80f8bd6b9361b85a0076ec92cc4551a30274d'
gate = json.loads((protocol / 'cd01-second-wave-gate.json').read_text())
score_path = protocol / 'cd01-first-wave-analysis.json'
assert sha(score_path) == gate['first_wave_analysis_sha256']
score = json.loads(score_path.read_text())
assert score['decision'] == 'continue_registered_panel'
assert score['screen']['completed_pairs'] == score['screen']['strict_wins'] == 2
assert not score['incomplete_work']
assert score['actual_admitted_search_frames'] + 800000000 <= master['nominal_search_limit'] + 8 * master['bounded_inflight_drain_per_cell']
assert score['known_auxiliary_frames'] < master['known_auxiliary_limit']
assert platform.node() == 'ms02'
now = datetime.now(timezone.utc)
remaining = (datetime.fromisoformat(master['deadline_utc']) - now).total_seconds()
assert remaining >= 5960, 'Complete remaining service bounds plus dispatch margin do not fit'
terminal = {}
for pair in (0, 1):
    name = f'cd01-pair-{pair}'
    state = properties(f'harmony-continuation-{name}-20260910.service')
    assert state['ActiveState'] == 'inactive' and state['MainPID'] == '0'
    result_path = root / f'runs/continuation-cd01/{name}/results.json'
    assert sha(result_path) == gate['panels'][name]['results_sha256']
    result = json.loads(result_path.read_text())
    assert result['execution_complete'] and result['allocation_stop'] is None
    terminal[name] = state

assets = json.loads((root / 'assets.json').read_text())
seeds, registrations = [], []
for pair in (2, 3):
    name = f'cd01-pair-{pair}'
    spec = next(p for p in master['panels'] if p['id'] == name)
    path = protocol / spec['registration']
    assert sha(path) == spec['registration_sha256']
    registration = json.loads(path.read_text())
    assert not (root / registration['output']).exists(), 'Output already exists; do not restart'
    unit = f'harmony-continuation-{name}-20260910.service'
    state = properties(unit)
    assert state['LoadState'] == 'not-found' and state['MainPID'] == '0'
    old_journal = subprocess.check_output(['journalctl', '-u', unit, '--no-pager', '-o', 'json'], text=True)
    assert not old_journal.strip(), 'Service has historical use; inspect without rerunning'
    assert sha(root / registration['build'] / 'nes-eval') == registration['binary_sha256']
    assert sha(root / registration['build'] / 'build-info.json') == registration['build_info_sha256']
    for file, field in [('run_cells.py', 'runner_sha256'), ('audit_controls.py', 'analyzer_sha256'), ('named_milestone_endpoint.py', 'named_endpoint_sha256')]:
        assert sha(protocol / file) == registration[field]
    for needed in registration['required_evidence']:
        assert sha(root / needed['path']) == needed['sha256']
    expected = registration['cells'][0]['expected_identity']
    for name, field in [('core', 'core_sha256'), ('metroid', 'rom_sha256')]:
        assert sha(Path(assets[name]['path'])) == assets[name]['sha256'] == expected[field]
    seeds.append(spec['seed'])
    registrations.append({'pair': pair, 'registration_sha256': sha(path), 'cpus': spec['cpus'], 'service_runtime_seconds': spec['service_runtime_seconds']})

scanned = 0
for path in (root / 'runs').rglob('summary.json'):
    data = json.loads(path.read_text())
    scanned += 1
    assert data.get('search_request', {}).get('seed') not in seeds, 'Remaining seed already used'
topology = []
for cpu in range(8):
    base = Path(f'/sys/devices/system/cpu/cpu{cpu}')
    topology.append({'cpu': cpu, 'package': (base / 'topology/physical_package_id').read_text().strip(), 'core': (base / 'topology/core_id').read_text().strip(), 'max_khz': int((base / 'cpufreq/cpuinfo_max_freq').read_text())})
assert len({(c['package'], c['core']) for c in topology}) == 8
assert len({c['max_khz'] for c in topology}) == 1
mem = dict(line.split(':', 1) for line in Path('/proc/meminfo').read_text().splitlines())
available_memory = int(mem['MemAvailable'].split()[0]) * 1024
available_disk = shutil.disk_usage(root).free
assert available_memory >= 24 * 1024**3 and available_disk >= 16 * 1024**3
print(json.dumps({'format': 'continuation-cd01-second-wave-preflight-v1', 'checked_utc': now.isoformat(), 'passed': True, 'host': 'ms02', 'remaining_deadline_seconds': remaining, 'gate_sha256': sha(protocol / 'cd01-second-wave-gate.json'), 'completed_services': terminal, 'remaining_seeds': seeds, 'saved_summaries_scanned': scanned, 'registrations': registrations, 'topology': topology, 'available_memory_bytes': available_memory, 'available_disk_bytes': available_disk, 'emulated_frames': 0}, indent=2))
