#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""AA-1(c) solo == co-tenant determinism check (Paul's 2026-07-17 directive).

The parallel campaign runs the matrix concurrently across cores (all cores busy = the
co-tenant stress). Because BR_RETIRED is a per-core, V-time (frequency-independent) count,
a tuple's execution must be BIT-IDENTICAL whether it ran solo or with every sibling core
busy. This compares, per shared `(payload, scale, seed, target)` tuple, the **final
state_digest** (the state at the exit sentinel — deterministic, unlike the skid-dependent
landed_digest) between a SOLO reference run-set and a CO-TENANT run-set that reused the
solo seed. It requires a full join: missing or extra keys are incomplete coverage, never a
match. Any divergence is a **P0 determinism finding** (`docs/ARM-ALTRA.md` §the-bet):
STOP and report; never serialize to make it disappear.

It also cross-checks `measured_taken` (the count) and `overflow.deliveries` per tuple.

Usage:  aa1c-determinism-check.py <solo-run-set-dir> <cotenant-run-set-dir>
Exit 0 iff both key sets are identical and every joined tuple's state_digest (and count, and
delivery) matches. Emits a stable-JSON report on stdout; a human summary on stderr. Exit 2
for invalid or incomplete evidence.
"""
import hashlib
import json
import sys
from pathlib import Path


WORK_CLOCK_BINDING = (
    "arm64 BR_RETIRED raw 0x21 = all architecturally executed branch instructions "
    "(taken or not; AA1-F1)"
)


class EvidenceError(ValueError):
    """The comparison input cannot certify a complete join."""


def digest_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def display_key(key):
    return {
        "payload": key[0],
        "scale": key[1],
        "seed": key[2],
        "target": key[3],
    }


def load(dir_path):
    d = Path(dir_path)
    manifest_path = d / "run-set.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise EvidenceError(f"cannot load {manifest_path}: {exc}") from exc

    expected = manifest.get("attempted")
    if not isinstance(expected, int) or expected < 1:
        raise EvidenceError(f"{manifest_path}: attempted must be a positive integer")

    # Lane identity, for the provenance attestation (hm-6sj): a solo reference and a co-tenant
    # run are DISTINCT run-sets recorded under DIFFERENT conditions. The RunSet schema requires
    # both fields, so their absence is malformed evidence.
    run_set_id = manifest.get("run_set_id")
    if not isinstance(run_set_id, str) or not run_set_id:
        raise EvidenceError(f"{manifest_path}: run_set_id is required")
    condition = manifest.get("condition")
    if not isinstance(condition, str) or not condition:
        raise EvidenceError(f"{manifest_path}: condition is required")

    records_path = d / manifest.get("records_file", "records.jsonl")
    expected_hash = manifest.get("records_sha256")
    if not isinstance(expected_hash, str):
        raise EvidenceError(f"{manifest_path}: records_sha256 is required")
    try:
        actual_hash = digest_file(records_path)
    except OSError as exc:
        raise EvidenceError(f"cannot read {records_path}: {exc}") from exc
    if actual_hash != expected_hash:
        raise EvidenceError(
            f"{records_path}: sha256 {actual_hash} != manifest {expected_hash}"
        )

    recs = {}
    records_read = 0
    with records_path.open(encoding="utf-8") as fh:
        for line_number, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            records_read += 1
            try:
                r = json.loads(line)
                o = r.get("overflow") or {}
                key = (r["payload"], r["scale"], r["seed"], o["target"])
                # Require + type-check the three COMPARED fields (hm-cte). Reading them with
                # `.get()` at comparison time let a record omitting all three on BOTH lanes compare
                # `None == None` and report MATCH having compared nothing. Demand them here, exactly
                # as aa3-determinism-compare.py does (a KeyError there → EvidenceError), so a
                # symmetric schema drift is caught as malformed evidence, not read as a match.
                if not isinstance(r["state_digest"], str) or not r["state_digest"]:
                    raise TypeError("state_digest must be a non-empty string")
                if not isinstance(r["measured_taken"], int) or isinstance(r["measured_taken"], bool):
                    raise TypeError("measured_taken must be an integer")
                if not isinstance(o["deliveries"], int) or isinstance(o["deliveries"], bool):
                    raise TypeError("overflow.deliveries must be an integer")
            except (json.JSONDecodeError, KeyError, TypeError) as exc:
                raise EvidenceError(
                    f"{records_path}:{line_number}: malformed comparison record: {exc}"
                ) from exc
            if key in recs:
                raise EvidenceError(
                    f"{records_path}:{line_number}: duplicate comparison key {key}"
                )
            recs[key] = r
    if records_read != expected:
        raise EvidenceError(
            f"{records_path}: read {records_read} records, manifest expects {expected}"
        )
    return {
        "records": recs,
        "records_read": records_read,
        "expected_records": expected,
        "records_sha256": actual_hash,
        "run_set_id": run_set_id,
        "condition": condition,
    }


def attest_distinct_lanes(solo, cotenant):
    """The two lanes must be a solo reference and a co-tenant run — DISTINCT run-sets under
    DIFFERENT conditions (hm-6sj). Comparing a directory against itself, or a copied/mislabelled
    lane, would otherwise full-join MATCH without ever contrasting solo against co-tenant."""
    if solo["run_set_id"] == cotenant["run_set_id"]:
        raise EvidenceError(
            f"both lanes are the same run-set (run_set_id {solo['run_set_id']!r}) — a determinism "
            "comparison must contrast a solo reference against a DISTINCT co-tenant run, not a "
            "directory against itself"
        )
    if solo["condition"] == cotenant["condition"]:
        raise EvidenceError(
            f"both lanes recorded the same condition ({solo['condition']!r}) — the comparison must "
            "contrast a pinned-solo reference against a co-tenant run, and identical conditions "
            "cannot be that contrast"
        )


def main(argv):
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        solo_input = load(argv[1])
        cot_input = load(argv[2])
        attest_distinct_lanes(solo_input, cot_input)
    except EvidenceError as exc:
        report = {
            "error": str(exc),
            "verdict": "INVALID_INPUT",
            "work_clock_binding": WORK_CLOCK_BINDING,
        }
        print(json.dumps(report, indent=2, sort_keys=True))
        print(f"work clock binding: {WORK_CLOCK_BINDING}", file=sys.stderr)
        print(f"INVALID INPUT: {exc}", file=sys.stderr)
        return 2

    solo = solo_input["records"]
    cot = cot_input["records"]
    solo_keys = set(solo)
    cot_keys = set(cot)
    shared = sorted(solo_keys & cot_keys)
    solo_only = sorted(solo_keys - cot_keys)
    cot_only = sorted(cot_keys - solo_keys)
    full_join = not solo_only and not cot_only

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

    # Coverage defects cannot mask an observed same-key determinism failure.
    if divergences:
        verdict = "P0_DIVERGENCE"
    elif not full_join:
        verdict = "INCOMPLETE_COVERAGE"
    elif shared:
        verdict = "MATCH"
    else:
        verdict = "NO_OVERLAP"

    report = {
        "work_clock_binding": WORK_CLOCK_BINDING,
        "solo_tuples": len(solo),
        "cotenant_tuples": len(cot),
        "shared_tuples_compared": len(shared),
        "join_cardinality": {
            "solo_expected_records": solo_input["expected_records"],
            "solo_records": solo_input["records_read"],
            "solo_unique_keys": len(solo),
            "cotenant_expected_records": cot_input["expected_records"],
            "cotenant_records": cot_input["records_read"],
            "cotenant_unique_keys": len(cot),
            "shared_keys": len(shared),
            "solo_only_keys": len(solo_only),
            "cotenant_only_keys": len(cot_only),
            "full_both_sides": full_join,
        },
        "solo_only_examples": [display_key(key) for key in solo_only[:8]],
        "cotenant_only_examples": [display_key(key) for key in cot_only[:8]],
        "divergences": divergences,
        "verdict": verdict,
    }
    print(json.dumps(report, indent=2, sort_keys=True))

    print("\n--- AA-1(c) solo==co-tenant determinism ---", file=sys.stderr)
    print(f"work clock binding: {WORK_CLOCK_BINDING}", file=sys.stderr)
    print(
        "full join: "
        f"solo {len(solo)}/{solo_input['expected_records']} keys/records; "
        f"co-tenant {len(cot)}/{cot_input['expected_records']} keys/records; "
        f"shared {len(shared)}; solo-only {len(solo_only)}; "
        f"co-tenant-only {len(cot_only)}",
        file=sys.stderr,
    )
    if divergences:
        print(f"P0 DETERMINISM FINDING: {len(divergences)} field divergence(s) — solo != "
              f"co-tenant. STOP and report; do not serialize to hide it.", file=sys.stderr)
        for d in divergences[:8]:
            print(f"  {d['payload']}/{d['scale']}/seed={d['seed']}: {d['field']} "
                  f"solo={d['solo']} cotenant={d['cotenant']}", file=sys.stderr)
        return 1
    if not full_join:
        print(
            "INCOMPLETE COVERAGE: solo and co-tenant key sets differ; MATCH is unknown.",
            file=sys.stderr,
        )
        return 2
    if not shared:
        print("NO SHARED TUPLES — the co-tenant set did not reuse the solo seed; nothing "
              "was compared. NOT a pass.", file=sys.stderr)
        return 2
    print(
        f"MATCH: all {len(shared)} tuples joined on both sides and are bit-identical "
        "solo vs co-tenant (state_digest + count + delivery). Co-tenancy does not "
        "perturb the digest.",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
