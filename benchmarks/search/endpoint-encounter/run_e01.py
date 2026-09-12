#!/usr/bin/env python3
"""Identity checks followed by the single registered standalone observer process."""
import argparse
from datetime import datetime, timezone
import gzip
import hashlib
import json
import os
from pathlib import Path

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--root',type=Path,required=True)
p.add_argument('--protocol',type=Path,required=True)
a=p.parse_args()
r=json.loads((a.protocol/'e01-registration.json').read_text())
assert (datetime.fromisoformat(r['deadline_utc'])-datetime.now(timezone.utc)).total_seconds()>=r['max_wall_seconds']
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
binary=a.root/r['binary']
assert sha(binary)==r['binary_sha256']
assert sha(binary.with_name('build-info.json'))==r['build_info_sha256']
source=a.protocol/r['input']
assert sha(source)==r['input_sha256']
artifact=a.protocol/'first-endpoint-encounter.json'
assert sha(artifact)==r['producing_artifact_sha256']
envelope=json.loads(artifact.read_text())
assert envelope['first']==r['producing_first'] and envelope['input']==json.loads(source.read_text())
raw=gzip.decompress((a.protocol/'c01-results.json.gz').read_bytes())
assert hashlib.sha256(raw).hexdigest()==r['c01_panel_sha256']
summary=json.loads(raw)['records'][0]['summary']
witness=summary['result']['endpoint_encounter_witness']
assert summary['status']=='complete' and witness['verified_replays']==2
assert witness['artifact_sha256']==r['producing_artifact_sha256']
assert witness['first']==r['producing_first']
assert witness['replay']['physical_suffix_frames']==r['expected_route_frames']
assets=json.loads((a.root/'assets.json').read_text())
core,rom=(Path(assets[k]['path']) for k in ('core','metroid'))
assert sha(core)==r['core_sha256'] and sha(rom)==r['rom_sha256']
output=a.root/r['output']
assert not output.exists()
os.execvp('taskset',['taskset','-c',r['cpu'],str(binary),str(core),str(rom),str(source),str(output),*r['arguments']])
