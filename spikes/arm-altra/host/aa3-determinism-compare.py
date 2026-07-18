#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-3 co-tenant determinism comparison (Paul's P0 rule): a SOLO reference lane and a
# CO-TENANT shard sharing the same base seed draw the same (payload, scale, seed, target)
# tuples. The exact-landing digest of a tuple must be IDENTICAL solo vs co-tenant -- if it
# differs, co-tenancy perturbed deterministic guest state, which is a P0 determinism finding
# (STOP and report, never serialize to hide). This recomputes that comparison from the raw
# records and emits determinism.json in the AA-1c parallel-evidence shape.
#
#   aa3-determinism-compare.py [--exclude-payload P]... <solo-ref.jsonl> <cotenant.jsonl> [...]
#
# Keys on (payload, scale, seed, target); compares BOTH the exact-landing digest
# (overflow.landed_digest) and the window-end full-state digest (state_digest). Every tuple
# present in both the solo lane and a co-tenant lane is compared; a mismatch is a divergence.
#
# The comparison runs on the replay-DETERMINISTIC payloads only. `llsc-atomics` is excluded by
# default because its landed state legitimately diverges even WITHIN a lane (the §4 spontaneous
# STXR fail/succeed hazard, AA-4's domain) — the floor-checker carves it out of AA-3
# replay-identity for the same reason, so a co-tenancy determinism verdict must not read that
# intrinsic non-determinism as a co-tenant P0. `wfi-idle` (AA-5 timer, excluded from the run)
# is defensively excluded too. This mirrors the checker's carve-out exactly.
import json
import sys

DEFAULT_EXCLUDE = {"llsc-atomics", "wfi-idle"}


def load(path, exclude):
    """(payload, scale, seed, target) -> {"landed": digest, "state": digest} from a records file."""
    out = {}
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            if r["payload"] in exclude:
                continue
            ov = r.get("overflow") or {}
            if not ov.get("armed"):
                continue
            key = (r["payload"], r["scale"], r["seed"], ov["target"])
            digs = {"landed": ov.get("landed_digest"), "state": r.get("state_digest")}
            # Within a lane a tuple repeats (reps); replay-identity already proved those equal
            # for the deterministic payloads, but assert it here too so a within-lane divergence
            # cannot hide behind the cross-lane comparison.
            prev = out.get(key)
            if prev is not None and prev != digs:
                raise SystemExit(
                    f"within-lane divergence in {path} at {key}: {prev} != {digs}"
                )
            out[key] = digs
    return out


def main():
    args = sys.argv[1:]
    exclude = set(DEFAULT_EXCLUDE)
    files = []
    i = 0
    while i < len(args):
        if args[i] == "--exclude-payload":
            exclude.add(args[i + 1])
            i += 2
        else:
            files.append(args[i])
            i += 1
    if len(files) < 2:
        raise SystemExit(
            "usage: aa3-determinism-compare.py [--exclude-payload P]... <solo.jsonl> <cotenant.jsonl> [...]"
        )
    solo = load(files[0], exclude)
    cotenant = {}
    for p in files[1:]:
        for k, v in load(p, exclude).items():
            cotenant[k] = v

    shared = sorted(set(solo) & set(cotenant))
    divergences = []
    for key in shared:
        s, c = solo[key], cotenant[key]
        if s != c:
            payload, scale, seed, target = key
            divergences.append(
                {
                    "payload": payload,
                    "scale": scale,
                    "seed": seed,
                    "target": target,
                    "solo": s,
                    "cotenant": c,
                }
            )

    report = {
        "solo_tuples": len(solo),
        "cotenant_tuples": len(cotenant),
        "shared_tuples_compared": len(shared),
        "excluded_payloads": sorted(exclude),
        "digests_compared_per_tuple": ["overflow.landed_digest", "state_digest"],
        "divergences": divergences,
        "verdict": "MATCH" if not divergences and shared else ("NO_OVERLAP" if not shared else "DIVERGENCE"),
    }
    print(json.dumps(report, indent=2))
    # Fail closed: a determinism comparison that found no overlap is not a pass, and any
    # divergence is a P0.
    if report["verdict"] != "MATCH":
        sys.exit(1)


if __name__ == "__main__":
    main()
