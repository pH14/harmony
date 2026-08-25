#!/usr/bin/env python3
"""One pass over an ae3 shard set, recomputing every number the verdict needs.

Nothing here reads a summary line: totals, rates and percentiles all come from the
per-arm records. Fields the instrumented harness adds (tsc_to_preempt, smi_delta,
retry_*) are used when present and skipped when not.
"""
import collections, glob, json, statistics, sys


def records(d):
    for f in sorted(glob.glob(d + "/core*.json")):
        n = 0
        for line in open(f):
            line = line.strip().rstrip(",")
            if not (line.startswith("{") and line.endswith("}")):
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            if r.get("kind") == "arm":
                n += 1
                yield f, r
        print(f"  {f.split('/')[-1]}: {n} arms", file=sys.stderr)


def pct(xs, q):
    if not xs:
        return None
    i = min(len(xs) - 1, int(len(xs) * q))
    return xs[i]


d = sys.argv[1]
arms = landings = exact = overshoot = mism = preempt = ovf_arms = 0
skids, tscs, ratios = [], [], []
by_core = collections.Counter()
fail_by_core = collections.Counter()
fails = []
retry_attempted = retry_landed = inferred = 0
replay_overshoot = digest_diverged = replay_inexact = 0
smi_arms = 0
seen_tsc = seen_smi = seen_retry = False
idx_by_file = collections.defaultdict(list)
all_targets = []
any_replay = False

for f, r in records(d):
    arms += 1
    landings += 1 + (1 if r.get("replay") else 0)
    by_core[r["core"]] += 1
    all_targets.append(r["target"])
    any_replay = any_replay or bool(r.get("replay"))
    idx_by_file[f].append(r["idx"])
    if r["period"]:
        ovf_arms += 1
        skids.append(r["skid"])
        if r.get("replay") and "replay_skid" in r and r.get("replay_preempt_exit"):
            skids.append(r["replay_skid"])
    if r["preempt_exit"]:
        preempt += 1
    if r.get("replay"):
        if "replay_preempt_exit" in r:
            # The instrumented harness records the replay arm's own exit reason.
            if r["replay_preempt_exit"]:
                preempt += 1
        elif r["period"] and r["ok"]:
            # The original harness does not record it, but its control flow settles
            # it: an arm whose exit reason was not KVM_EXIT_PREEMPT returns before
            # landing, so its digest cannot match, so the arm cannot be ok. A passing
            # replayed arm with a non-zero period therefore took the exit as well.
            preempt += 1
            inferred += 1
    if r["work_landed"] == r["target"] and r["landed_exact"]:
        exact += 1
    if r["overshoot"]:
        overshoot += 1
    if r.get("replay") and not r["replay_match"]:
        if r.get("replay_overshoot"):
            replay_overshoot += 1
        elif "replay_landed_exact" in r:
            # The instrumented harness says whether the replay landed at all. A replay
            # that landed exactly and still hashed differently is a divergence of the
            # guest's architectural state at the same work count, which is a different
            # and far more serious thing than an overshoot.
            if r["replay_landed_exact"]:
                digest_diverged += 1
            else:
                replay_inexact += 1
        elif not r["overshoot"]:
            mism += 1
    if not r["ok"]:
        fails.append(r)
        fail_by_core[r["core"]] += 1
    if "tsc_to_preempt" in r:
        seen_tsc = True
        if r["period"] and r["tsc_to_preempt"]:
            tscs.append(r["tsc_to_preempt"])
            ratios.append(r["work_at_preempt"] / r["tsc_to_preempt"])
    if "smi_delta" in r:
        seen_smi = True
        if r["smi_delta"]:
            smi_arms += 1
    if "retry_attempts" in r:
        seen_retry = True
        if r["retry_attempts"]:
            retry_attempted += 1
            if r["retry_landed"]:
                retry_landed += 1

skids.sort()
print(f"\narms={arms} landings={landings} overflow_arms={ovf_arms}")
print(f"landings through the deterministic exit: {preempt} "
      f"({preempt - inferred} attested in the record, {inferred} inferred from a passing "
      f"replay of an armed target)")
print(f"first-arm exact landings: {exact} of {arms}")
# Only an arm that armed an overflow can overshoot, so that is the denominator.
exposed = ovf_arms * (2 if any_replay else 1)
print(f"failing arms: {len(fails)} of {arms}")
print(f"  overflow arms exposed to overshoot: {exposed}")
print(f"  first-arm overshoot: {overshoot}"
      + (f"  (1 in {exposed // overshoot})" if overshoot else ""))
print(f"  replay-arm overshoot, recorded as such: {replay_overshoot}"
      + (f"  (1 in {exposed // replay_overshoot})" if replay_overshoot else ""))
print(f"  replay landed inexactly for another reason: {replay_inexact}")
print(f"  replay landed exactly and the digest still differed: {digest_diverged}")
print(f"  replay mismatch whose cause this harness does not record: {mism}"
      + (f"  (1 in {arms // mism} targets)" if mism else ""))
tot_over = overshoot + replay_overshoot
if tot_over:
    print(f"  all recorded overshoots: {tot_over} in {exposed} exposed arms "
          f"(1 in {exposed // tot_over})")
print(f"contiguous index runs: "
      f"{all(v == list(range(len(v))) for v in idx_by_file.values())}")
if skids:
    print(f"\nguest-mode skid, n={len(skids)} "
          f"(every arm whose own skid the record carries)")
    for q, name in ((0.5, "p50"), (0.9, "p90"), (0.99, "p99"), (0.999, "p99.9"),
                    (0.9999, "p99.99")):
        print(f"  {name:>7} {pct(skids, q)}")
    print(f"  {'max':>7} {skids[-1]}   mean {statistics.mean(skids):.1f}   min {skids[0]}")
    over_margin = sum(1 for s in skids if s > 16192)
    print(f"  beyond the sealed margin 16192: {over_margin} of {len(skids)}")
print(f"\nfailures by core: {dict(fail_by_core)}   arms by core: {dict(by_core)}")
if fails:
    ft = sorted(r["target"] for r in fails)
    print("failing targets:", ft)
    allt = sorted(t for t in all_targets)
    below = sum(1 for t in allt if t < min(ft))
    print(f"  target range over the whole run: {allt[0]} to {allt[-1]}, median {allt[len(allt)//2]}")
    print(f"  share of all targets below the smallest failing target: "
          f"{below/len(allt):.3f}")
if seen_tsc and ratios:
    ratios.sort()
    print(f"\nbranches per TSC tick during the run to the overflow: "
          f"p1={pct(ratios,0.01):.3f} p50={pct(ratios,0.5):.3f} p99={pct(ratios,0.99):.3f}")
    for r in fails:
        if r.get("tsc_to_preempt"):
            print(f"  failing arm core{r['core']} idx {r['idx']}: work {r['work_at_preempt']} "
                  f"in {r['tsc_to_preempt']} ticks = {r['work_at_preempt']/r['tsc_to_preempt']:.3f}")
if seen_smi:
    print(f"\narms with a non-zero SMI-received delta: {smi_arms} of {arms}")
if seen_retry:
    print(f"re-arm after an overshoot: attempted on {retry_attempted}, landed exactly on "
          f"{retry_landed}")
