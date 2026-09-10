#!/usr/bin/env python3
"""AP01-specific active/cached census; never infer lifetime damage from HP."""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path
import sys

from score_s01 import raw
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'depth-transfer'))
from boss_interval import classify


def sha(data):
    return hashlib.sha256(data).hexdigest()


def load(root, name):
    return json.loads(raw(root, name))


def active_ids(rows, stream, sidecar):
    """Latest ID per slot, only under the source-proved CI01 preconditions."""
    h = stream[0]
    assert h['key_policy'] == 'metroid_items_tanks_spatial_16_posture_motion_context_selection_32_legacy_progress_v10'
    assert h['replacement_policy'] == 'opaque_preference_then_fewest_frames'
    assert h.get('slot_retention_policy') is None  # AP01 native adapter fixes the default.
    assert h['retention_policy'] == 'admit_alive' and h['suffix_policy'] == 'one_to_six'
    assert h['origin_kind'] == 'snapshot_root' and h['resume_actions'] == 0
    assert sidecar['snapshot_evictions'] == sidecar['entry_drops'] == 0
    assert sidecar['retained_diagnostics']['missing_snapshots'] == 0
    assert sidecar['retention_context_census']['largest_slot'] == 1
    assert sidecar['retention_diagnostics']['alternative_admissions'] == 0
    assert sidecar['retained'] < h['archive_entry_limit']
    bounds = {0: 0}
    for job in stream[1:]:
        assert job['event'] == 'job' and job['parent_id'] in bounds
        for decision in job['decisions']:
            if decision['decision'] == 'retained' and decision['id'] not in bounds:
                assert decision['id'] == len(bounds)
                bounds[decision['id']] = bounds[job['parent_id']] + 6
    assert len(bounds) == sidecar['retained']
    assert max(bounds.values()) < h['action_limit']
    assert len({r['id'] for r in rows}) == len(rows) == sidecar['resident_snapshots']
    assert all(r['id'] in bounds for r in rows)
    slots = defaultdict(list)
    for row in rows:
        slots[json.dumps(row['retention_group'], sort_keys=True)].append(row['id'])
    active = {max(ids) for ids in slots.values()}
    assert len(active) == sidecar['active_entries'] == sidecar['retention_context_census']['slots']
    assert len(active) == sidecar['retained_diagnostics']['active_entries']
    return active, max(bounds.values())


def slots_for(context):
    memory = context['memory']
    flat = {'area': memory['area'], 'mode': memory['mode']}
    for i, e in enumerate(memory['enemies']):
        assert e['slot'] == i * 16
        for dst, src in [('offset', 'slot'), ('status', 'status'), ('type', 'data_index'), ('special', 'special'), ('hp', 'hit_points')]:
            flat[f'slot{i}_{dst}'] = e[src]
        flat[f'slot{i}_saved_status'] = context['saved_status'][i]
    return [classify(flat, i) for i in range(6)]


def describe(rows):
    counts, hp_counts, ids = Counter(), Counter(), []
    for r in rows:
        slots = slots_for(r['context'])
        mask = sum(1 << i for i, s in enumerate(slots) if s is not None)
        assert r['endpoint_boss_slots'] == mask
        available = [s.hp for s in slots if s is not None and s.hp != 255]
        counts['entries'] += 1
        counts['classified_entries'] += bool(mask)
        counts['available_hp_entries'] += bool(available)
        counts['unavailable_hp_slots'] += sum(s is not None and s.hp == 255 for s in slots)
        for s in slots:
            if s is not None and s.hp != 255:
                hp_counts[str(s.hp)] += 1
        if any(hp < 140 for hp in available):
            ids.append(r['id'])
        assert not r['dead'] and not r['failed']
        assert 0 < r['state']['health'] < 8000
    return {**counts, 'snapshot_local_hp_counts': dict(sorted(hp_counts.items(), key=lambda x: int(x[0]))),
            'entries_with_classified_hp_below_root_140': ids,
            'samus_health_range': [min(r['state']['health'] for r in rows), max(r['state']['health'] for r in rows)]}


def score(protocol, output):
    registration = load(protocol, 'ci01-registration.json')
    request_bytes = raw(protocol, 'ci01-request.json')
    q = json.loads(request_bytes)
    report = load(output, 'ci01-context.json')
    process = load(output, 'ci01-process.json')
    inventory = load(output, 'ci01-inventory.json')
    assert process['returncode'] == 0 and process['stop_reason'] is None
    assert process['wall_seconds'] <= registration['wall_seconds']
    assert report['request_sha256'] == sha(request_bytes) == registration['request_sha256']
    assert report['checkpoint_sha256'] == q['checkpoint_sha256'] == inventory['checkpoint_sha256']
    assert report['origin_sha256'] == q['origin_sha256']
    assert report['positive_control'] == q['expected_root_context']
    assert report['direct_physical_frames'] == report['setup_frames'] == 929
    assert report['continuation_frames'] == inventory['emulated_frames'] == 0
    assert report['direct_physical_frames'] <= registration['known_auxiliary_frame_ceiling']
    rows = report['entries']
    assert [{k: v for k, v in r.items() if k != 'context'} for r in rows] == inventory['entries']
    assert report['verified_restores'] == len(rows) + 1
    ap = protocol / 'ap01-output'
    assert sha(raw(ap, 'checkpoint.bin')) == q['checkpoint_sha256']
    assert sha(raw(ap, 'origin.bin')) == q['origin_sha256']
    assert load(ap, 'result.json')['full_campaign_replay']
    stream = [json.loads(line) for line in raw(ap, 'stream.jsonl').splitlines()]
    sidecar = json.loads(raw(ap, 'progress.jsonl').splitlines()[-1])
    active, max_actions = active_ids(rows, stream, sidecar)
    return {'format': 'metroid-ci01-analysis-v1', 'registration_sha256': sha(raw(protocol, 'ci01-registration.json')),
            'checkpoint_sha256': q['checkpoint_sha256'],
            'context_report_sha256': sha(raw(output, 'ci01-context.json')),
            'direct_physical_frames': 929, 'continuation_frames': 0,
            'active_inference': 'Latest admitted stable ID per cell under the source-backed CI01 preconditions.',
            'max_input_action_upper_bound': max_actions, 'active_ids': sorted(active),
            'active': describe([r for r in rows if r['id'] in active]),
            'inactive_cached': describe([r for r in rows if r['id'] not in active]),
            'limits': ['Snapshot-local HP, not continuous episode damage or lifetime progress.',
                       'Historical uncached/rejected endpoints remain unmeasured.',
                       'No policy change, fresh gain, capability or retention cause follows from this census alone.']}


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--protocol', type=Path, default=Path(__file__).parent)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    a = p.parse_args()
    a.out.write_text(json.dumps(score(a.protocol, a.output), indent=2) + '\n')
