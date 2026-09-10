#!/usr/bin/env python3
"""Audit saved host records for the frozen development seeds before dispatch."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import socket

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--seeds',required=True)
p.add_argument('--out',type=Path,required=True)
a=p.parse_args(); seeds=[int(s) for s in a.seeds.split(',')]
assert len(seeds)==len(set(seeds))==4
roots=sorted(Path('/root').glob('harmony*')); matches=[];skipped=[];checked=0;digest=hashlib.sha256()
for root in roots:
 if not root.is_dir():continue
 for directory, dirs, files in os.walk(root):
  dirs[:]=[d for d in dirs if d not in {'.git','target','builds','assets','node_modules','__pycache__'} and not d.startswith('source')]
  for name in sorted(files):
   if not name.endswith('.json') or not (name=='summary.json' or 'registration' in name or 'seed' in name):continue
   f=Path(directory)/name
   if f.stat().st_size>8*1024**2:
    skipped.append(str(f));continue
   data=f.read_bytes();checked+=1;digest.update(str(f).encode());digest.update(hashlib.sha256(data).digest())
   found=[seed for seed in seeds if str(seed).encode() in data]
   if found:matches.append({'path':str(f),'seeds':found})
r={'recorded_utc':datetime.now(timezone.utc).isoformat(),'host':socket.gethostname(),'seeds':seeds,'roots':[str(r) for r in roots],'files_checked':checked,'matches':matches,'skipped':skipped,'scanned_path_content_digest':digest.hexdigest(),'scope':'Saved summary, registration and seed JSON files under /root/harmony*, excluding source/build caches and assets; files above8MiB are explicit skips. Unrecorded historical use cannot be certified.'}
a.out.write_text(json.dumps(r,indent=2)+'\n')
print(json.dumps({'host':r['host'],'files_checked':checked,'matches':matches,'skipped':skipped}))
assert not matches and not skipped
