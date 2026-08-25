#!/usr/bin/env python3
"""Is the landed digest a pure function of the target, and which arm was wrong?

The harness claims the landed state is a pure function of `target` for its fixed
payload. With targets drawn uniformly on [1,100000] over hundreds of thousands of arms,
most target values recur, so the claim is testable against the records themselves. Where
it holds, a disagreeing arm can be attributed: the arm whose digest differs from every
other arm that landed on the same target is the one that went wrong.
"""
import collections, glob, gzip, json, sys

def shards(d):
    """Shard records, plain or gzipped: they are gzipped for the mirror."""
    return sorted(glob.glob(d + "/core*.json")
                  + glob.glob(d + "/core*.json.gz"))


def opener(f):
    return gzip.open(f, "rt") if f.endswith(".gz") else open(f)

d = sys.argv[1]
by_target = collections.defaultdict(collections.Counter)
rows = collections.defaultdict(list)
fails = []

for f in shards(d):
    for line in opener(f):
        line = line.strip().rstrip(",")
        if not (line.startswith("{") and line.endswith("}")):
            continue
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("kind") != "arm":
            continue
        t = r["target"]
        # Only digests taken at an exact landing are evidence about the target.
        if r["landed_exact"] and r["work_landed"] == t:
            by_target[t][r["digest"]] += 1
            rows[t].append((f.split("/")[-1], r["idx"], "first", r["digest"]))
        if r.get("replay") and r["replay_match"]:
            by_target[t][r["replay_digest"]] += 1
        if not r["ok"]:
            fails.append((f.split("/")[-1], r))

recurring = {t: c for t, c in by_target.items() if sum(c.values()) > 1}
disagreeing = {t: c for t, c in recurring.items() if len(c) > 1}
print(f"targets landed on more than once: {len(recurring)}")
print(f"of those, targets with more than one distinct digest: {len(disagreeing)}")
for t, c in list(disagreeing.items())[:10]:
    print(f"  target {t}: {dict(c)}")

print(f"\nfailing arms: {len(fails)}")
for fn, r in fails:
    t = r["target"]
    votes = by_target.get(t, collections.Counter())
    consensus = votes.most_common(1)[0] if votes else None
    print(f"\n{fn} idx {r['idx']} target {t} skid {r['skid']} "
          f"landed {r['work_landed']} exact {r['landed_exact']} overshoot {r['overshoot']}")
    print(f"  first-arm digest  {r['digest']}")
    print(f"  replay digest     {r.get('replay_digest')}")
    print(f"  digests other arms produced for this target: {dict(votes)}")
    if consensus:
        who = []
        if r["digest"] == consensus[0]:
            who.append("the first arm agrees with the consensus")
        elif r["landed_exact"]:
            who.append("THE FIRST ARM DISAGREES WITH THE CONSENSUS")
        if r.get("replay_digest") == consensus[0]:
            who.append("the replay agrees with the consensus")
        else:
            who.append("the replay disagrees with the consensus")
        print("  " + "; ".join(who))
