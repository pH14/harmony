#!/usr/bin/env python3
"""F03 own-chain inputs and immutable bounded MM2 trajectory diagnostics."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
root=Path('/root/harmony-alternative-futures-followup-20260909');old=Path('/root/harmony-alternative-futures-20260908')
def sha(p): return hashlib.sha256(p.read_bytes()).hexdigest()
binary=root/'builds/f04-mm2/mm2-future-probe'
assert sha(binary)=='8265697e3e51c6bc445dbee19fbeff431d95ccb529c5b59b239679510f49ad6c'
proof={'format':'F03-execution-v2','build':json.loads((binary.parent/'build-info.json').read_text()),'runs':[]}
for chain in ['c02-chain-s20261001','c03-chain-s20261001']:
    directory=old/'runs'/chain
    matches=sorted((directory/'08-wily1').rglob('request.private.json'))
    assert len(matches)==1, 'ambiguous chain stage requests'
    private=matches[0]
    prior=json.loads(private.read_text());prefix=Path(prior['prefix_input']);source=private.parent/'campaign/witness-input.json'
    chain_json=json.loads((directory/'chain.json').read_text())
    assert chain_json['stages'][-1]['stage']=='wily1'
    assert sha(prefix)==prior['prefix_sha256']==chain_json['stages'][-1]['parent_prefix_sha256']
    assert sha(source)=={'c02-chain-s20261001': 'fd00a3b5778b229dd29bf218b265279fc3d2ec33d3612630a2bced5eefa06193', 'c03-chain-s20261001': '12813b3581a3ed72a4a2935f4c01531e573ea1035745f8fe99bb5ab73e7a7d5e'}[chain], 'frozen trajectory input changed'
    request={'core':prior['core'],'rom':prior['rom'],'prefix':str(prefix),'prefix_sha256':sha(prefix),'input':str(source),'input_sha256':sha(source),'source_snapshot_sha256':chain_json['stages'][-1]['result']['witness']['snapshot_sha256']}
    request_path=root/('f04-'+chain+'-request.json')
    with request_path.open('x') as f: json.dump(request,f)
    output=root/('f04-'+chain)
    start=time.monotonic()
    with (root/('f04-'+chain+'.log')).open('xb') as log:
        child=subprocess.Popen(['timeout','--signal=TERM','--kill-after=5','300','taskset','-c','17',str(binary),str(request_path),str(output)],stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
        _,status,u=os.wait4(child.pid,0)
    row={'chain':chain,'chain_sha256':sha(directory/'chain.json'),'exit_code':os.waitstatus_to_exitcode(status),'elapsed_seconds':time.monotonic()-start,'cpu_seconds':u.ru_utime+u.ru_stime,'max_rss_kib':u.ru_maxrss,'wall_bound_seconds':300}
    proof['runs'].append(row);(root/'f04-mm2-execution-partial.json').write_text(json.dumps(proof,indent=2)+'\n')
    assert row['exit_code']==0,chain
    row['summary']=json.loads((output/'summary.json').read_text());row['outcomes_sha256']=sha(output/'outcomes.jsonl');row['sources_sha256']=sha(output/'sources.json')
    baseline=next(v for v in json.loads((root/'f03-execution.json').read_text())['runs'] if v['chain']==chain)
    assert row['outcomes_sha256']==baseline['outcomes_sha256']
    for field in ['origin','endpoint','sources','total_physical_frames']:
        assert row['summary'][field]==baseline['summary'][field],field
    print(json.dumps({k:v for k,v in row.items() if k!='summary'}),flush=True)
with (root/'f04-mm2-execution.json').open('x') as f: json.dump(proof,f,indent=2);f.write('\n')
