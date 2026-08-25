#!/usr/bin/env python3
"""Cross-arm and cross-core repetition: does the same work count always land in the
same guest state? Reads the per-arm records only; no summary line is consulted."""
import collections, glob, gzip, json, sys

d = sys.argv[1]


def opener(f):
    return gzip.open(f, "rt") if f.endswith(".gz") else open(f)


by_work = collections.defaultdict(set)      # work count -> set of digests
cores_at = collections.defaultdict(set)     # work count -> set of cores
landings = collections.Counter()            # work count -> exact landings seen
files = sorted(glob.glob(d + "/core*.json") + glob.glob(d + "/core*.json.gz"))
for f in files:
    with opener(f) as fh:
        for r in json.load(fh):
            if r.get("kind") != "arm":
                continue
            t, c = r["target"], r["core"]
            if r.get("landed_exact") and not r.get("overshoot"):
                by_work[t].add(r["digest"])
                cores_at[t].add(c)
                landings[t] += 1
            # a replay arm counts only when its digest matched, which is what
            # says it landed in the same state; a mismatch is a failing arm and
            # is counted as a failure elsewhere, never as a landing.
            if r.get("replay") and r.get("replay_match"):
                by_work[t].add(r["replay_digest"])
                cores_at[t].add(c)
                landings[t] += 1

rep = {w: v for w, v in landings.items() if v > 1}
disagree = [w for w in rep if len(by_work[w]) > 1]
multicore = {w: cores_at[w] for w in rep if len(cores_at[w]) > 1}
print("shards:", len(files))
print("distinct work counts landed on at all:      %d" % len(landings))
print("distinct work counts landed on >1 time:     %d" % len(rep))
print("  of those, landed on from >1 core:         %d" % len(multicore))
print("  total landings at a repeated work count:  %d" % sum(rep.values()))
print("  most landings at one work count:          %d" % (max(rep.values()) if rep else 0))
print("work counts with two different landed states: %d" % len(disagree))
if disagree:
    for w in sorted(disagree)[:20]:
        print("   ", w, sorted(by_work[w]))
