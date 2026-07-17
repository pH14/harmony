#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""check-floors.py — machine floor-checker (docs/AMD-EPYC.md §Evidence integrity #2).

Recomputes every numeric acceptance floor FROM the retained raw per-sample records
that the hammer wrote, never from a summary line the harness asserted. The stage
disposition may not be written until this passes; this script's own stdout is
retained evidence. Exit non-zero if any floor is unmet.

It is deliberately dumb and independent of the C harness: it re-derives exactness
from count deltas vs oracle deltas, re-derives overflow multiplicity/totality from
the per-record overflow counts, and re-checks that every attempted rep is present
(no missing samples — a gap is a failure to account, not a pass, #6).

Usage:
  check-floors.py exactness --min-reps R --records FILE [FILE ...]
  check-floors.py overflow  --min-overflows M --records FILE [FILE ...]
  check-floors.py speclockmap --off FILE --on FILE   # AE-1(c): off overcounts, on exact
"""
import argparse, json, sys


def load(paths):
    recs = []
    for p in paths:
        with open(p) as f:
            arr = json.load(f)
        for r in arr:
            if r.get("kind") == "end":
                # gate-RC propagation (#1): a harness that ended non-zero cannot pass here
                if r.get("rc", 1) != 0:
                    print(f"FAIL[{p}]: harness end rc={r.get('rc')} (a mismatch was seen in-run)")
                    raise SystemExit(1)
            else:
                r["_src"] = p
                recs.append(r)
    return recs


def _clean(rr):
    """A window is clean iff no interrupt landed in either sub-measurement. Records
    predating the irq-accounting field are treated as clean=field-absent -> assume the
    record's own `clean` flag, else True."""
    if "clean" in rr:
        return bool(rr["clean"])
    if "irqs_n1" in rr and "irqs_n2" in rr:
        return rr["irqs_n1"] == 0 and rr["irqs_n2"] == 0
    return True


def check_exactness(recs, min_reps):
    ex = [r for r in recs if r.get("kind") == "exactness"]
    if not ex:
        print("FAIL: no exactness records"); return False
    ok = True
    by_payload = {}
    for r in ex:
        by_payload.setdefault(r["payload"], []).append(r)
    for pl, rs in sorted(by_payload.items()):
        reps = sorted(rr["rep"] for rr in rs)
        # totality (#6): reps must be the contiguous set 0..max, none missing
        expect = list(range(len(reps)))
        missing = expect != reps
        clean = [rr for rr in rs if _clean(rr)]
        contaminated = [rr for rr in rs if not _clean(rr)]
        # floor: at least min_reps CLEAN windows present (a vacuous all-contaminated
        # run must not pass — the exactness claim is about interrupt-free windows).
        too_few = len(clean) < min_reps
        # exactness recomputed here on CLEAN windows, not read from rr["exact"]:
        mism = [rr for rr in clean if (rr["count_n2"] - rr["count_n1"]) != rr["oracle_delta"]]
        mux = [rr for rr in rs if rr.get("multiplexed")]
        # per-class offset stability across CLEAN windows only
        offs = {(rr["count_n1"] - rr["taken_per_iter"] * rr["n1"]) for rr in clean}
        stable = len(offs) <= 1
        good = not missing and not too_few and not mism and not mux and stable
        ok = ok and good
        print(f"{'PASS' if good else 'FAIL'} exactness[{pl}]: reps={len(reps)} "
              f"clean={len(clean)} contaminated={len(contaminated)} "
              f"clean_mismatches={len(mism)} multiplexed={len(mux)} "
              f"offset_stable={stable} missing_reps={missing}")
    return ok


def check_overflow(recs, min_overflows):
    summ = [r for r in recs if r.get("kind") == "overflow_summary"]
    anom = [r for r in recs if r.get("kind") == "overflow_anomaly"]
    if not summ:
        print("FAIL: no overflow_summary records"); return False
    total_ok = sum(r["hits_1_ok"] for r in summ)
    total_lost = sum(r["hits_0_lost"] for r in summ)
    total_dup = sum(r["hits_gt1_dup"] for r in summ)
    total_arms = sum(r["arms_total"] for r in summ)
    # totality (#6): every arm is accounted -> lost + ok + dup == arms
    accounted = all(r["hits_0_lost"] + r["hits_1_ok"] + r["hits_gt1_dup"] == r["arms_total"] for r in summ)
    # anomaly records must corroborate the tally (every lost/dup arm has a record)
    anom_expected = total_lost + total_dup
    # zero missed/duplicate overflows is the doc's AE-1(d) bar
    good = (total_lost == 0 and total_dup == 0 and total_ok >= min_overflows
            and accounted and len(anom) >= anom_expected)
    skid_max = max((r["skid_max"] for r in summ), default=0)
    clean_skid_max = max((r["clean_skid_max"] for r in summ), default=0)
    print(f"{'PASS' if good else 'FAIL'} overflow: arms={total_arms} delivered_ok={total_ok} "
          f"lost={total_lost} duplicate={total_dup} min_required={min_overflows} "
          f"accounted={accounted} anomaly_records={len(anom)}/{anom_expected} "
          f"skid_max={skid_max} clean_skid_max={clean_skid_max}")
    for r in summ:
        print(f"  [{r['payload']}] period={r['period']} skid_hist(0|1-9|10-99|100-999|1e3-9999|>=1e4)="
              f"{r['skid_hist']} clean_arms={r['clean_arms']} clean_skid_max={r['clean_skid_max']}")
    return good


def check_speclockmap(off_path, on_path):
    """AE-1(c): with the workaround OFF the `locked` differential overcounts and/or
    varies; with it ON it equals the oracle exactly and is invariant across reps."""
    # judged ONLY on interrupt-free (clean) windows — the ±1 tick contamination is not
    # the SpecLockMap effect and would appear on both sides otherwise.
    off = [r for r in load([off_path]) if r.get("payload") == "locked" and _clean(r)]
    on = [r for r in load([on_path]) if r.get("payload") == "locked" and _clean(r)]
    if not off or not on:
        print("FAIL: speclockmap needs CLEAN `locked` records in both --off and --on"); return False
    off_deltas = {r["count_n2"] - r["count_n1"] for r in off}
    on_deltas = {r["count_n2"] - r["count_n1"] for r in on}
    oracle = on[0]["oracle_delta"]
    on_exact = on_deltas == {oracle}
    # the doc's hypothesis: OFF overcounts (non-oracle or non-invariant), ON is exact.
    # A NULL result (OFF already exact on clean windows) is reported honestly — on this
    # Zen 2 part the erratum is simply not reproduced for ex_ret_brn_tkn.
    off_overcounts = any(d != oracle for d in off_deltas) or len(off_deltas) > 1
    verdict = "REPRODUCED" if (on_exact and off_overcounts) else \
              ("NULL(no-overcount)" if on_exact and off_deltas == {oracle} else "AMBIGUOUS")
    print(f"speclockmap[{verdict}] clean windows: "
          f"off_clean={len(off)} off_deltas={sorted(off_deltas)} "
          f"on_clean={len(on)} on_deltas={sorted(on_deltas)} oracle={oracle}")
    # the checker's job is to REPORT the reproduced-vs-null verdict, not to fail on a
    # null: a scientifically-clean null is a valid ladder input (doc §hardware flag).
    return on_exact


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    e = sub.add_parser("exactness"); e.add_argument("--min-reps", type=int, required=True)
    e.add_argument("--records", nargs="+", required=True)
    o = sub.add_parser("overflow"); o.add_argument("--min-overflows", type=int, required=True)
    o.add_argument("--records", nargs="+", required=True)
    s = sub.add_parser("speclockmap"); s.add_argument("--off", required=True); s.add_argument("--on", required=True)
    a = ap.parse_args()
    if a.cmd == "exactness":
        ok = check_exactness(load(a.records), a.min_reps)
    elif a.cmd == "overflow":
        ok = check_overflow(load(a.records), a.min_overflows)
    else:
        ok = check_speclockmap(a.off, a.on)
    print("FLOOR_CHECK:", "PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
