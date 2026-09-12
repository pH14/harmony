#!/usr/bin/env python3
"""Verify the three-pass native positive episode with the existing Python decoder."""
import argparse
import gzip
import hashlib
import importlib.util
import json
from pathlib import Path

MODULE=Path(__file__).resolve().parents[1]/'depth-transfer/boss_interval.py'
import sys
spec=importlib.util.spec_from_file_location('endpoint_boss_reference',MODULE)
reference=importlib.util.module_from_spec(spec)
sys.modules[spec.name]=reference
spec.loader.exec_module(reference)


def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()


def verify(root):
    reg=json.loads((root/'e01-registration.json').read_text())
    summary=json.loads((root/'e01-summary.json').read_text())
    assert summary['format']=='metroid-boss-memory-probe-v3'
    assert summary['trace_observation_policy']=='boss_context_intervals_v1'
    assert summary['verified_replays']==reg['expected_replays']==3
    for k in ('input_sha256','core_sha256','rom_sha256'):
        assert summary[k]==reg[k]
    for k in ('endpoint','raw_endpoint','emulator_sha256','route_frames'):
        assert summary['ordinary'][k]==summary['one_frame'][k]
    assert summary['one_frame']['route_frames']==reg['expected_route_frames']
    actual=summary['ordinary']['physical_frames_including_setup']+2*summary['one_frame']['physical_frames_including_setup']
    assert actual==reg['expected_physical_frames_including_setup']<=reg['auxiliary_frame_ceiling']
    trace=gzip.decompress((root/'e01-relevant-frames.jsonl.gz').read_bytes())
    assert len(trace)==summary['one_frame']['relevant_trace_bytes']<=reg['trace_limit_bytes']
    observer=reference.BossIntervalObserver()
    previous=None
    first=None
    classified=0
    checked=0
    hp_drops=[]
    last=None
    for line in trace.splitlines():
        row=json.loads(line)
        assert len(row)==11 and len(row[8])==len(row[9])==len(row[10])==6
        raw={'frame':row[0],'area':row[1],'mode':row[2]}
        for slot,fields in enumerate(row[8]):
            for name,value in zip(('offset','status','type','special','hp','x','y','name_table'),fields):
                raw[f'slot{slot}_{name}']=value
            raw[f'slot{slot}_saved_status']=row[9][slot]
        mask=sum(1<<slot for slot in range(6) if reference.classify(raw,slot) is not None)
        if mask:
            classified+=1
            if first is None:first=row[0]
        if previous!=row[0]-1:observer.reset()
        report=observer.observe(0,row[0],raw)
        if previous==row[0]-1:
            expected=[None]*6
            for event in report['intervals']:
                expected[event['slot']]={k:event[k] for k in ('kind','hp_loss')}
                if event['kind']=='hp_drop':hp_drops.append({'frame':row[0],**event})
            assert expected==row[10]
            checked+=1
        previous=row[0]
        last={'frame':row[0],'area':row[1],'mask':mask}
    assert last=={'frame':reg['expected_route_frames'],'area':reg['producing_first']['area'],'mask':reg['producing_first']['boss_slots']}
    diagnostics=summary['one_frame']['context_diagnostics']
    assert classified==diagnostics['classified_frames']>0
    assert first==diagnostics['first_classified_route_frame']
    assert sum(x['hp_loss'] for x in hp_drops)==diagnostics['observed_hp_loss']
    assert (hp_drops[0]['frame'] if hp_drops else None)==diagnostics['first_hp_drop_route_frame']
    return {'format':'metroid-endpoint-e01-analysis-v1','decision':'pass',
        'registration_sha256':sha(root/'e01-registration.json'),'summary_sha256':sha(root/'e01-summary.json'),
        'trace_sha256':hashlib.sha256(trace).hexdigest(),'python_decoder_sha256':sha(MODULE),
        'known_auxiliary_frames':actual,'python_checked_retained_intervals':checked,
        'classified_frames':classified,'first_classified_route_frame':first,'last_classified_endpoint':last,
        'observed_hp_drop_events':hp_drops,'endpoint':summary['one_frame']['endpoint'],
        'raw_endpoint':summary['one_frame']['raw_endpoint'],
        'claim':'This reused development campaign generated a reproducible, live Ridley-context endpoint. Standalone cadence and Python decoding agree.',
        'limits':['C01 did not finish its registered 250M horizon; this does not relabel that census passed.',
                  'Only the saved first-encounter tape is examined; no claim about damage in other campaign branches.',
                  'Classification does not establish fight capability, retention history, allocation failure or fresh-search efficacy.']}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root',type=Path,default=Path(__file__).parent)
    p.add_argument('--out',type=Path,required=True)
    a=p.parse_args();assert not a.out.exists()
    a.out.write_text(json.dumps(verify(a.root),indent=2)+'\n')
