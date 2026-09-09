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
def validate_replays(files, result, bindings, policy):
    def require(condition, message):
        if not condition:
            raise ValueError(message)
    require(result['terminal_policy'] == policy, 'verification terminal policy mismatch')
    require(len(result['pairs']) == len(files), 'verification witness count mismatch')
    verified = []
    for index, (file, pair) in enumerate(zip(files, result['pairs'])):
        require(pair['pair'] == index and pair['stratum'] == 0, 'verification witness order mismatch')
        require(len(pair['sides']) == 2, 'verification needs two independent targets')
        a, b = pair['sides']
        expected = bindings[file]
        require(a == b, 'independent replay mismatch')
        require(expected['terminal_policy'] == policy, 'producer terminal policy mismatch')
        require(a['snapshot_sha256'] == expected['producer_snapshot_sha256'], 'producer snapshot mismatch')
        require(a['endpoint'] == expected['endpoint'], 'producer endpoint mismatch')
        require(a['map_from_action'] == expected['prefix_actions'], 'verification suffix window mismatch')
        required = set(map(tuple, expected['suffix_living_maps']))
        require(required <= set(map(tuple, a['living_maps'])), 'missing living suffix map')
        verified.append({'file': file, 'input_sha256': expected['input_sha256'], 'endpoint': a['endpoint'],
                         'dead': a['dead'], 'snapshot_sha256': a['snapshot_sha256'],
                         'producer_snapshot_sha256': expected['producer_snapshot_sha256'],
                         'prefix_actions': expected['prefix_actions'], 'required_living_suffix_maps': sorted(required),
                         'verified_replays': 2, 'physical_frames': 2*a['physical_frames']})
    return verified


def main():
    root = Path('/root/harmony-alternative-futures-followup-20260909')
    old = Path('/root/harmony-alternative-futures-20260908')
    def sha(p): return hashlib.sha256(p.read_bytes()).hexdigest()
    out = root / 'f04-matched'
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
    bindings={}
    for file in files:
        p=copy.deepcopy(template); p.update(stratum=0,execution=len(pairs),replaces=True,created_execution=0,in_window_ever=False)
        p['exposure']={k:0 if isinstance(v,int) else None for k,v in p['exposure'].items()}
        p['candidate']=p['incumbent']=records[file]['endpoint']
        p['candidate_input']=p['incumbent_input']=json.loads((out/file).read_text())
        bindings[file]=json.loads((out/(file+'.proof.json')).read_text())
        assert bindings[file]['input_sha256']==sha(out/file)
        assert bindings[file]['suffix_living_maps']==records[file]['reached_maps']
        pairs.append(p)
    audit={'format':'metroid-retention-audit-v1','samples':[pairs,[],[],[],[]],'verification_prefix_actions':[[bindings[f]['prefix_actions']]*2 for f in files]}
    # Rust owns normalized hold and aggregate physical-work preflight bounds.
    source=root/'f04-witness-audit.json'
    with source.open('x') as f: json.dump(audit,f)
    assets=json.loads(Path('/root/harmony-search-eval/assets.json').read_text())
    binary=root/'builds/f04-metroid/metroid-retention-probe'
    assert sha(binary)=='6906a651d67cdf539e40ab8a549c4c95ad08b8034cb789adb48881ebfbc6cc4b'
    output=root/'f04-witness-verification'
    command=[str(binary),'--verify-sources',assets['core']['path'],assets['metroid']['path'],str(source),str(output)]
    start=time.monotonic()
    with (root/'f04-witness-verification.log').open('xb') as log:
        child=subprocess.Popen(['timeout','--signal=TERM','--kill-after=5','300','taskset','-c','16',*command],stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
        _,status,u=os.wait4(child.pid,0)
    resources={'exit_code':os.waitstatus_to_exitcode(status),'elapsed_seconds':time.monotonic()-start,'cpu_seconds':u.ru_utime+u.ru_stime,'max_rss_kib':u.ru_maxrss,'wall_bound_seconds':300}
    (root/'f04-witness-resources.json').write_text(json.dumps(resources,indent=2)+'\n')
    assert resources['exit_code']==0
    result=json.loads((output/'summary.json').read_text())
    verified=validate_replays(files,result,bindings,summary['terminal_policy'])
    assert sum(v['physical_frames'] for v in verified)==result['physical_frames']
    proof={'format':'F02-witness-verification-v2','build':json.loads((binary.parent/'build-info.json').read_text()),'resources':resources,'audit_sha256':sha(source),'result_sha256':sha(output/'summary.json'),'physical_frames':result['physical_frames'],'witnesses':verified}
    with (root/'f04-witness-proof.json').open('x') as f: json.dump(proof,f,indent=2);f.write('\n')
    print(json.dumps({'witnesses':len(verified),'verified_replays':2*len(verified),'physical_frames':result['physical_frames'],'resources':resources}))


if __name__ == '__main__':
    main()
