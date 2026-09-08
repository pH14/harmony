#!/usr/bin/env python3
"""Freeze an attested executable using an explicitly owned reusable target cache."""
import argparse, importlib.util, json, os, pathlib, shutil, subprocess
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('root',type=pathlib.Path);p.add_argument('out',type=pathlib.Path);p.add_argument('target',type=pathlib.Path);a=p.parse_args()
spec=importlib.util.spec_from_file_location('evaluation',a.root/'benchmarks/search/eval.py');evaluation=importlib.util.module_from_spec(spec);spec.loader.exec_module(evaluation)
before=evaluation.source_identity(a.root);a.out.mkdir(parents=True,exist_ok=False)
env={**os.environ,'HARMONY_SEARCH_SOURCE_SHA256':before['source_tree_sha256']}
with (a.out/'build.log').open('wb') as log:
 subprocess.run(['cargo','build','--release','--locked','--manifest-path',str(a.root/'workloads/nes/Cargo.toml'),'--bin','nes-eval','--target-dir',str(a.target),'-j','7'],env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
assert before==evaluation.source_identity(a.root),'source changed while compiling'
shutil.copy2(a.target/'release/nes-eval',a.out/'nes-eval')
metadata={'format':'harmony-search-build-v1',**before,'binary_sha256':evaluation.digest(a.out/'nes-eval'),'rustc':subprocess.check_output(['rustc','-Vv'],text=True),'cargo':subprocess.check_output(['cargo','-V'],text=True).strip(),'profile':'release','locked':True,'rustflags':env.get('RUSTFLAGS',''),'target_cache':str(a.target)}
evaluation.write_json(a.out/'build-info.json',metadata);print(json.dumps(metadata),flush=True)
