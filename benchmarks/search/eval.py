#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run, compare and export reproducible local search evaluations."""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import html
import itertools
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import statistics
import math
import subprocess
import sys
import time

SCHEMA = 'harmony-search-eval-v1'
ALLOWED_SEARCH = {'seed','workers','executions','frames','actions','memory_mib','window','wall_seconds','selector','suffix','mixture','verification'}


def valid_id(value):
    return isinstance(value, str) and value not in {'.', '..'} and re.fullmatch(r'[a-zA-Z0-9_][a-zA-Z0-9_.-]*', value)


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''): value.update(block)
    return value.hexdigest()


def read_json(path, default=None):
    try: return json.loads(Path(path).read_text())
    except FileNotFoundError: return default


def write_json(path, value):
    path = Path(path)
    temp = path.with_suffix(path.suffix + '.tmp')
    temp.write_text(json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False) + '\n')
    temp.replace(path)


def source_identity(root):
    root = Path(root)
    manifest = []
    excluded = {'.git','target','build','__pycache__','.agents','.claude','.codex','node_modules'}
    for directory in ['dissonance','workloads','benchmarks/search']:
        for current, dirs, files in os.walk(root / directory):
            dirs[:] = sorted(d for d in dirs if d not in excluded)
            for name in sorted(files):
                path = Path(current) / name
                if not path.is_symlink(): manifest.append((str(path.relative_to(root)),digest(path)))
    for name in ['Cargo.toml','Cargo.lock','rust-toolchain.toml','clippy.toml','.cargo/config.toml']:
        if (root/name).is_file(): manifest.append((name,digest(root/name)))
    h = hashlib.sha256(json.dumps(sorted(manifest),separators=(',',':')).encode()).hexdigest()
    try:
        revision = subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,stderr=subprocess.DEVNULL,text=True).strip()
        dirty = bool(subprocess.check_output(['git','status','--porcelain'],cwd=root,text=True))
    except subprocess.CalledProcessError:
        revision,dirty = None,None
    return {'source_tree_sha256':h,'revision':revision,'dirty':dirty,'files':len(manifest)}


def disk_usage(root):
    logical = allocated = 0
    categories = {}
    for current, dirs, files in os.walk(root,followlinks=False):
        dirs[:] = [d for d in dirs if not (Path(current)/d).is_symlink()]
        for name in files:
            path=Path(current)/name
            try: stat=path.lstat()
            except FileNotFoundError: continue
            if path.is_symlink(): continue
            logical+=stat.st_size
            allocated+=getattr(stat,'st_blocks',0)*512
            category = 'stream' if name=='stream.jsonl' else 'checkpoint' if 'checkpoint' in name else 'media' if path.suffix in {'.mp4','.wav','.png','.rgb'} else 'telemetry' if name in {'resources.jsonl','progress.jsonl','events.jsonl'} else 'reports_and_logs'
            categories[category]=categories.get(category,0)+stat.st_size
    return {'logical_bytes':logical,'allocated_bytes':allocated,'categories':categories}


class ProcessMetrics:
    """Sample one isolated process group; retain exited members' I/O counters."""
    def __init__(self, group):
        self.group=group
        self.io={}

    def sample(self):
        rss=0
        if not Path('/proc').is_dir(): return {'rss_bytes':None,'write_bytes':None,'read_bytes':None}
        for directory in Path('/proc').iterdir():
            if not directory.name.isdigit(): continue
            try:
                # comm may contain spaces and parentheses; fields follow its final ')'.
                fields=(directory/'stat').read_text().rsplit(')',1)[1].split()
                if int(fields[2]) != self.group: continue
                key=(directory.name,fields[19])
                status=dict(line.split(':',1) for line in (directory/'status').read_text().splitlines() if ':' in line)
                rss+=int(status.get('VmRSS','0 kB').split()[0])*1024
                counters=dict(line.split(':',1) for line in (directory/'io').read_text().splitlines())
                self.io[key]=(int(counters['read_bytes']),int(counters['write_bytes']))
            except (FileNotFoundError,ProcessLookupError,PermissionError,ValueError,KeyError): continue
        return {'rss_bytes':rss,'read_bytes':sum(x[0] for x in self.io.values()),'write_bytes':sum(x[1] for x in self.io.values())}


