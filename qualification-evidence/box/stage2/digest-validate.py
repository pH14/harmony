#!/usr/bin/env python3
"""Check the digest inversion against records that state the answer.

The campaign harness records only the replay arm's digest, so the work count it landed
on had to be recovered by inverting that digest. `ae3-instr` records both the digest and
the work count, so on its records the inversion can be scored rather than trusted.
"""
import glob, json, struct, sys

N = 300000

def fnv(rip, rcx):
    h = 1469598103934665603
    for b in struct.pack("<QQ", rip, rcx & 0xFFFFFFFFFFFFFFFF):
        h ^= b
        h = (h * 1099511628211) & 0xFFFFFFFFFFFFFFFF
    return "%016x" % h

table = {}
for w in range(0, N + 1):
    table[fnv(0x1006, N - w)] = (w, "step")
    table[fnv(0x1008, N - w - 1)] = (w, "overflow")

agree = disagree = missing = 0
examples = []
for d in sys.argv[1:]:
    for f in sorted(glob.glob(d + "/core*.json")) or [d]:
        for line in open(f):
            line = line.strip().rstrip(",")
            if not (line.startswith("{") and line.endswith("}")):
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            if r.get("kind") != "arm" or "replay_work_landed" not in r:
                continue
            hit = table.get(r["replay_digest"])
            if hit is None:
                missing += 1
                continue
            w, stop = hit
            want_stop = "overflow" if r.get("replay_overshoot") else "step"
            if w == r["replay_work_landed"] and stop == want_stop:
                agree += 1
            else:
                disagree += 1
                if len(examples) < 5:
                    examples.append((r["core"], r["idx"], r["replay_work_landed"], w, stop,
                                     want_stop))
print(f"replay arms whose landing the record states: {agree + disagree + missing}")
print(f"  inversion agrees with the record: {agree}")
print(f"  inversion disagrees:              {disagree}")
print(f"  digest not in the dictionary:     {missing}")
for e in examples:
    print(f"  core{e[0]} idx {e[1]}: record says {e[2]} ({e[5]}), inversion says {e[3]} ({e[4]})")
