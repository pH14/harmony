#!/usr/bin/env python3
"""Snapshot owned tranche work without double-counting nested chain stages."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('root', type=Path)
p.add_argument('output', type=Path)
a = p.parse_args()

def read(path):
    return json.loads(path.read_text())

def last_progress(path):
    # A live writer may have appended only part of its latest line.
    for line in reversed(path.read_text().splitlines()):
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue
    return {}

rows, seen = [], set()
for path in sorted((a.root / 'runs').rglob('summary.json')):
    d = read(path)
    if d.get('format') != 'harmony-search-eval-cell-v1':
        # Use structural fields as older runner revisions may name the format differently.
        if not all(k in d for k in ['cell', 'status', 'search_request']):
            continue
    seen.add(path.parent)
    result = d.get('result') or d.get('last_progress') or {}
    rows.append({'path_components': list(path.parent.relative_to(a.root).parts), 'kind': 'evaluation_cell',
                 'status': d['status'], 'executions': result.get('executions', 0),
                 'admitted_frames': result.get('frames_emulated', 0),
                 'observed_cpu_seconds': d.get('cpu_seconds', 0)})
for path in sorted((a.root / 'runs').glob('*/result.json')):
    d = read(path)
    if 'campaign' not in d:
        continue
    seen.add(path.parent)
    c = d['campaign']
    rows.append({'path_components': list(path.parent.relative_to(a.root).parts), 'kind': 'local_diagnostic',
                 'status': 'complete', 'executions': c['executions_completed'],
                 'admitted_frames': c['frames_emulated'], 'observed_cpu_seconds': 0})
for path in sorted((a.root / 'runs').rglob('progress.jsonl')):
    parent = path.parent.parent if path.parent.name == 'campaign' else path.parent
    if parent in seen:
        continue
    seen.add(parent)
    d = last_progress(path)
    rows.append({'path_components': list(parent.relative_to(a.root).parts), 'kind': 'partial_or_active',
                 'status': 'partial_or_active', 'executions': d.get('executions', 0),
                 'admitted_frames': d.get('frames_emulated', 0), 'observed_cpu_seconds': 0})
resources = []
for path in sorted(a.root.glob('*-resources.json')):
    d = read(path)
    resources.append({'file': path.name, 'exit_code': d.get('exit_code'),
                      'observed_cpu_seconds': d.get('user_seconds', 0) + d.get('system_seconds', 0)})
probes = []
for path in sorted((a.root / 'probes').glob('*/summary.json')):
    d = read(path)
    if 'probe_frames_discarded_survivor' in d:
        probes.append({'path_components': list(path.parent.relative_to(a.root).parts),
                       'physical_frames': sum(d['probe_frames_discarded_survivor']) + d['prefix_and_gain_export_frames']})
value = {'format': 'alternative-futures-accounting-snapshot-v1',
         'time_utc': datetime.now(timezone.utc).isoformat(),
         'limits': ['Admitted search work counts each evaluation cell once, including nested chain stages.',
                    'Local diagnostic search work is separate from source and witness replay.',
                    'CPU is a measured lower bound: completed evaluation cells plus separately logged diagnostic processes.',
                    'Active-process CPU, compilation/tests, and unlogged external prefix/bridge work are excluded.',
                    'Paired-probe physical work is reported separately, not added to admitted campaign work.'],
         'admitted_search_frames': sum(x['admitted_frames'] for x in rows),
         'search_executions': sum(x['executions'] for x in rows),
         'observed_cpu_seconds_lower_bound': sum(x['observed_cpu_seconds'] for x in rows + resources),
         'paired_probe_physical_frames': sum(x['physical_frames'] for x in probes),
         'rows': rows, 'diagnostic_process_resources': resources, 'paired_probes': probes}
if a.output.exists():
    raise FileExistsError('accounting snapshots are immutable')
a.output.write_text(json.dumps(value, indent=2) + '\n')
print(json.dumps({k: v for k, v in value.items() if k not in ['rows', 'diagnostic_process_resources', 'paired_probes']}))
