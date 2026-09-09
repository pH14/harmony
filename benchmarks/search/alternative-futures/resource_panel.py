#!/usr/bin/env python3
"""Bounded paired development and qualification for the resource-extremes policy."""
import argparse,json,pathlib,subprocess
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--root',type=pathlib.Path,required=True);p.add_argument('--mode',choices=['smoke','legacy','development','long'],required=True);p.add_argument('--policy',choices=['control','resource'],default='resource');p.add_argument('--seed',type=int,default=3);p.add_argument('--build',default='resource-005');p.add_argument('--label',default='r01');p.add_argument('--mixture',default='alphabet_only');p.add_argument('--metroid-only',action='store_true');a=p.parse_args()
root=a.root;panel=root/'manifests/alternative-futures';build=root/'builds'/a.build
base=json.loads((root/'benchmarks/search/metroid-long-horizon-semantic.json').read_text());base['seeds']=[a.seed]
base['search']['retention_audit']=True
if a.policy=='resource':base['search']['slot_retention']='resource_extremes_2_v1'
limits={'smoke':(5000,70000000,240),'legacy':(100000,400000000,240),'development':(500000,70000000,1080),'long':(3000000,400000000,5280)}
executions,frames,seconds=limits[a.mode]
base['search'].update(executions=executions,frames=frames,wall_seconds=seconds,verification='campaign' if a.mode=='smoke' else 'witness',mixture=a.mixture)
if a.mode=='legacy' and (a.mixture!='alphabet_only' or a.policy!='control' or a.seed!=3):raise ValueError('legacy hash check requires its exact frozen condition')

if a.mode=='smoke' and not a.metroid_only:
 base['cases'].append({'id':'mm2-metal','game':'mm2','stage':6,'origin':'ordinary independent Metal stage setup; qualification only','rom_sha256':'49136b412ff61beac6e40d0bbcd8691a39a50cd2744fdcdde3401eed53d71edf','search':{'retention_audit':False,'selector':'room_cell_uniform_128_energy_frontier_cheapest:3,6,12,2','mixture':'energy_splice:6'}})
name=f'{a.label}-{a.mode}-{a.policy}-s{a.seed}';base['id']=name
panel.mkdir(parents=True,exist_ok=True);manifest=panel/f'{name}.json';out=root/'runs'/name
if manifest.exists() or out.exists():raise FileExistsError('experiment names and manifests are immutable')
manifest.write_text(json.dumps(base,indent=2)+'\n')

cmd=['python3',str(root/'benchmarks/search/eval.py'),'run',str(manifest),'--assets','/root/harmony-search-eval/assets.json','--binary',str(build/'nes-eval'),'--build-info',str(build/'build-info.json'),'--out',str(out),'--jobs','1','--cpus','4','--memory-capacity-mib','10240','--finish-seconds','60' if a.mode in ['smoke','legacy'] else '120','--disk-limit-gib','4']
subprocess.run(cmd,check=True)
for path in sorted(out.glob('*/summary.json')):
 s=json.loads(path.read_text());r=s.get('result') or {};progress=s.get('last_progress') or {}
 print(json.dumps({'cell':s['cell'],'status':s['status'],'solved':r.get('solved'),'executions':r.get('executions'),'frames':r.get('frames_emulated'),'stream':r.get('stream_sha256'),'retention':progress.get('retention_diagnostics'),'diagnostics':progress.get('workload_diagnostics')}),flush=True)
 if a.mode=='legacy':assert r.get('stream_sha256')=='bd2832ea030de2a6ba8b5247b71b560f52682dbbd4042a6847b115505d975cde','legacy stream changed'

 if a.mode=='smoke' and a.policy=='resource' and a.seed==3 and a.mixture=='alphabet_only':
  expected={'metroid-full':'b5fd812953975c745732eaf1ff0be2465cf4a8381eb5596b02a5fc8d147706e8','mm2-metal':'fe3c0d35a75fc2c802d952920d05e6514107594f891e35f5c3aa7ef96b3851a0'}
  assert r.get('stream_sha256')==expected[s['case']],'candidate smoke stream changed'