class Tail:
    def __init__(self,path): self.path=Path(path); self.offset=0; self.pending=b''; self.last={}
    def read(self):
        try:
            with self.path.open('rb') as f:
                f.seek(self.offset); block=f.read(); self.offset=f.tell()
        except FileNotFoundError: return self.last
        lines=(self.pending+block).split(b'\n');self.pending=lines.pop()
        for line in lines:
            if line: self.last=json.loads(line)
        return self.last


def expand_suite(suite, selected=None):
    if suite.get('format') != SCHEMA: raise ValueError('unknown suite format')
    if not valid_id(suite['id']): raise ValueError('invalid suite id')
    for axis in ('seeds', 'workers', 'memory_mib'):
        values = suite[axis]
        if not values or any(type(x) is not int for x in values) or len(values) != len(set(values)):
            raise ValueError('empty, invalid or duplicate matrix axis: ' + axis)
    jobs=[]; names=set()
    for case in suite['cases']:
        name=case['id']
        if not valid_id(name) or name in names: raise ValueError('invalid or duplicate case id')
        names.add(name)
        if selected and name not in selected: continue
        settings={**suite['search'],**case.get('search',{})}
        if set(settings)-ALLOWED_SEARCH: raise ValueError('unknown search settings')
        for seed,workers,memory in itertools.product(suite['seeds'],suite['workers'],suite['memory_mib']):
            request={**settings,**{k:case[k] for k in ('game','level','stage','ai','whole_game') if k in case},'seed':seed,'workers':workers,'memory_mib':memory}
            for field in ('seed','workers','memory_mib','executions','actions','window','wall_seconds'):
                val=request[field]
                if type(val) is not int or val<0 or (field!='seed' and val==0): raise ValueError('invalid '+field)
            if request.get('frames') is not None and (type(request['frames']) is not int or request['frames'] <= 0): raise ValueError('invalid frames')
            cell=f'{name}-s{seed}-w{workers}-m{memory}'
            jobs.append({'id':cell,'case':case,'request':request})
    if selected and set(selected)-names: raise ValueError('unknown selected case')
    if not jobs: raise ValueError('empty evaluation matrix')
    return jobs


def resolve_assets(job, assets):
    request=job['request'].copy()
    for name in ('core',request['game']):
        item=assets[name]
        actual=digest(item['path'])
        if actual!=item['sha256']: raise ValueError('asset checksum mismatch: '+name)
        if name!='core' and actual!=job['case']['rom_sha256']: raise ValueError('ROM differs from frozen suite: '+name)
        prefix='core' if name=='core' else 'rom'
        request[prefix]=str(Path(item['path']).resolve());request[prefix+'_sha256']=actual
    return request


def host_identity():
    info={'hostname':platform.node(),'system':platform.system(),'release':platform.release(),'machine':platform.machine(),'logical_cpus':os.cpu_count()}
    if Path('/proc/cpuinfo').exists():
        model=next((x.split(':',1)[1].strip() for x in Path('/proc/cpuinfo').read_text().splitlines() if x.startswith('model name')),None)
        info['cpu_model']=model
    return info


