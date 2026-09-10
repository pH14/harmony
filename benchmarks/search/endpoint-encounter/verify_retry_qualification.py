#!/usr/bin/env python3
"""Recompute RQ01 identity, exercised-retry, replay and accounting gates."""
import argparse
import hashlib
import json
from pathlib import Path
from score_s01 import raw

ARTIFACTS = ('stream.jsonl','checkpoint.bin','origin.bin','campaign.json','root-snapshot.json',
             'root.json','result.json','witness-local.json','witness-full.json')
sha = lambda b: hashlib.sha256(b).hexdigest()


def read(root, name):
    return json.loads(raw(root, name))


def cell(protocol, output, name):
    q = read(protocol, name+'.json')
    r = read(output/name, 'result.json')
    usage = read(output/name,'usage.json')
    process = read(output,name+'-process.json')
    assert process['returncode'] == 0 and process['stop_reason'] is None
    assert process['wall_seconds'] <= 180
    assert usage['completed_execution'] and usage['error'] is None
    assert usage['request_sha256'] == sha(raw(protocol,name+'.json'))
    assert r['complete'] and r['executions'] == q['executions']
    assert r['frames'] <= q['frames'] + q['workers']*6*120
    assert r['root']['snapshot_sha256'] == q['expected_snapshot_sha256']
    assert r['root']['endpoint'] == q['expected_endpoint']
    assert r['root']['context'] == q['expected_context']
    assert r['root']['emulator_sha256'] == q['expected_emulator_sha256']
    assert r['full_campaign_replay']
    assert r['root_local_witness_replays'] == r['complete_prefix_witness_replays'] == 2
    assert r['stream_sha256'] == sha(raw(output/name,'stream.jsonl'))
    campaign = read(output/name,'campaign.json')
    local = read(output/name,'witness-local.json')
    prefix = read(protocol,'first-encounter-input.json')['actions']
    assert read(output/name,'witness-full.json')['actions'] == prefix + local['actions']
    assert r['witness']['local_input_sha256'] == sha(raw(output/name,'witness-local.json'))
    assert r['witness']['full_input_sha256'] == sha(raw(output/name,'witness-full.json'))
    retry = q.get('local_terminal_retry', False)
    retries = campaign.get('local_terminal_retries',0)
    first = campaign.get('first_local_retry')
    if retry:
        assert campaign['local_terminal_retry'] == 'one_per_live_boundary_predrawn_attempts_v1'
        assert retries > 0 and first is not None
        assert first['input'] == local and len(first['snapshot_sha256']) == 64
        assert not r['witness']['dead']
        # The source-attested native helper checks this postcard digest against
        # the actual linear replay snapshot before any successful result exists.
    else:
        assert retries == 0 and first is None and 'local_terminal_retry' not in campaign
    cost = r['cost']
    assert cost == usage['cost']
    assert cost['admitted_search_frames'] == cost['campaign_replay_admitted_frames'] == r['frames'] == campaign['frames_emulated']
    assert cost['direct_setup_frames'] == 6*929
    assert cost['direct_physical_frames'] == 6*929 + 4*sum(a['hold_frames'] for a in prefix+local['actions'])
    assert cost['direct_physical_frames'] <= q['direct_frame_limit']
    known = sum(cost[k] for k in ('admitted_search_frames','campaign_replay_admitted_frames','direct_physical_frames'))
    return {'executions':r['executions'],'admitted_frames':r['frames'],'retry_attempts':retries,
            'first_surviving_retry':first,'known_auxiliary_frames':known,
            'cost':cost,'process':process,'witness':r['witness'],
            'artifacts_sha256':{f:sha(raw(output/name,f)) for f in ARTIFACTS}}


def verify(protocol, output, phase):
    control = cell(protocol,output,'rq01-control')
    old = protocol/'aq01-output/one-slot'
    for f in ARTIFACTS:
        assert raw(output/'rq01-control',f) == raw(old,f), 'default artifact changed: '+f
    result = {'format':'local-retry-rq01-analysis-v1','decision':'pass',
              'registration_sha256':sha(raw(protocol,'rq01-registration.json')),
              'historical_default_artifacts_identical':True,'cells':{'rq01-control':control}}
    if phase == 'all':
        for name in ('rq01-one','rq01-two'):
            result['cells'][name] = cell(protocol,output,name)
        for f in ARTIFACTS:
            assert raw(output/'rq01-one',f) == raw(output/'rq01-two',f), 'buffer artifact changed: '+f
        result['candidate_buffer_artifacts_identical'] = True
    result['known_auxiliary_frames'] = sum(c['known_auxiliary_frames'] for c in result['cells'].values())
    assert result['known_auxiliary_frames'] <= read(protocol,'rq01-registration.json')['block_known_auxiliary_ceiling']
    result['scope'] = 'Native integration qualification on one supplied original root and reused seed; not fresh performance evidence.'
    result['unknown'] = 'Engine setup, unadmitted work and reconstruction remain additional unknown costs.'
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--protocol',type=Path,default=Path(__file__).parent)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--phase',choices=['control','all'],required=True)
    p.add_argument('--out',type=Path,required=True)
    a=p.parse_args()
    a.out.write_text(json.dumps(verify(a.protocol,a.output,a.phase),indent=2)+'\n')
