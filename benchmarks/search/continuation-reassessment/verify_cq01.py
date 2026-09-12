#!/usr/bin/env python3
"""Verify the reused native unchanged-continuation fixture, buffering, hosts and known work."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path


def raw(path):
    return path.read_bytes() if path.exists() else gzip.decompress(Path(str(path)+'.gz').read_bytes())


def read(path):
    return json.loads(raw(path))


def sha(data):
    return hashlib.sha256(data).hexdigest()


def verify(protocol, output):
    total=0; records=[]; artifacts={}; events={}
    for host in ('msr1','ms02'):
        reg_path=protocol/f'cq01-{host}-registration.json'; reg=read(reg_path)
        panel=read(output/host/'results.json')
        assert panel['registration_sha256']==sha(raw(reg_path))
        assert panel['execution_complete'] and panel['allocation_stop'] is None
        assert [r['id'] for r in panel['records']]==[c['id'] for c in reg['cells']]
        for record,cell in zip(panel['records'],reg['cells']):
            assert record['exit_code']==0 and record['checks_passed']
            summary=record['summary']; result=summary['result']
            assert summary['status']==result['status']=='complete'
            assert summary['build']['binary_sha256']==reg['binary_sha256']
            assert result['verification']=='campaign' and result['stop_reason'] in cell['allowed_stops']
            for key,value in cell['expected_identity'].items():
                assert summary['identity'][key]==value, key
            assert summary['elapsed_seconds']<=cell['subprocess_timeout_seconds']
            assert summary['max_process_rss_bytes']<=12*1024**3
            path=output/host/record['id']/summary['cell']/'campaign'
            assert read(path/'result.json')==result
            stream=raw(path/'stream.jsonl'); header=json.loads(stream.splitlines()[0])
            assert header['parent_scheduler']==cell['manifest']['search']['selector']
            assert header['mixture_policy']=='alphabet_continuation_v1'
            assert header['origin_kind']=='genesis' and header['resume_actions']==0
            jobs=[json.loads(line) for line in stream.splitlines()[1:]]
            assert all(j['event'] in ('job','skip') for j in jobs)
            continuations=[j for j in jobs if j['event']=='job' and j['selector']['path']=='continuation']
            assert 0 < len(continuations) <= 500, 'native fixture must exercise bounded continuation dispatch'
            assert all(j['sequence'] % 4 == 0 and j['splice']['outcome']=='tail' for j in continuations)
            campaign=read(path/'campaign.json')
            assert campaign['bootstrap_frames']+sum(j['frames'] for j in jobs if j['event']=='job')==result['frames_emulated']
            assert result['frames_emulated']<=500000+8*6*128
            witness=2*(result['witness']['physical_suffix_frames']+sum(w['replay']['physical_suffix_frames'] for w in result['milestone_witnesses'].values()))
            charge=2*result['frames_emulated']+witness; total+=charge
            key=f'{host}/{record["id"]}'
            artifacts[key]={name:sha(raw(path/name)) for name in ('stream.jsonl','campaign.json','checkpoint.json')}
            events[key]=stream.splitlines()[1:]
            records.append({'cell':key,'admitted_frames':result['frames_emulated'],'continuation_jobs':len(continuations),
                            'inferred_campaign_replay_admitted_frames':result['frames_emulated'],
                            'twice_replayed_witness_frames':witness,'known_auxiliary_frames':charge,
                            'elapsed_seconds':summary['elapsed_seconds'],'cpu_seconds':summary['cpu_seconds'],
                            'max_process_rss_bytes':summary['max_process_rss_bytes'],'artifact_sha256':artifacts[key]})
    assert artifacts['msr1/one-slot']==artifacts['msr1/two-slots']
    assert events['msr1/two-slots']==events['ms02/two-slots']
    assert total<=read(protocol/'cq01-registration.json')['known_auxiliary_frame_ceiling']
    return {'format':'continuation-reassessment-cq01-qualification-v1','decision':'pass','records':records,
            'same_host_buffer_identity':True,'cross_host_post_header_stream_identity':True,
            'known_auxiliary_frames':total,
            'limits':['Reused fixture; no performance or fresh-validation claim.',
                      'Full campaign replay work inferred from admitted work; in-job origin replay is included. Setup, unadmitted work and any out-of-job reconstruction remain additional unknowns.',
                      'Cross-host machine snapshots and costs are not claimed identical.']}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--protocol',type=Path,default=Path(__file__).parent)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--out',type=Path,required=True)
    a=p.parse_args(); a.out.write_text(json.dumps(verify(a.protocol,a.output),indent=2)+'\n')
