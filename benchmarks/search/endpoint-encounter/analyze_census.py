#!/usr/bin/env python3
"""Score the frozen development census without converting missing evidence to negatives."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path


def sha_bytes(value):
    return hashlib.sha256(value).hexdigest()


def analyze(reg, panel, registration_sha256, artifact=None):
    assert panel['registration_sha256'] == registration_sha256
    assert len(reg['cells']) == 1 and len(panel['records']) <= 1
    out = {'format':'metroid-endpoint-census-analysis-v1',
           'registration_sha256':registration_sha256, 'complete':False,
           'decision':'incomplete; no negative encounter claim or automatic rerun',
           'admitted_frames_known':0, 'known_auxiliary_frames':0,
           'endpoint_counts':None, 'first_encounter':None,
           'unknown':['Setup, unadmitted work and unmeasured verification after failures remain additional unknowns.'],
           'scope':'One reused development seed. Eligible completed live endpoints only; no fresh-validation or performance claim.'}
    if not panel['records']:
        return out
    record = panel['records'][0]
    assert record['id'] == reg['cells'][0]['id']
    summary = record.get('summary')
    if not summary:
        return out
    identity = summary.get('identity') or {}
    for k,v in reg['cells'][0]['expected_identity'].items():
        assert identity.get(k) == v, 'identity mismatch: ' + k
    assert summary['build']['binary_sha256'] == reg['binary_sha256']
    manifest = reg['cells'][0]['manifest']
    expected = {**manifest['search'], 'seed':manifest['seeds'][0],
                'workers':manifest['workers'][0], 'memory_mib':manifest['memory_mib'][0],
                'game':manifest['cases'][0]['game'], 'rom_sha256':manifest['cases'][0]['rom_sha256']}
    for k,v in expected.items():
        assert summary['search_request'].get(k) == v, 'request mismatch: ' + k
    progress = summary.get('last_progress') or {}
    result = summary.get('result') or {}
    out['reported_stop_reason'] = result.get('stop_reason')
    out['record_failure'] = record.get('failure')
    out['last_reported_observations'] = {
        'frames':progress.get('frames_emulated'), 'executions':progress.get('executions'),
        'endpoint_counts':progress.get('workload_diagnostics',{}).get('endpoint_encounters',{}).get('counts'),
        'named_progress':progress.get('workload_diagnostics',{}).get('named_progress'),
        'scope':'Last complete progress record; may omit later interrupted work and does not establish completion of the registered horizon.'}
    out['admitted_frames_known'] = result.get('frames_emulated', progress.get('frames_emulated', 0))
    out['search_frame_overshoot'] = max(0, out['admitted_frames_known'] - reg['nominal_admitted_search_limit'])
    normal = [result.get('witness')] + [v.get('replay') for v in result.get('milestone_witnesses',{}).values()]
    out['known_auxiliary_frames'] = 2 * sum(v['physical_suffix_frames'] for v in normal if v is not None)
    encounter = result.get('endpoint_encounter_witness')
    if encounter:
        out['known_auxiliary_frames'] += encounter['known_replay_frames']
    out['resource_cost'] = {k:summary.get(k) for k in ('cpu_seconds','elapsed_seconds','max_process_rss_bytes','peak_disk_logical_bytes_sampled')}
    complete = (panel['execution_complete'] and panel['allocation_stop'] is None
                and record.get('checks_passed') and record['exit_code'] == 0
                and summary['status'] == result.get('status') == 'complete'
                and result.get('stop_reason') == 'frame_limit'
                and out['admitted_frames_known'] >= reg['nominal_admitted_search_limit'])
    if not complete:
        return out
    assert result['verification'] == 'witness'
    assert progress['frames_emulated'] == result['frames_emulated']
    assert progress['executions'] == result['executions']
    diagnostic = progress['workload_diagnostics']['endpoint_encounters']
    assert diagnostic['format'] == 'metroid-live-endpoint-encounters-v1'
    counts = diagnostic['counts']
    for key in ('actions_observed','live_endpoints','classified_endpoints'):
        assert type(counts[key]) is int and counts[key] >= 0
    assert counts['actions_observed'] >= counts['live_endpoints'] >= counts['classified_endpoints']
    assert counts['live_endpoints'] > 0
    named = progress['workload_diagnostics']['named_progress']
    assert named['format'] == 'metroid-named-progress-v2'
    areas = {k:named['first_seen'][k] for k in ('kraid_area','ridley_area')}
    out.update(complete=True, endpoint_counts=counts, first_encounter=counts['first'], boss_areas=areas,
               named_progress=named, known_auxiliary_within_ceiling=out['known_auxiliary_frames'] <= reg['known_auxiliary_limit'])
    if counts['classified_endpoints'] == 0:
        assert counts['first'] is None and encounter is None and artifact is None
        out['decision'] = ('no classified eligible endpoint despite boss-area observations' if any(areas.values())
                           else 'no classified eligible endpoint; boss-area regime not observed')
    else:
        first = counts['first']
        assert 1 <= first['execution'] <= result['executions']
        assert first['area'] in (18,20) and 1 <= first['boss_slots'] <= 63
        assert first['route_action_end_frame'] > 0
        assert encounter and encounter['first'] == first and encounter['verified_replays'] == 2
        replay = encounter['replay']
        assert replay['dead'] is False and replay['physical_suffix_frames'] == first['route_action_end_frame']
        assert encounter['known_replay_frames'] == 2 * replay['physical_suffix_frames']
        replay_first = replay['diagnostics']['endpoint_encounters']['counts']['first']
        for k in ('area','boss_slots','route_action_end_frame'):
            assert replay_first[k] == first[k]
        assert artifact is not None and sha_bytes(artifact) == encounter['artifact_sha256']
        envelope = json.loads(artifact)
        assert envelope['format'] == 'metroid-endpoint-encounter-input-v1' and envelope['first'] == first
        assert 0 < len(envelope['input']['actions']) <= manifest['search']['actions']
        out['encounter_artifact_sha256'] = encounter['artifact_sha256']
        out['decision'] = 'positive producing endpoint; standalone observer qualification required before local combat'
    if not out['known_auxiliary_within_ceiling']:
        out['decision'] = 'auxiliary ceiling exceeded; stop escalation and retain evidence'
    return out


if __name__ == '__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--registration',type=Path,required=True)
    p.add_argument('--panel',type=Path,required=True)
    p.add_argument('--encounter-artifact',type=Path)
    p.add_argument('--out',type=Path,required=True)
    args=p.parse_args()
    raw=args.panel.read_bytes()
    if args.panel.suffix == '.gz': raw=gzip.decompress(raw)
    result=analyze(json.loads(args.registration.read_text()),json.loads(raw),sha_bytes(args.registration.read_bytes()),
                   args.encounter_artifact.read_bytes() if args.encounter_artifact else None)
    result['panel_sha256']=sha_bytes(raw)
    assert not args.out.exists()
    args.out.write_text(json.dumps(result,indent=2)+'\n')