def run_one(job,request,args,cpus,build,host):
    root=args.out/job['id'];root.mkdir()
    write_json(root/'request.private.json',request)
    campaign=root/'campaign'
    command=[str(args.binary),str(root/'request.private.json'),str(campaign)]
    if cpus: command=['taskset','-c',','.join(map(str,cpus)),*command]
    started=time.monotonic(); peak_rss=None;peak_disk=peak_allocated=0; last_frames=0;last_time=0.; last_phase=None
    phase_peaks = {}
    timed_out=False;disk_exceeded=False
    with (root/'stdout.log').open('wb') as stdout,(root/'stderr.log').open('wb') as stderr,(root/'resources.jsonl').open('w') as telemetry:
        environment = {**os.environ, 'HARMONY_COORDINATOR_PROFILE': '1'}
        process=subprocess.Popen(command,stdout=stdout,stderr=stderr,start_new_session=True,env=environment)
        metrics=ProcessMetrics(process.pid);tail=Tail(campaign/'progress.jsonl')
        deadline=request['wall_seconds']+args.finish_seconds
        try:
            while True:
                elapsed=time.monotonic()-started
                current=metrics.sample();disk=disk_usage(root);progress=tail.read()
                stage=read_json(campaign/'phase.json',{}).get('phase','preparation')
                frames=progress.get('frames_emulated',0)
                delta=elapsed-last_time
                sample={'elapsed_seconds':elapsed,'phase':stage,**current,'disk':disk,'executions':progress.get('executions',0),'frames_emulated':frames,'interval_frames_per_second':max(0,frames-last_frames)/delta if delta>0 and stage=='search' and last_phase=='search' else None,'search':progress}
                telemetry.write(json.dumps(sample,separators=(',',':'),allow_nan=False)+'\n');telemetry.flush()
                if current['rss_bytes'] is not None:
                    peak_rss=max(peak_rss or 0,current['rss_bytes'])
                    phase_peaks[stage] = max(phase_peaks.get(stage) or 0, current['rss_bytes'])
                else:
                    phase_peaks.setdefault(stage, None)
                peak_disk=max(peak_disk,disk['logical_bytes']);peak_allocated=max(peak_allocated,disk['allocated_bytes'])
                last_frames=frames;last_time=elapsed;last_phase=stage
                pid,status,usage=os.wait4(process.pid,os.WNOHANG)
                if pid:
                    process.returncode=os.waitstatus_to_exitcode(status)
                    break
                if elapsed>deadline or disk['logical_bytes']>args.disk_limit_gib*1024**3:
                    timed_out=elapsed>deadline;disk_exceeded=not timed_out
                    os.killpg(process.pid,signal.SIGKILL)
                    _,status,usage=os.wait4(process.pid,0);process.returncode=os.waitstatus_to_exitcode(status)
                    break
                time.sleep(args.sample_seconds)
        except BaseException:
            if process.returncode is None:
                os.killpg(process.pid,signal.SIGKILL);_,status,usage=os.wait4(process.pid,0);process.returncode=os.waitstatus_to_exitcode(status)
            raise
    result=read_json(campaign/'result.json')
    status='timeout' if timed_out else 'disk_limit' if disk_exceeded else 'complete' if process.returncode==0 and result else 'error'
    if status=='complete' and job['case'].get('require_solved',False) and not result['solved']: status='regression'
    rss_scale=1 if platform.system()=='Darwin' else 1024
    summary={'format':SCHEMA,'suite':args.suite_id,'suite_sha256':args.suite_sha256,'cell':job['id'],'case':job['case']['id'],'origin':job['case']['origin'],'status':status,'exit_code':process.returncode,'host':host,'cpu_set':cpus,'build':build,'search_request':{k:v for k,v in request.items() if k not in {'rom','core'}},'identity':read_json(campaign/'identity.json'),'result':result,'elapsed_seconds':time.monotonic()-started,'peak_process_tree_rss_bytes_sampled':peak_rss,'max_process_rss_bytes':int(usage.ru_maxrss)*rss_scale,'cpu_seconds':usage.ru_utime+usage.ru_stime,'peak_disk_logical_bytes_sampled':peak_disk,'peak_disk_allocated_bytes_sampled':peak_allocated,'final_disk':disk_usage(root),'io_bytes_last_sample':current,'last_progress':tail.read()}
    summary['rss_by_phase_bytes_sampled'] = phase_peaks
    summary['rusage'] = {'input_block_operations': usage.ru_inblock, 'output_block_operations': usage.ru_oublock,
                         'voluntary_context_switches': usage.ru_nvcsw, 'involuntary_context_switches': usage.ru_nivcsw}
    # The final disk footprint includes this report. Settle its decimal field widths.
    for _ in range(4):
        write_json(root/'summary.json',summary)
        final = disk_usage(root)
        if summary['final_disk'] == final: break
        summary['final_disk'] = final
        summary['peak_disk_logical_bytes_sampled'] = max(peak_disk, final['logical_bytes'])
        summary['peak_disk_allocated_bytes_sampled'] = max(peak_allocated, final['allocated_bytes'])
    print(json.dumps({'cell':job['id'],'status':status,'solved':result['solved'] if result else None,'seconds':round(summary['elapsed_seconds'],2)}),flush=True)
    return summary


