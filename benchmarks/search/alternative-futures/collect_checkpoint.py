#!/usr/bin/env python3
"""Export compact, hashed evidence without copying ROMs, checkpoints or input tapes."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('root', type=Path)
p.add_argument('output', type=Path)
p.add_argument('runs', nargs='+')
a = p.parse_args()


def read(path):
    return json.loads(path.read_text())


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def pick(value, names):
    return {k: value[k] for k in names.split() if k in value}


cells, diagnostics, chains = [], [], []
for name in a.runs:
    directory = a.root / 'runs' / name
    if not directory.is_dir():
        raise ValueError('missing run: ' + name)
    for path in sorted(directory.rglob('summary.json')):
        s = read(path)
        result = s.get('result') or {}
        row = {'run': name, 'cell': s['cell'], 'summary_sha256': digest(path),
               'status': s['status'], 'build': s.get('build'),
               'request': s.get('search_request'), 'identity': s.get('identity'),
               'work': pick(result, 'executions frames_emulated stream_sha256 stop_reason search_seconds verification_seconds solved'),
               'resources': pick(s, 'elapsed_seconds cpu_seconds cpu_set peak_process_tree_rss_bytes_sampled peak_disk_logical_bytes_sampled'),
               'evidence': pick(result, 'witness milestone_witnesses verified_replays'),
               'last_progress': pick(s.get('last_progress') or {}, 'executions frames_emulated retention_diagnostics retained_diagnostics workload_diagnostics continuation_diagnostics resident_memory_bytes resident_snapshot_bytes history_memory_bytes draw_state_memory_bytes'),
               'failure': pick(s, 'exit_code error timeout')}
        progress = path.parent / 'campaign' / 'progress.jsonl'
        if progress.exists():
            points = [json.loads(line) for line in progress.read_text().splitlines()]
            row['common_frame_checkpoints'] = []
            for boundary in [8_000_000, 50_000_000, 100_000_000, 200_000_000, 300_000_000, 380_000_000]:
                candidates = [v for v in points if v['frames_emulated'] <= boundary]
                if candidates:
                    v = candidates[-1]
                    row['common_frame_checkpoints'].append({'boundary': boundary, **pick(v, 'executions frames_emulated workload_diagnostics retention_diagnostics')})
        cells.append(row)
    chain = directory / 'chain.json'
    if chain.exists():
        value = read(chain)
        # Chain result objects contain mechanical proofs and costs, not controller tapes.
        chains.append({'run': name, 'sha256': digest(chain), 'chain': value})
    result = directory / 'result.json'
    if result.exists():
        d = read(result)
        row = {'run': name, 'result_sha256': digest(result)}
        identity = directory / 'identity.json'
        if identity.exists():
            row['identity'] = read(identity)
        resource = a.root / (name + '-resources.json')
        if resource.exists():
            row['resources'] = read(resource)
        if 'campaign' in d:
            c = d['campaign']
            row['work'] = pick(c, 'executions_completed frames_emulated stream_sha256 terminal_policy suffix_policy parent_scheduler mixture_policy memory_budget_mib action_limit resident_memory_bytes resident_snapshot_bytes history_memory_bytes')
            row['archive'] = pick(c['archive'], 'executions retained rejected deaths selector_accounting')
            row['evidence'] = pick(d, 'source_physical_frames composed_verification_physical_frames search_seconds elapsed_seconds witness milestone_witnesses verified_replays full_campaign_replay cost_limitations')
            progress = directory / 'progress.jsonl'
            if progress.exists():
                row['last_progress'] = pick(json.loads(progress.read_text().splitlines()[-1]), 'executions frames_emulated workload_diagnostics retention_diagnostics retained_diagnostics')
        else:
            row['raw_replay'] = {k: v for k, v in d.items() if k not in ['mask_followup', 'idle_followup']}
            for key in ['mask_followup', 'idle_followup']:
                values = d.get(key, [])
                row[key + '_summary'] = {'observations': len(values), 'health_counts': dict(Counter(v['state']['health'] for v in values)), 'pose_counts': dict(Counter(v['state']['pose'] for v in values))}
        diagnostics.append(row)
output = {'format': 'alternative-futures-checkpoint-v1', 'scope': 'explicit registered development and qualification runs; not untouched validation', 'remote_host': 'ms02', 'runs': a.runs, 'cells': cells, 'diagnostics': diagnostics, 'chains': chains}
if a.output.exists():
    raise FileExistsError('checkpoint outputs are immutable')
a.output.write_text(json.dumps(output, indent=2) + '\n')
print(json.dumps({'output': str(a.output), 'bytes': a.output.stat().st_size, 'cells': len(cells), 'diagnostics': len(diagnostics), 'chains': len(chains)}))
