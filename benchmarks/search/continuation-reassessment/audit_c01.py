#!/usr/bin/env python3
"""Recover existing corrected-terminal C01 arrivals without extending its run."""
import gzip
import json
from pathlib import Path

from audit import raw, sha

HERE = Path(__file__).resolve().parent


def audit(evidence):
    published_path = HERE.parent / 'endpoint-encounter/c01-results.json.gz'
    published = json.loads(gzip.decompress(published_path.read_bytes()))
    summary_bytes = raw(evidence / 'summary.json')
    summary = json.loads(summary_bytes)
    assert summary == published['records'][0]['summary']
    assert summary['result']['stop_reason'] == 'wall_limit'
    assert summary['identity']['policies']['terminal_policy'] == 'death_or_bcd_underflow_or_ending_v3'
    first = summary['last_progress']['workload_diagnostics']['named_progress']['first_seen']
    bounds = {name: {'before': None, 'after': None} for name in first}
    progress_bytes = raw(evidence / 'progress.jsonl')
    previous = (0, 0)
    count = 0
    for line in progress_bytes.splitlines():
        record = json.loads(line)
        execution, frames = record['executions'], record['frames_emulated']
        assert execution >= previous[0] and frames >= previous[1]
        previous = execution, frames
        count += 1
        observed = record['workload_diagnostics']['named_progress']['first_seen']
        for name, event in first.items():
            assert observed[name] == (event if event and execution >= event['execution'] else None)
            if not event:
                continue
            if execution < event['execution']:
                bounds[name]['before'] = [execution, frames]
            elif bounds[name]['after'] is None:
                bounds[name]['after'] = [execution, frames]
    assert record == summary['last_progress']
    assert previous == (summary['result']['executions'], summary['result']['frames_emulated'])
    endpoints = {}
    for name, event in first.items():
        interval = None
        witness = summary['result']['milestone_witnesses'].get(name)
        if event:
            assert witness['replay']['diagnostics']['named_progress']['first_seen'][name]
            before, after = bounds[name]['before'], bounds[name]['after']
            interval = [after[1] if after[0] == event['execution'] else before[1] + 1, after[1]]
            assert interval[0] <= interval[1]
        else:
            assert witness is None
        endpoints[name] = {'first_seen': event, 'arrival_frames_interval': interval,
                           'right_censored_after_frames': previous[1] if event is None else None,
                           'bracket': bounds[name]}
    return {'format': 'corrected-c01-arrival-audit-v1', 'summary_sha256': sha(summary_bytes),
            'progress_sha256': sha(progress_bytes), 'progress_bytes': len(progress_bytes),
            'progress_records': count, 'published_record_gzip_sha256': sha(published_path.read_bytes()),
            'identity': summary['identity'], 'endpoints': endpoints, 'new_emulator_frames': 0,
            'limits': ['C01 remained incomplete at its original 35-minute wall cap; no extension or rerun.',
                       'One reused, outcome-selected control seed; no fresh calibration pass or candidate comparison.',
                       'Positive replayed milestones remain valid within the completed prefix; full 250M horizon is unobserved.',
                       'Key v10 and terminal v3 differ from historical 012; no cross-record causal effect is identified.',
                       'Checkpoint intervals describe admitted work, not within-job arrival or population uncertainty.']}


if __name__ == '__main__':
    print(json.dumps(audit(HERE / 'c01-evidence'), indent=2))