def run_matrix(args):
    suite=read_json(args.suite);jobs=expand_suite(suite,args.case)
    args.suite_id=suite['id'];args.suite_sha256=digest(args.suite)
    args.out=args.out.resolve();args.binary=args.binary.resolve()
    if args.out.exists(): raise ValueError('matrix output must be a new directory')
    assets=read_json(args.assets)
    resolved=[(job,resolve_assets(job,assets)) for job in jobs]
    if args.sample_seconds<=0 or args.finish_seconds<=0 or args.jobs<=0 or args.disk_limit_gib<=0 or (args.cpus is not None and args.cpus<=0) or args.overhead_mib<0: raise ValueError('runner limits must be positive')
    cpus=sorted(os.sched_getaffinity(0)) if hasattr(os,'sched_getaffinity') else []
    if args.cpus: cpus=cpus[:args.cpus]
    capacity=len(cpus) if cpus else args.cpus or os.cpu_count()
    for job,_ in resolved:
        if job['request']['workers']>capacity or job['request']['memory_mib']+args.overhead_mib>args.memory_capacity_mib: raise ValueError('cell exceeds CPU or memory allocation')
    args.out.mkdir(parents=True)
    supplied_build = read_json(args.build_info) if args.build_info else None
    binary_hash = digest(args.binary)
    if supplied_build and supplied_build.get('binary_sha256', binary_hash) != binary_hash:
        raise ValueError('build identity does not match the executable')
    build={'binary_sha256':binary_hash,'source':supplied_build,'attestation':'supplied' if supplied_build else 'unavailable'}
    host=host_identity();write_json(args.out/'suite.json',suite);write_json(args.out/'matrix.json',{'format':SCHEMA,'build':build,'host':host,'cells':[j['id'] for j,_ in resolved],
        'runner':{'jobs':args.jobs,'cpu_capacity':capacity,'memory_capacity_mib':args.memory_capacity_mib,'overhead_mib':args.overhead_mib,'sample_seconds':args.sample_seconds,'disk_limit_gib':args.disk_limit_gib,'finish_seconds':args.finish_seconds}})
    available=cpus.copy();memory=args.memory_capacity_mib;running={};results=[]
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        while resolved or running:
            for job,request in resolved[:]:
                workers=request['workers'];charge=request['memory_mib']+args.overhead_mib
                if len(running)>=args.jobs or charge>memory or (cpus and workers>len(available)):continue
                allocation=available[:workers] if cpus else []
                if cpus: del available[:workers]
                memory-=charge;resolved.remove((job,request))
                future=pool.submit(run_one,job,request,args,allocation,build,host);running[future]=(allocation,charge,job)
            done,_=concurrent.futures.wait(running,return_when=concurrent.futures.FIRST_COMPLETED)
            for future in done:
                allocation,charge,job=running.pop(future);available=sorted(available+allocation);memory+=charge
                try:
                    results.append(future.result())
                except Exception as error:
                    failure = {'format':SCHEMA,'cell':job['id'],'case':job['case']['id'],'origin':job['case']['origin'],
                               'status':'infrastructure_error','error':str(error),'result':None,'identity':None,
                               'search_request':job['request'],'host':host,'build':build,'last_progress':{}}
                    failure_root = args.out/job['id']
                    failure_root.mkdir(exist_ok=True)
                    write_json(failure_root/'summary.json',failure)
                    results.append(failure)
                # Completed cells survive later failures or an interrupted matrix.
                write_json(args.out/'results.json',sorted(results,key=lambda x:x['cell']))
    write_json(args.out/'results.json',sorted(results,key=lambda x:x['cell']))
    return 0 if all(x['status']=='complete' for x in results) else 1


def load_results(matrix, require_complete=True):
    results = read_json(matrix / 'results.json')
    if not isinstance(results, list):
        raise ValueError('matrix has no completed results index')
    ids = [item['cell'] for item in results]
    if any(not valid_id(cell) for cell in ids) or len(ids) != len(set(ids)):
        raise ValueError('invalid or duplicate result cell')
    registered = read_json(matrix / 'matrix.json')['cells']
    if require_complete and set(ids) != set(registered):
        raise ValueError('matrix is incomplete; keep failed cells, do not silently drop them')
    return results


