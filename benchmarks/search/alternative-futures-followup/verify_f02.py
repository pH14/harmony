#!/usr/bin/env python3
"""Verify every F02 witness twice from ordinary genesis without new suffix work."""
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
root = Path('/root/harmony-alternative-futures-followup-20260909')
old = Path('/root/harmony-alternative-futures-20260908')
def sha(p): return hashlib.sha256(p.read_bytes()).hexdigest()
out = root / 'f02-matched-a'
assert sha(out/'outcomes.jsonl') == '4b0d0057cca74c34fc78247d4e4eb51e3e995c3f4b0658108ea66ead83fccbb8'
summary = json.loads((out/'summary.json').read_text())
files = [f for p in summary['pairs'] for side in ['discarded','survivor'] for f in p[side]['witness_files']]
assert len(files) == len(set(files)) == 15
wanted = {}
for file in files:
    match = re.fullmatch(r'witness-(s\d+-p\d+-(?:candidate|incumbent))-t(\d+)\.json', file)
    assert match
    wanted[(match[1], int(match[2]))] = file
records = {}
with (out/'outcomes.jsonl').open() as f:
    for line in f:
        row=json.loads(line); key=(row['side'],row['trial'])
        if key in wanted: records[wanted[key]]=row['outcome']
assert set(records) == set(files)
template = json.loads((old/'probes/p02-audit.json').read_text())['samples'][2][0]
pairs=[]
for file in files:
    p=copy.deepcopy(template); p.update(stratum=0,execution=len(pairs),replaces=True)
    p['candidate']=p['incumbent']=records[file]['endpoint']
    p['candidate_input']=p['incumbent_input']=json.loads((out/file).read_text())
    pairs.append(p)
audit={'format':'metroid-retention-audit-v1','samples':[pairs,[],[],[],[]]}
planned=sum(max(1,min(120,a['hold_frames'])) for p in pairs for side in ['candidate_input','incumbent_input'] for a in p[side]['actions'])
assert planned <= 2_000_000
source=root/'f02-witness-audit.json'
with source.open('x') as f: json.dump(audit,f)
assets=json.loads(Path('/root/harmony-search-eval/assets.json').read_text())
binary=root/'builds/f02-verify/metroid-retention-probe'
assert sha(binary)=='3d692ea9483a5f9f8567d2dc7fb3a696efd54210ca8b17aecb623331f9ec6306'
output=root/'f02-witness-verification'
command=[str(binary),'--verify-sources',assets['core']['path'],assets['metroid']['path'],str(source),str(output)]
start=time.monotonic()
with (root/'f02-witness-verification.log').open('xb') as log:
    child=subprocess.Popen(['timeout','--signal=TERM','--kill-after=5','300','taskset','-c','16',*command],stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
    _,status,u=os.wait4(child.pid,0)
resources={'exit_code':os.waitstatus_to_exitcode(status),'elapsed_seconds':time.monotonic()-start,'cpu_seconds':u.ru_utime+u.ru_stime,'max_rss_kib':u.ru_maxrss,'wall_bound_seconds':300}
(root/'f02-witness-resources.json').write_text(json.dumps(resources,indent=2)+'\n')
assert resources['exit_code']==0
result=json.loads((output/'summary.json').read_text()); verified=[]
for file,p in zip(files,result['pairs']):
    a,b=p['sides']; assert a==b
    required=set(map(tuple,records[file]['reached_maps']))
    assert required <= set(map(tuple,a['living_maps']))
    verified.append({'file':file,'input_sha256':sha(out/file),'endpoint':a['endpoint'],'dead':a['dead'],'snapshot_sha256':a['snapshot_sha256'],'required_living_maps':sorted(required),'verified_replays':2,'physical_frames':2*a['physical_frames']})
proof={'format':'F02-witness-verification-v1','build':json.loads((binary.parent/'build-info.json').read_text()),'resources':resources,'audit_sha256':sha(source),'result_sha256':sha(output/'summary.json'),'physical_frames':result['physical_frames'],'witnesses':verified}
with (root/'f02-witness-proof.json').open('x') as f: json.dump(proof,f,indent=2);f.write('\n')
print(json.dumps({'witnesses':len(verified),'verified_replays':2*len(verified),'physical_frames':result['physical_frames'],'resources':resources}))
