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
        # floor: at least min_reps present
        too_few = len(reps) < min_reps
        # exactness recomputed here, not read from r["exact"]:
        mism = [rr for rr in rs if (rr["count_n2"] - rr["count_n1"]) != rr["oracle_delta"]]
        mux = [rr for rr in rs if rr.get("multiplexed")]
        # per-class offset stability: absolute count offset must be constant across reps
        offs = {(rr["count_n1"] - rr["taken_per_iter"] * rr["n1"]) for rr in rs}
        stable = len(offs) == 1
        good = not missing and not too_few and not mism and not mux and stable
        ok = ok and good
        print(f"{'PASS' if good else 'FAIL'} exactness[{pl}]: reps={len(reps)} "
              f"mismatches={len(mism)} multiplexed={len(mux)} "
              f"offset_stable={stable} missing_reps={missing}")
    return ok


def check_overflow(recs, min_overflows):
    ov = [r for r in recs if r.get("kind") == "overflow"]
    if not ov:
        print("FAIL: no overflow records"); return False
    total = sum(r["overflows_delivered"] for r in ov)
    floor_sum = sum(r["expected_overflows_floor"] for r in ov)
    mux = [r for r in ov if r.get("multiplexed")]
    # multiplicity: delivered must meet or exceed the by-construction floor per record
    short = [r for r in ov if r["overflows_delivered"] < r["expected_overflows_floor"]]
    good = total >= min_overflows and not mux and not short
    print(f"{'PASS' if good else 'FAIL'} overflow: total_delivered={total} "
          f"floor_sum={floor_sum} min_required={min_overflows} "
          f"short_records={len(short)} multiplexed={len(mux)}")
    return good


def check_speclockmap(off_path, on_path):
    """AE-1(c): with the workaround OFF the `locked` differential overcounts and/or
    varies; with it ON it equals the oracle exactly and is invariant across reps."""
    off = [r for r in load([off_path]) if r.get("payload") == "locked"]
    on = [r for r in load([on_path]) if r.get("payload") == "locked"]
    if not off or not on:
        print("FAIL: speclockmap needs `locked` records in both --off and --on"); return False
    off_deltas = {r["count_n2"] - r["count_n1"] for r in off}
    on_deltas = {r["count_n2"] - r["count_n1"] for r in on}
    oracle = on[0]["oracle_delta"]
    on_exact = on_deltas == {oracle}
    # the evidence we want: OFF is either non-oracle or non-invariant (the overcount);
    # ON is exactly the oracle and invariant. A NULL result (off already exact) is
    # reported honestly, not massaged.
    off_overcounts = any(d != oracle for d in off_deltas) or len(off_deltas) > 1
    good = on_exact and off_overcounts
    print(f"{'PASS' if good else 'INCONCLUSIVE'} speclockmap: "
          f"off_deltas={sorted(off_deltas)} on_deltas={sorted(on_deltas)} "
          f"oracle={oracle} on_exact={on_exact} off_overcounts={off_overcounts}")
    return good


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