def solve_interval(solved, total):
    """Wilson 95% interval; small panels deliberately produce wide intervals."""
    if not total:
        return None
    z = 1.959963984540054
    p = solved / total
    denominator = 1 + z * z / total
    middle = (p + z * z / (2 * total)) / denominator
    radius = z * math.sqrt(p * (1 - p) / total + z * z / (4 * total * total)) / denominator
    # Wilson's endpoints are exactly 0/1 at the boundary. Floating point
    # cancellation can otherwise put the observed fraction just outside its
    # interval, producing a negative error-bar length in publication plots.
    return [0. if solved == 0 else max(0., middle - radius),
            1. if solved == total else min(1., middle + radius)]


def aggregates(results):
    groups = {}
    for item in results:
        request = item.get('search_request', {})
        key = (item['case'], request.get('workers'), request.get('memory_mib'))
        groups.setdefault(key, []).append(item)
    rows = []
    for key, items in sorted(groups.items(), key=lambda x: str(x[0])):
        valid = [x for x in items if x['status'] in {'complete', 'regression'} and x.get('result')]
        solved = [x for x in valid if x['result']['solved']]
        rows.append({'case': key[0], 'workers': key[1], 'memory_mib': key[2], 'trials': len(items),
                     'valid_trials': len(valid), 'errors': len(items) - len(valid), 'solved': len(solved),
                     'solve_fraction_of_valid': len(solved) / len(valid) if valid else None,
                     'solve_fraction_wilson95': solve_interval(len(solved), len(valid)),
                     'median_frames_to_victory_among_successes': statistics.median([x['result']['frames_to_first_victory'] for x in solved]) if solved else None,
                     'median_search_frames_per_second': statistics.median([x['result']['frames_per_second'] for x in valid]) if valid else None})
    return rows


def compare(base, candidate):
    left = {x['cell']: x for x in load_results(base)}
    right = {x['cell']: x for x in load_results(candidate)}
    left_runner = read_json(base / 'matrix.json').get('runner')
    right_runner = read_json(candidate / 'matrix.json').get('runner')
    if left.keys() != right.keys():
        raise ValueError('different registered cells; comparisons require matching suites')
    rows = []
    for cell in sorted(left):
        a, b = left[cell], right[cell]
        if a['origin'] != b['origin']:
            raise ValueError('origin mismatch: ' + cell)
        if not a.get('identity') or not b.get('identity'):
            rows.append({'cell': cell, 'comparable': False, 'baseline_status': a['status'], 'candidate_status': b['status']})
            continue
        if a['identity']['policies'] != b['identity']['policies']:
            raise ValueError('adapter policy changed: ' + cell)
        for key in ('game', 'level', 'stage', 'ai', 'whole_game', 'seed', 'workers', 'memory_mib', 'window', 'actions', 'executions', 'frames', 'wall_seconds', 'rom_sha256', 'core_sha256', 'verification'):
            if a['search_request'].get(key) != b['search_request'].get(key):
                raise ValueError('comparison changed ' + key + ': ' + cell)
        row = {'cell': cell, 'comparable': True, 'baseline_status': a['status'], 'candidate_status': b['status'],
               'same_host': a['host'] == b['host'],
               'same_cpu_set': bool(a.get('cpu_set')) and a.get('cpu_set') == b.get('cpu_set'),
               'same_runner_configuration': left_runner is not None and left_runner == right_runner}
        row['timing_environment_matches'] = all(row[key] for key in
            ('same_host', 'same_cpu_set', 'same_runner_configuration'))
        for label, value in [('baseline', a), ('candidate', b)]:
            r = value['result'] or {}
            progress = value.get('last_progress', {})
            row[label] = {'solved': r.get('solved'), 'frames_to_victory': r.get('frames_to_first_victory'),
                          'frames': r.get('frames_emulated', progress.get('frames_emulated')),
                          'search_seconds': r.get('search_seconds'), 'frames_per_second': r.get('frames_per_second'),
                          'peak_rss': value.get('max_process_rss_bytes'), 'peak_disk': value.get('peak_disk_logical_bytes_sampled'),
                          'progress': r.get('progress', progress.get('progress')),
                          'workload_diagnostics': progress.get('workload_diagnostics')}
        rows.append(row)
    return {'format': 'harmony-search-comparison-v1', 'pairs': rows,
            'timing_note': 'Matching recorded allocation does not establish host isolation. Concurrent jobs may share physical cores and finish at different times; use admitted frame cost for search quality and isolated runs for precise throughput claims.',
            'baseline': aggregates(list(left.values())), 'candidate': aggregates(list(right.values()))}


