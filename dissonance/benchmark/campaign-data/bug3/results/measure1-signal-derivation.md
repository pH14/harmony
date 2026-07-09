<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Measure-1 derivation — bug 3, signal config (the ρ = −0.671 figure)

Committed so the report's `ρ = −0.671` is **reproducible from the committed per-seed JSONs**
(PR#90 round-1 finding). Method matches `benchmark::report` (measure 1): over the **signal
finders only**, Spearman rank correlation (average-rank tie handling) between *distinct cells
discovered by branch 256* (from each `b3-signal-<seed>.json` `events`) and *time-to-bug*
(the certified find branch, from `finds.log`).

| seed | cells@256 | TTB (find branch) |
|---|---|---|
| 1  | 4 | 80  |
| 2  | 4 | 159 |
| 6  | 4 | 31  |
| 8  | 4 | 16  |
| 9  | 4 | 227 |
| 10 | 3 | 311 |
| 11 | 4 | 1   |
| 15 | 3 | 287 |
| 16 | 4 | 11  |
| 17 | 4 | 194 |
| 18 | 4 | 235 |

- n = 11 finders (< the Klees 20-trial floor → underpowered; the report marks `correlates? ❌`).
- **Spearman ρ (cells@256 vs TTB) = −0.6708** → rounds to the report's −0.671.

## What the derivation reveals — the ρ is degenerate

`cells@256` takes only **two distinct values (3 or 4)**. The negative ρ is produced *entirely*
by the two seeds that made only 3 cells (10, 15) happening to be the two slowest to find the bug
(TTB 311, 287); the nine 4-cell seeds span the whole TTB range (1 … 235). So this is not a
graded novelty↔progress relationship — it is a 2-point artifact of a tiny (≤4-cell) template
vocabulary. It is reported as a *positive nuance* only, and does not survive as evidence that the
signal beats baseline (it does not — see the report's find-rate and censored-median numbers).

## Reproduce

```sh
# from dissonance/benchmark/campaign-data
python3 - <<'PY'
import json, glob, re, statistics
budget = 256
finds = {}
for line in open('bug3/results/finds.log'):
    m = re.match(r'b3-signal-(\d+) .*branch (\d+)', line)
    if m: finds[int(m.group(1))] = int(m.group(2))
pairs = []
for f in sorted(glob.glob('bug3/results/b3-signal-*.json')):
    if '-solo' in f: continue
    d = json.load(open(f)); s = d['seed']
    if s not in finds: continue
    cells = set()
    for e in d['events']:
        if e['branch'] > budget: break
        cells.update(e['touched'])
    pairs.append((s, len(cells), finds[s]))
def rank(v):
    o = sorted(range(len(v)), key=lambda i: v[i]); r=[0]*len(v); i=0
    while i < len(v):
        j=i
        while j+1<len(v) and v[o[j+1]]==v[o[i]]: j+=1
        for k in range(i,j+1): r[o[k]]=(i+j)/2+1
        i=j+1
    return r
c=[c for _,c,_ in pairs]; t=[t for _,_,t in pairs]
rc, rt = rank(c), rank(t); n=len(pairs)
mc, mt = sum(rc)/n, sum(rt)/n
cov=sum((rc[i]-mc)*(rt[i]-mt) for i in range(n))
sc=sum((rc[i]-mc)**2 for i in range(n))**.5; st=sum((rt[i]-mt)**2 for i in range(n))**.5
print(f'n={n}  rho={cov/(sc*st):.4f}')   # -> n=11  rho=-0.6708
PY
```
