#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""AA-1(c) solo == co-tenant determinism check (Paul's 2026-07-17 directive).

The parallel campaign runs the matrix concurrently across cores (all cores busy = the
co-tenant stress). Because BR_RETIRED is a per-core, V-time (frequency-independent) count,
a tuple's execution must be BIT-IDENTICAL whether it ran solo or with every sibling core
busy. This compares, per shared `(payload, scale, seed, target)` tuple, the **final
state_digest** (the state at the exit sentinel — deterministic, unlike the skid-dependent
landed_digest) between a SOLO reference run-set and a CO-TENANT run-set that reused the
solo seed. Any divergence is a **P0 determinism finding** (`docs/ARM-ALTRA.md` §the-bet):
STOP and report; never serialize to make it disappear.

It also cross-checks `measured_taken` (the count) and `overflow.deliveries` per tuple.

Usage:  aa1c-determinism-check.py <solo-run-set-dir> <cotenant-run-set-dir>
Exit 0 iff every shared tuple's state_digest (and count, and delivery) matches. Emits a
stable-JSON report on stdout; a human summary on stderr. Exit 2 if there are NO shared
tuples (nothing was compared — not a pass).
"""
import json
import sys
from pathlib import Path


def load(dir_path):
    d = Path(dir_path)
    manifest = json.loads((d / "run-set.json").read_text())
    recs = {}
    with (d / manifest.get("records_file", "records.jsonl")).open() as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            o = r.get("overflow") or {}
            key = (r["payload"], r["scale"], r["seed"], o.get("target"))
            recs[key] = r
    return recs


def main(argv):
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    solo = load(argv[1])
    cot = load(argv[2])
    shared = sorted(set(solo) & set(cot))

    divergences = []
    for key in shared:
        s, c = solo[key], cot[key]
        for field, sv, cv in (
            ("state_digest", s.get("state_digest"), c.get("state_digest")),
            ("measured_taken", s.get("measured_taken"), c.get("measured_taken")),
            ("deliveries", (s.get("overflow") or {}).get("deliveries"),
             (c.get("overflow") or {}).get("deliveries")),
        ):
            if sv != cv:
                divergences.append({
                    "payload": key[0], "scale": key[1], "seed": key[2], "target": key[3],
                    "field": field, "solo": sv, "cotenant": cv,
                })

    report = {
        "solo_tuples": len(solo),
        "cotenant_tuples": len(cot),
        "shared_tuples_compared": len(shared),
        "divergences": divergences,
        "verdict": "MATCH" if not divergences and shared else
                   ("NO_OVERLAP" if not shared else "P0_DIVERGENCE"),
    }
    print(json.dumps(report, indent=2, sort_keys=True))

    print("\n--- AA-1(c) solo==co-tenant determinism ---", file=sys.stderr)
    print(f"shared tuples compared: {len(shared)}", file=sys.stderr)
    if not shared:
        print("NO SHARED TUPLES — the co-tenant set did not reuse the solo seed; nothing "
              "was compared. NOT a pass.", file=sys.stderr)
        return 2
    if divergences:
        print(f"P0 DETERMINISM FINDING: {len(divergences)} field divergence(s) — solo != "
              f"co-tenant. STOP and report; do not serialize to hide it.", file=sys.stderr)
        for d in divergences[:8]:
            print(f"  {d['payload']}/{d['scale']}/seed={d['seed']}: {d['field']} "
                  f"solo={d['solo']} cotenant={d['cotenant']}", file=sys.stderr)
        return 1
    print(f"MATCH: all {len(shared)} shared tuples bit-identical solo vs co-tenant "
          f"(state_digest + count + delivery). Co-tenancy does not perturb the digest.",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
