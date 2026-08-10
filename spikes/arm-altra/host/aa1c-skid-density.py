#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""AA-1(c) constants-pack extractor: the skid distribution and the per-class density.

`docs/ARM-ALTRA.md` §AA-1(c) makes the skid distribution and the N1 `skid_margin` a
mandatory deliverable (§Definition of done #2). This reads the retained per-sample
records (bulk + grid run-sets) and derives, purely from the raw `overflow.skid` and
`measured_taken` fields — never from a summary line — :

  * the skid distribution per (payload, scale, condition): n / min / max / mean / p50 /
    p95 / p99 / p100, over DELIVERED armed overflows only (deliveries >= 1);
  * the candidate `skid_margin`: the worst-case skid, reported per scale (skid is
    scale-dependent — more branches retire during a fixed kick latency at a higher loop
    branch rate — so the margin is dominated by the largest scale, 1e8) and overall;
  * the per-class density: measured_taken min/max per (payload, scale) — the
    event-density inputs the SimCpu/PlannerConfig re-parameterization needs.

It is descriptive only: it declares no disposition and asserts no acceptance. The
floor-checker grades; this quantifies the constants the graded evidence contains.

Usage:  aa1c-skid-density.py <run-set-dir> [<run-set-dir> ...]
Emits stable JSON (sorted keys) on stdout; a human summary on stderr.
"""
import json
import sys
from pathlib import Path


def load_records(dir_path):
    d = Path(dir_path)
    manifest = json.loads((d / "run-set.json").read_text())
    recs_file = manifest.get("records_file", "records.jsonl")
    recs = []
    with (d / recs_file).open() as fh:
        for line in fh:
            line = line.strip()
            if line:
                recs.append(json.loads(line))
    return recs


def pct(sorted_vals, p):
    if not sorted_vals:
        return None
    # Nearest-rank percentile; deterministic, no interpolation/float surprises.
    k = max(0, min(len(sorted_vals) - 1, (p * len(sorted_vals)) // 100))
    return sorted_vals[k]


def main(argv):
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2

    # (payload, scale, condition) -> list of skids ; and measured_taken ranges.
    skids = {}
    taken = {}
    lost = {}  # armed but deliveries == 0 (migration probe / anomaly), accounted separately
    total_delivered = 0
    for dir_path in argv[1:]:
        for r in load_records(dir_path):
            o = r.get("overflow")
            if not o or not o.get("armed"):
                continue
            key = (r["payload"], r["scale"], r["condition"])
            if o.get("deliveries", 0) >= 1 and o.get("skid") is not None:
                skids.setdefault(key, []).append(int(o["skid"]))
                total_delivered += 1
            else:
                lost[key] = lost.get(key, 0) + 1
            tk = int(r["measured_taken"])
            lo, hi = taken.get(key, (tk, tk))
            taken[key] = (min(lo, tk), max(hi, tk))

    per_group = {}
    per_scale_max = {}
    overall_max = 0
    for key, vals in sorted(skids.items()):
        vals.sort()
        payload, scale, cond = key
        gmax = vals[-1]
        overall_max = max(overall_max, gmax)
        per_scale_max[scale] = max(per_scale_max.get(scale, 0), gmax)
        per_group["|".join(key)] = {
            "n": len(vals),
            "min": vals[0],
            "max": gmax,
            "mean": sum(vals) // len(vals),
            "p50": pct(vals, 50),
            "p95": pct(vals, 95),
            "p99": pct(vals, 99),
            "measured_taken_min": taken[key][0],
            "measured_taken_max": taken[key][1],
        }

    out = {
        "total_delivered_armed": total_delivered,
        "lost_by_group": {"|".join(k): v for k, v in sorted(lost.items())},
        "skid_margin_candidate_overall": overall_max,
        "skid_margin_candidate_by_scale": dict(sorted(per_scale_max.items())),
        "per_group": per_group,
    }
    print(json.dumps(out, indent=2, sort_keys=True))

    print(f"\n--- AA-1(c) skid/density summary ---", file=sys.stderr)
    print(f"delivered armed overflows analysed: {total_delivered}", file=sys.stderr)
    print(f"skid_margin candidate (overall max): {overall_max}", file=sys.stderr)
    for scale, m in sorted(per_scale_max.items()):
        print(f"  worst-case skid @ {scale}: {m}", file=sys.stderr)
    if lost:
        print(f"groups with lost/undelivered armed (expected only in the migration probe):",
              file=sys.stderr)
        for k, v in sorted(lost.items()):
            print(f"  {'|'.join(k)}: {v}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
