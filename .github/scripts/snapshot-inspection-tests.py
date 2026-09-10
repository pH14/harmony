#!/usr/bin/env python3
"""Exercise the actual inspection comparison from the recovery acceptance script."""
import json
import os
from pathlib import Path
import subprocess
import sys

source = Path(sys.argv[1]).read_text()
start = source.index('inspection_before=$(jq')
end = source.index('&& record "the recorded finding is unchanged"', start)
expression = source[start:end].rstrip().removesuffix('\\').rstrip()
point = {
    'properties': [{'meaning': 'literal [index] * ? values', 'assertion': 2}],
    'virtual_time_nanos': 13091213671,
    'state_hash': '51cc10f001ea4940013f348c2d61520f459054abc648b7cbae80f71ef8b52cec',
}
cases = [('same', point, True)]
for field, value in [('state_hash', '0' * 64), ('virtual_time_nanos', 13091213672),
                     ('properties', [{'meaning': 'changed', 'assertion': 2}])]:
    cases.append((field, {**point, field: value}, False))
for name, after, equal in cases:
    env = {**os.environ, 'inspected': json.dumps(point), 'after': json.dumps(after, indent=2, sort_keys=True)}
    result = subprocess.run(['bash', '-e', '-c', expression], env=env, capture_output=True)
    assert (result.returncode == 0) == equal, (name, result.stderr.decode())
# Failed parsing cannot be mistaken for two equal empty command substitutions.
result = subprocess.run(['bash', '-e', '-c', expression],
                        env={**os.environ, 'inspected': '{', 'after': '{'}, capture_output=True)
assert result.returncode != 0
print('Inspection identity: same output accepted; changed hash, moment, property and malformed JSON rejected')