def report_html(results, title):
    def number(value):
        return 'unavailable' if value is None else f'{value:,.1f}' if isinstance(value, float) else f'{value:,}'
    rows = []
    for item in results:
        r = item.get('result') or {}
        cell = html.escape(item['cell'])
        rows.append('<tr>' + ''.join('<td>' + str(x) + '</td>' for x in [
            f'<a href="{cell}/summary.json">{cell}</a>', html.escape(item['status']),
            'yes' if r.get('solved') else 'no' if r else 'unavailable', number(r.get('frames_to_first_victory')),
            number(r.get('frames_emulated')), number(r.get('frames_per_second')), number(r.get('search_seconds')),
            number(item.get('max_process_rss_bytes')), number(item.get('peak_disk_logical_bytes_sampled'))]) + '</tr>')
    panels = ''.join('<li>' + html.escape(json.dumps(row, sort_keys=True)) + '</li>' for row in aggregates(results))
    return '''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>''' + html.escape(title) + '''</title><style>body{font:15px/1.55 system-ui;margin:2rem;color:#162132;background:#f7f9fc}table{border-collapse:collapse;background:white;white-space:nowrap}th,td{padding:.55rem;text-align:right;border-bottom:1px solid #dce3ef}th:first-child,td:first-child{text-align:left}a{color:#1356a0}li{margin:.8rem 0;overflow-wrap:anywhere}.scroll{overflow:auto}h1{font-size:1.6rem}</style>
<h1>''' + html.escape(title) + '''</h1><p>Fresh search runs with frozen workload policies. Independent stage and level fixtures are separate from whole-game completion. Missing victories are censored at the recorded budget; failures remain visible.</p>
<p>Frames include admitted emulator work and replay/probes inside search. Throughput excludes witness verification and external export. Peak RSS is the operating system's process maximum; disk peaks are sampled. See each summary for phase measurements, process-group RSS, logical archive memory, I/O and provenance.</p>
<div class="scroll"><table><thead><tr><th>Cell</th><th>Status</th><th>Solved</th><th>Frames to victory</th><th>Total frames</th><th>Frames/s</th><th>Search s</th><th>RSS bytes</th><th>Disk bytes</th></tr></thead><tbody>''' + ''.join(rows) + '''</tbody></table></div>
<h2>Seed panels</h2><p>Wilson 95% intervals describe uncertainty in solve fractions. Time-to-victory medians include successes only and are not estimates for censored runs. Three-seed pilots are exploratory.</p><ul>''' + panels + '''</ul><p><a href="results.json">Results JSON</a> · <a href="suite.json">Frozen matrix</a> · <a href="matrix.json">Build and host</a> · <a href="checksums.json">SHA-256 manifest</a></p></html>'''


def export(matrix, out):
    if out.exists():
        raise ValueError('public export directory must be new')
    results = load_results(matrix)
    sources = []
    # Explicit allowlist. Verify every path before creating a partial public export.
    for item in results:
        for name in ('summary.json', 'resources.jsonl', 'campaign/identity.json', 'campaign/result.json',
                     'campaign/campaign.json', 'campaign/progress.jsonl', 'campaign/witness-input.json', 'campaign/victory-input.json'):
            relative = Path(item['cell']) / name
            source = matrix / relative
            if source.resolve() != matrix.resolve() / relative:
                raise ValueError('export refuses symlinks: ' + str(relative))
            if source.is_file():
                sources.append((source, relative))
    for name in ('results.json', 'suite.json', 'matrix.json'):
        source = matrix / name
        if source.resolve() != matrix.resolve() / name:
            raise ValueError('export refuses symlinks: ' + name)
        sources.append((source, Path(name)))
    out.mkdir(parents=True)
    for source, relative in sources:
        destination = out / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
    (out / 'index.html').write_text(report_html(results, read_json(matrix / 'suite.json')['id']))
    write_json(out / 'aggregates.json', aggregates(results))
    checksums = {str(p.relative_to(out)): digest(p) for p in sorted(out.rglob('*')) if p.is_file()}
    write_json(out / 'checksums.json', checksums)


