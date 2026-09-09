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
binary=root/'builds/f03/mm2-future-probe'
assert sha(binary)=='263934b5a447c8f31b0de524d38bfe787874dc3a41252181b550b007b4d0b0d2'
proof={'format':'F03-execution-v1','build':json.loads((binary.parent/'build-info.json').read_text()),'runs':[]}
for chain in ['c02-chain-s20261001','c03-chain-s20261001']:
    directory=old/'runs'/chain
    private=next((directory/'08-wily1').rglob('request.private.json'))
    prior=json.loads(private.read_text());prefix=Path(prior['prefix_input']);source=private.parent/'campaign/witness-input.json'
    chain_json=json.loads((directory/'chain.json').read_text())
    assert chain_json['stages'][-1]['stage']=='wily1'
    assert sha(prefix)==prior['prefix_sha256']==chain_json['stages'][-1]['parent_prefix_sha256']
    request={'core':prior['core'],'rom':prior['rom'],'prefix':str(prefix),'prefix_sha256':sha(prefix),'input':str(source),'input_sha256':sha(source)}
    request_path=root/('f03-'+chain+'-request.json')
    with request_path.open('x') as f: json.dump(request,f)
    output=root/('f03-'+chain)
    start=time.monotonic()
    with (root/('f03-'+chain+'.log')).open('xb') as log:
        child=subprocess.Popen(['timeout','--signal=TERM','--kill-after=5','300','taskset','-c','17',str(binary),str(request_path),str(output)],stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
        _,status,u=os.wait4(child.pid,0)
    row={'chain':chain,'chain_sha256':sha(directory/'chain.json'),'exit_code':os.waitstatus_to_exitcode(status),'elapsed_seconds':time.monotonic()-start,'cpu_seconds':u.ru_utime+u.ru_stime,'max_rss_kib':u.ru_maxrss,'wall_bound_seconds':300}
    proof['runs'].append(row);(root/'f03-execution-partial.json').write_text(json.dumps(proof,indent=2)+'\n')
    assert row['exit_code']==0,chain
    row['summary']=json.loads((output/'summary.json').read_text());row['outcomes_sha256']=sha(output/'outcomes.jsonl');row['sources_sha256']=sha(output/'sources.json')
    print(json.dumps({k:v for k,v in row.items() if k!='summary'}),flush=True)
with (root/'f03-execution.json').open('x') as f: json.dump(proof,f,indent=2);f.write('\n')
