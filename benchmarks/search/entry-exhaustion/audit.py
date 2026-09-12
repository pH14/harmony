#!/usr/bin/env python3
"""Reconstruct admitted failure streaks in the already replayed AP01 stream."""
from collections import Counter, defaultdict
import gzip
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]/'endpoint-encounter'


def raw(name):
    return gzip.decompress((ROOT/'ap01-output'/name).read_bytes())


def audit():
    stream = raw('stream.jsonl.gz')
    rows = [json.loads(line) for line in stream.splitlines()]
    report = json.loads(raw('campaign.json.gz'))
    assert json.loads(raw('result.json.gz'))['full_campaign_replay']
    assert report['duplicates_skipped'] == 0
    sidecar = json.loads(raw('progress.jsonl.gz').splitlines()[-1])
    assert rows[0]['parent_scheduler'] == 'room_cell_uniform_128_energy_progress_cheapest_v1:3,6,12,2'
    assert rows[0]['mixture_policy'] == 'alphabet_only'
    assert rows[0]['origin_kind'] == 'snapshot_root'
    seen, streaks = {0}, defaultdict(int)
    counts, productive, late, track = Counter(), [], [], []
    for j in rows[1:]:
        assert j['event'] == 'job' and j['parent_id'] in seen
        if j['selector']['counter_reset']:
            streaks.clear()
        parent = j['parent_id']; before = streaks[parent]
        retained = [d['id'] for d in j['decisions'] if d['decision'] == 'retained']
        for child in retained:
            if child not in seen:
                assert child == len(seen)
                seen.add(child)
        category = ('at_least_3' if before >= 3 else 'below_3')+'_'+j['selector']['path']
        counts[category] += 1
        streaks[parent] = 0 if retained else before+1
        row = {'sequence': j['sequence'], 'parent_id': parent, 'path': j['selector']['path'],
               'pre_admission_failure_streak': before, 'frames': j['frames'],
               'retained_ids': retained}
        if before >= 3:
            late.append(row)
            if retained:
                productive.append(row)
        if parent == 982:
            track.append(row)
    assert len(seen) == report['archive']['retained']
    selector = report['archive']['selector']
    assert sum(bool([d for d in j['decisions'] if d['decision']=='retained']) for j in rows[1:]) == selector['productive_selections']
    active = json.loads((ROOT/'ci01-analysis.json').read_text())['active_ids']
    assert sum(streaks[i]>=3 for i in active) == selector['retirement']['entries_over_threshold']
    assert sum(j['selector']['path']=='uniform' for j in rows[1:]) == selector['uniform_selections']
    return {'format': 'ap01-entry-exhaustion-audit-v1',
            'stream_sha256': hashlib.sha256(stream).hexdigest(),
            'jobs': len(rows)-1, 'metadata_dropped_at_last_sidecar': sidecar['historical_entries_dropped'],
            'metadata_dropped_in_final_report': report['historical_entries_dropped'],
            'matched_report_productive_selections': selector['productive_selections'],
            'matched_active_entries_over_threshold': selector['retirement']['entries_over_threshold'], 'all_exhausted_resets': sum(j['selector']['counter_reset'] for j in rows[1:]),
            'admitted_job_counts': dict(counts), 'after_threshold_retained_jobs': productive,
            'after_threshold_frames': sum(j['frames'] for j in late), 'hp129_entry982': track,
            'limits': ['Streaks are reconstructed at admission, not necessarily the earlier dispatch boundary; queued work may cross the threshold.',
                       'Retained descendants are archive outcomes, not useful milestones or demonstrated boss damage.',
                       'One conditional stream; no fresh-search prevalence or efficacy estimate.',
                       'The uniform fallback and all-exhausted reset prevent absolute permanent starvation.']}


if __name__ == '__main__':
    print(json.dumps(audit(), indent=2))
