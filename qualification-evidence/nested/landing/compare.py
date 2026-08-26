#!/usr/bin/env python3
"""Compare two ae3-forceexit records arm by arm: same seed, same targets, so the
landed state digest must agree if virtualization does not change where a target
lands."""
import json, sys

def load(path):
    out = {}
    for line in open(path):
        line = line.strip().rstrip(',')
        if not line.startswith('{'): continue
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if r.get('kind') == 'arm':
            out[r['idx']] = r
    return out

a, b = load(sys.argv[1]), load(sys.argv[2])
common = sorted(set(a) & set(b))
target_mismatch = [i for i in common if a[i]['target'] != b[i]['target']]
digest_mismatch = [i for i in common if a[i]['digest'] != b[i]['digest']]
print(f"arms compared: {len(common)}  (A {len(a)}, B {len(b)})")
print(f"targets differ: {len(target_mismatch)}")
print(f"landed digests differ: {len(digest_mismatch)}")
for i in digest_mismatch[:5]:
    print(f"  idx {i} target {a[i]['target']}: {a[i]['digest']} vs {b[i]['digest']}")
sa = [a[i]['skid'] for i in common if a[i]['preempt_exit']]
sb = [b[i]['skid'] for i in common if b[i]['preempt_exit']]
if sa and sb:
    print(f"skid median  A {sorted(sa)[len(sa)//2]}   B {sorted(sb)[len(sb)//2]}")
    print(f"skid max     A {max(sa)}   B {max(sb)}")
