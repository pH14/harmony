#!/usr/bin/env python3
"""Fresh fixed-order MM2 chains; never accepts a supplied gameplay prefix."""
import argparse, hashlib, json, pathlib, subprocess, time

STAGES = [('metal',6),('heat',0),('air',1),('wood',2),('bubble',3),('quick',4),('flash',5),('crash',7),('wily1',8),('wily2',9),('wily3',10)]
def sha(path): return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()
def write(path, value):
    path=pathlib.Path(path);temp=path.with_suffix('.tmp');temp.write_text(json.dumps(value,indent=2)+'\n');temp.replace(path)
def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root',type=pathlib.Path,required=True);p.add_argument('--out',type=pathlib.Path,required=True)
    p.add_argument('--binary',type=pathlib.Path,required=True);p.add_argument('--build-info',type=pathlib.Path,required=True)
    p.add_argument('--progress-binary',type=pathlib.Path,required=True);p.add_argument('--assets',type=pathlib.Path,required=True)
    p.add_argument('--seed',type=int,required=True);p.add_argument('--mixture',default='energy_splice:6')
    p.add_argument('--selector',default='room_cell_uniform_128_energy_frontier_cheapest:3,6,12,2')
    p.add_argument('--executions',type=int,default=1000000);p.add_argument('--frames',type=int,default=120000000)
    p.add_argument('--stage-seconds',type=int,default=1080);p.add_argument('--chain-seconds',type=int,default=5400)
    p.add_argument('--qualification',action='store_true');p.add_argument('--slot-retention')
    a=p.parse_args();a.out.mkdir(parents=True,exist_ok=False)
    started=time.monotonic();assets=json.loads(a.assets.read_text());prefix=None
    manifest={'format':'mm2-fresh-fixed-order-chain-v1','scope':'fresh chained search under recovered historical stage order; not unrestricted whole-game search','seed':a.seed,'order':[name for name,_ in STAGES]+['wily4 reach'],'attempts_per_stage':1,'external_gameplay_inputs':False,'limits':vars(a)|{'root':str(a.root),'out':str(a.out),'binary':str(a.binary),'build_info':str(a.build_info),'progress_binary':str(a.progress_binary),'assets':str(a.assets)},'binary_sha256':sha(a.binary),'progress_binary_sha256':sha(a.progress_binary),'stages':[],'status':'running'}
    write(a.out/'chain.json',manifest)
    for index,(name,number) in enumerate(STAGES[:1] if a.qualification else STAGES):
        remaining=int(a.chain_seconds-(time.monotonic()-started))
        if remaining<=180:manifest['status']='chain_wall_limit';break
        cellroot=a.out/f'{index:02}-{name}'
        search={'executions':5000 if a.qualification else a.executions,'frames':a.frames,'actions':4096,'window':2,'result_slots':2,'wall_seconds':min(a.stage_seconds,remaining-120),'selector':a.selector,'suffix':'one_to_six','mixture':a.mixture,'verification':'campaign' if a.qualification else 'witness','mm2_chain':True}
        if a.slot_retention:search['slot_retention']=a.slot_retention
        if prefix is not None:search.update(prefix_input=str(prefix),prefix_sha256=sha(prefix))
        suite={'format':'harmony-search-eval-v1','id':f'chain-{a.seed}-{index}-{name}','seeds':[a.seed],'workers':[4],'memory_mib':[8192],'search':search,'cases':[{'id':f'mm2-{name}','game':'mm2','stage':number,'origin':'fresh fixed-order chain; only prior victories from this chain','rom_sha256':assets['mm2']['sha256'],'search':{}}]}
        suite_path=a.out/f'{index:02}-{name}.json';write(suite_path,suite)
        record={'index':index,'stage':name,'suite_sha256':sha(suite_path),'parent_prefix_sha256':sha(prefix) if prefix else None,'status':'running'}
        manifest['stages'].append(record);write(a.out/'chain.json',manifest)
        command=['python3',str(a.root/'benchmarks/search/eval.py'),'run',str(suite_path),'--assets',str(a.assets),'--binary',str(a.binary),'--build-info',str(a.build_info),'--out',str(cellroot),'--jobs','1','--cpus','4','--memory-capacity-mib','10240','--finish-seconds','120','--disk-limit-gib','4']
        with (a.out/f'{index:02}-{name}.log').open('w') as log:
            result=subprocess.run(command,stdout=log,stderr=subprocess.STDOUT)
        cell=cellroot/f'mm2-{name}-s{a.seed}-w4-m8192';summary_path=cell/'summary.json'
        if summary_path.exists():
            summary=json.loads(summary_path.read_text());record['summary_sha256']=sha(summary_path)
            record['status']=summary['status'];record['result']=summary.get('result');record['peak_rss_bytes']=summary.get('peak_process_tree_rss_bytes_sampled');record['peak_disk_bytes']=summary.get('peak_disk_logical_bytes_sampled');record['elapsed_seconds']=summary.get('elapsed_seconds')
            cost=cell/'campaign/chain-cost.json'
            if cost.exists():record['prefix_cost']=json.loads(cost.read_text())
        else:record['status']='missing_summary'
        record['exit_code']=result.returncode
        if result.returncode or not record.get('result',{}).get('solved'):
            manifest['status']=('infrastructure_error' if result.returncode or record['status']!='complete' else 'qualification_complete' if a.qualification else 'stage_not_solved')
            write(a.out/'chain.json',manifest);break
        prefix=cell/'campaign/next-prefix.json'
        if not prefix.is_file():raise RuntimeError('solved stage did not produce its own fresh continuation')
        record['next_prefix_sha256']=sha(prefix)
        # Qualify every carried prefix twice before the next stage is searched.
        next_stage = STAGES[index+1][0] if index+1 < len(STAGES) else 'wily4'
        replay_path=a.out/f'{index:02}-{name}-bridge.json'
        with replay_path.open('w') as stream:
            bridge=subprocess.run([str(a.progress_binary),'mm2',assets['core']['path'],assets['mm2']['path'],str(prefix),next_stage],stdout=stream,stderr=subprocess.PIPE,timeout=120)
        if bridge.returncode:
            record['bridge_error']=bridge.stderr.decode();manifest['status']='bridge_replay_failed';write(a.out/'chain.json',manifest);break
        replay=json.loads(replay_path.read_text());record['bridge_replay']=replay
        prefix_frames=sum(action['hold_frames'] for action in json.loads(prefix.read_text())['actions'])
        record['bridge_physical_frames']=2*(prefix_frames+replay['result']['setup_frames_after_tape'])
        print(json.dumps({'stage':name,'solved':True,'executions':record['result']['executions'],'frames':record['result']['frames_emulated']}),flush=True)
        write(a.out/'chain.json',manifest)
    else:
        if a.qualification:manifest['status']='qualification_complete'
        else:
            replay=manifest['stages'][-1]['bridge_replay']
            endpoint=replay['result']['endpoint']
            manifest['wily4_replay']=replay
            manifest['status']='wily4_all_weapons' if endpoint['stage']==11 and endpoint['weapons_obtained']==255 and replay['verified_replays']==2 else 'reach_unqualified'
    manifest['elapsed_seconds']=time.monotonic()-started
    manifest['admitted_frames_all_attempts']=sum((r.get('result') or {}).get('frames_emulated',0) for r in manifest['stages'])
    manifest['executions_all_attempts']=sum((r.get('result') or {}).get('executions',0) for r in manifest['stages'])
    manifest['setup_physical_frames_all_attempts']=sum(r.get('prefix_cost',{}).get('new_target_setup_frames',0) for r in manifest['stages'])
    manifest['export_physical_frames_all_attempts']=sum(r.get('prefix_cost',{}).get('physical_export_replay_frames',0)+r.get('prefix_cost',{}).get('award_transition_physical_frames',0) for r in manifest['stages'])
    manifest['bridge_physical_frames_all_attempts']=sum(r.get('bridge_physical_frames',0) for r in manifest['stages'])
    manifest['witness_suffix_physical_frames_all_attempts']=sum(2*((r.get('result') or {}).get('witness') or {}).get('physical_suffix_frames',0) for r in manifest['stages'])
    manifest['accounted_physical_frames_lower_bound']=sum(manifest[k] for k in ('admitted_frames_all_attempts','setup_physical_frames_all_attempts','export_physical_frames_all_attempts','bridge_physical_frames_all_attempts','witness_suffix_physical_frames_all_attempts'))
    manifest['cost_limitations']='Missing/incomplete stage results and full campaign verification have incomplete physical accounting, never zero-cost successes. The lower bound includes admitted search, successful target setup, physical export, bridges, and two witness suffixes; failed target construction and campaign replay work may be missing.'
    write(a.out/'chain.json',manifest);print(json.dumps({'status':manifest['status'],'stages':len(manifest['stages']),'frames':manifest['admitted_frames_all_attempts']}),flush=True)
if __name__=='__main__':main()
