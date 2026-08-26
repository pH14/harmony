#!/usr/bin/env python3
"""Summarise an ae3-forceexit record: landing outcome and the skid distribution."""
import json, sys, statistics

def pct(xs, p):
    if not xs: return None
    k = (len(xs) - 1) * p / 100.0
    lo = int(k)
    hi = min(lo + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)

for path in sys.argv[1:]:
    arms, end = [], None
    for line in open(path):
        line = line.strip().rstrip(',')
        if not line.startswith('{'): continue
        r = json.loads(line)
        (arms if r.get('kind') == 'arm' else [] if r.get('kind') != 'end' else []).append(r) if r.get('kind') == 'arm' else None
        if r.get('kind') == 'end': end = r
    skids = sorted(a['skid'] for a in arms if a['preempt_exit'])
    exact = sum(1 for a in arms if a['landed_exact'])
    over = sum(1 for a in arms if a['overshoot'])
    rmatch = sum(1 for a in arms if a.get('replay') and a.get('replay_match'))
    rtot = sum(1 for a in arms if a.get('replay'))
    below = sum(1 for a in arms if not a['preempt_exit'])
    print(f"== {path}")
    print(f"   arms {len(arms)}  landed exactly {exact}  overshoot {over}  "
          f"below margin (no overflow) {below}")
    if rtot:
        print(f"   replayed {rtot}  digests identical {rmatch}")
    if skids:
        print(f"   skid over {len(skids)} overflow arms: min {skids[0]}  "
              f"p50 {pct(skids,50):.0f}  p99 {pct(skids,99):.0f}  "
              f"p99.9 {pct(skids,99.9):.0f}  max {skids[-1]}  "
              f"stdev {statistics.pstdev(skids):.0f}")
    if end:
        print(f"   harness verdict rc={end['rc']}")
