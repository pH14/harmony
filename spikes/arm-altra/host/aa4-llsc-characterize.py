#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-4(a)/(b): characterize the LL/SC divergence and the LSE invariance from the AA-3
# exact-landing records. For each payload's (seed, target) tuple, gather the per-rep values and
# report WHAT diverges across reps: the work-clock count (measured_taken / work_end), the landed
# work value, the in-guest self-check status, and the state digests (landed + full state). The
# distinction between a COUNT divergence (breaks the work clock itself) and a STATE-only
# divergence (same count, different architectural state) is the crux of the ruling.
import json
import glob
import sys
from collections import defaultdict


def characterize(records_glob, payload):
    tup = defaultdict(list)
    for f in glob.glob(records_glob):
        with open(f) as fh:
            for line in fh:
                r = json.loads(line)
                if r["payload"] != payload:
                    continue
                ov = r.get("overflow") or {}
                if not ov.get("armed"):
                    continue
                k = (r["seed"], ov["target"])
                tup[k].append(
                    dict(
                        mt=r["measured_taken"],
                        we=r["work_end"],
                        ld=ov["landed_digest"],
                        st=r["state_digest"],
                        ex=r["exit_reason"],
                        ps=r["payload_status"],
                        land=ov["landed"],
                    )
                )
    n = len(tup)
    div_state = div_count = div_landed = div_ps = 0
    ex_all = set()
    sample = None
    for k, v in tup.items():
        mts = {x["mt"] for x in v}
        wes = {x["we"] for x in v}
        lds = {x["ld"] for x in v}
        sts = {x["st"] for x in v}
        lands = {x["land"] for x in v}
        pss = {x["ps"] for x in v}
        for x in v:
            ex_all.add(x["ex"])
        if len(lds) > 1 or len(sts) > 1:
            div_state += 1
            if sample is None:
                sample = (k, v)
        if len(mts) > 1 or len(wes) > 1:
            div_count += 1
        if len(lands) > 1:
            div_landed += 1
        if len(pss) > 1:
            div_ps += 1
    print(f"=== {payload}: {n} tuples ===")
    if n == 0:
        return
    print(f"  diverge in state (landed_digest/state_digest): {div_state} ({100*div_state/n:.1f}%)")
    print(f"  diverge in COUNT (measured_taken/work_end):     {div_count} ({100*div_count/n:.1f}%)")
    print(f"  diverge in landed work value:                   {div_landed}")
    print(f"  diverge in payload_status (in-guest self-check): {div_ps}")
    print(f"  exit_reasons seen: {sorted(ex_all)}")
    if sample:
        k, v = sample
        print(f"  SAMPLE divergent tuple seed={k[0]} target={k[1]}:")
        for x in v:
            print(
                f"    measured_taken={x['mt']} work_end={x['we']} landed={x['land']} "
                f"payload_status={x['ps']} landed_digest={x['ld'][:22]} state_digest={x['st'][:22]}"
            )


if __name__ == "__main__":
    g = sys.argv[1] if len(sys.argv) > 1 else "results/aa-3/exact/*/records.jsonl"
    for p in ("llsc-atomics", "lse-atomics"):
        characterize(g, p)