def build_binary(args):
    """Compile once into an immutable run bundle and attest the exact source tree."""
    root = args.root.resolve()
    if args.out.exists():
        raise ValueError('build output must be a new directory')
    before = source_identity(root)
    args.out.mkdir(parents=True)
    environment = {**os.environ, 'HARMONY_SEARCH_SOURCE_SHA256': before['source_tree_sha256']}
    command = ['cargo', 'build', '--release', '--locked', '--manifest-path', str(root / 'workloads/nes/Cargo.toml'),
               '--bin', 'nes-eval', '--target-dir', str(args.out.resolve() / 'target'), '-j', str(args.jobs)]
    with (args.out / 'build.log').open('wb') as log:
        subprocess.run(command, cwd=root, env=environment, stdout=log, stderr=subprocess.STDOUT, check=True)
    after = source_identity(root)
    if before != after:
        raise ValueError('source changed during compilation; discard this build and retry')
    binary = args.out / 'nes-eval'
    shutil.copy2(args.out / 'target/release/nes-eval', binary)
    metadata = {'format': 'harmony-search-build-v1', **before, 'binary_sha256': digest(binary),
                'rustc': subprocess.check_output(['rustc', '-Vv'], text=True),
                'cargo': subprocess.check_output(['cargo', '-V'], text=True).strip(),
                'profile': 'release', 'locked': True, 'rustflags': environment.get('RUSTFLAGS', '')}
    write_json(args.out / 'build-info.json', metadata)
    print(json.dumps(metadata), flush=True)


def main():
    parser=argparse.ArgumentParser(description=__doc__);subs=parser.add_subparsers(dest='command',required=True)
    run=subs.add_parser('run');run.add_argument('suite',type=Path);run.add_argument('--assets',type=Path,required=True);run.add_argument('--binary',type=Path,required=True);run.add_argument('--out',type=Path,required=True);run.add_argument('--build-info',type=Path);run.add_argument('--case',action='append');run.add_argument('--jobs',type=int,default=1);run.add_argument('--cpus',type=int);run.add_argument('--memory-capacity-mib',type=int,default=8192);run.add_argument('--overhead-mib',type=int,default=1024);run.add_argument('--sample-seconds',type=float,default=1);run.add_argument('--finish-seconds',type=int,default=600);run.add_argument('--disk-limit-gib',type=float,default=8)
    cmp=subs.add_parser('compare');cmp.add_argument('baseline',type=Path);cmp.add_argument('candidate',type=Path);cmp.add_argument('--out',type=Path,required=True)
    exp=subs.add_parser('export');exp.add_argument('matrix',type=Path);exp.add_argument('--out',type=Path,required=True)
    build=subs.add_parser('build');build.add_argument('--root',type=Path,default=Path(__file__).resolve().parents[2]);build.add_argument('--out',type=Path,required=True);build.add_argument('--jobs',type=int,default=os.cpu_count())
    identity=subs.add_parser('source');identity.add_argument('root',type=Path);identity.add_argument('--out',type=Path,required=True)
    args=parser.parse_args()
    try:
        if args.command=='run':return run_matrix(args)
        if args.command=='build':build_binary(args)
        if args.command=='compare':write_json(args.out,compare(args.baseline,args.candidate))
        if args.command=='export':export(args.matrix,args.out)
        if args.command=='source':write_json(args.out,source_identity(args.root))
    except (ValueError,KeyError,OSError,subprocess.CalledProcessError) as error:
        parser.exit(2,str(error)+'\n')
    return 0

if __name__=='__main__':raise SystemExit(main())
