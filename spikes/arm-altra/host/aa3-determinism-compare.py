#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""AA-3 solo == co-tenant exact-landing determinism comparison.

Usage:
  aa3-determinism-compare.py [--exclude-payload P]... <solo-run-set> <cotenant-run-set> [...]

Each input may be a run-set directory (preferred: the manifest's count and sha256 are
verified) or a legacy records.jsonl path. The comparison is a FULL JOIN over
(`payload`, `scale`, `seed`, `target`): MATCH requires identical key sets, identical
per-key repetition counts, and identical landed + final-state digests. Partial overlap
is INCOMPLETE_COVERAGE, never MATCH. Duplicate sample ids and tuple collisions across
co-tenant inputs are rejected instead of silently overwriting evidence.

`llsc-atomics` is excluded by default because its landed state legitimately diverges
within a lane (the AA-4 exclusive-monitor hazard). `wfi-idle` is excluded because its
timer resume is an AA-5 concern. This mirrors floor-check's AA-3 replay carve-out.
"""

import hashlib
import json
import sys
from pathlib import Path


DEFAULT_EXCLUDE = {"llsc-atomics", "wfi-idle"}
WORK_CLOCK_BINDING = (
    "arm64 BR_RETIRED raw 0x21 = all architecturally executed branch instructions "
    "(taken or not; AA1-F1)"
)


class EvidenceError(ValueError):
    """The comparison input cannot certify a complete join."""


class WithinLaneDivergence(ValueError):
    """Repeated executions of one tuple diverged within a lane."""


def display_key(key):
    return {
        "payload": key[0],
        "scale": key[1],
        "seed": key[2],
        "target": key[3],
    }


def resolve_input(input_path):
    path = Path(input_path)
    if not path.is_dir():
        return path, None, None

    manifest_path = path / "run-set.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise EvidenceError(f"cannot load {manifest_path}: {exc}") from exc

    expected = manifest.get("attempted")
    if not isinstance(expected, int) or expected < 1:
        raise EvidenceError(f"{manifest_path}: attempted must be a positive integer")
    expected_hash = manifest.get("records_sha256")
    if not isinstance(expected_hash, str):
        raise EvidenceError(f"{manifest_path}: records_sha256 is required")
    return path / manifest.get("records_file", "records.jsonl"), expected, expected_hash


def load(input_path, exclude):
    """Load one run-set without collapsing its expected repeated tuples."""
    records_path, expected_records, expected_hash = resolve_input(input_path)
    values = {}
    multiplicities = {}
    sample_ids = set()
    raw_records = 0
    included_records = 0
    digest = hashlib.sha256()

    try:
        fh = records_path.open("rb")
    except OSError as exc:
        raise EvidenceError(f"cannot read {records_path}: {exc}") from exc

    with fh:
        for line_number, raw_line in enumerate(fh, 1):
            digest.update(raw_line)
            line = raw_line.strip()
            if not line:
                continue
            raw_records += 1
            try:
                r = json.loads(line)
                sample_id = r["sample_id"]
                payload = r["payload"]
            except (json.JSONDecodeError, KeyError, TypeError) as exc:
                raise EvidenceError(
                    f"{records_path}:{line_number}: malformed comparison record: {exc}"
                ) from exc
            if sample_id in sample_ids:
                raise EvidenceError(
                    f"{records_path}:{line_number}: duplicate sample_id {sample_id}"
                )
            sample_ids.add(sample_id)
            if payload in exclude:
                continue

            try:
                overflow = r["overflow"]
                if not overflow["armed"]:
                    raise EvidenceError(
                        f"{records_path}:{line_number}: deterministic AA-3 tuple is not armed"
                    )
                key = (payload, r["scale"], r["seed"], overflow["target"])
                value = {
                    "landed": overflow["landed_digest"],
                    "state": r["state_digest"],
                }
            except (KeyError, TypeError) as exc:
                raise EvidenceError(
                    f"{records_path}:{line_number}: malformed AA-3 tuple: {exc}"
                ) from exc

            included_records += 1
            previous = values.get(key)
            if previous is not None and previous != value:
                raise WithinLaneDivergence(
                    f"{records_path}:{line_number}: tuple {key} diverged within the lane: "
                    f"{previous} != {value}"
                )
            values[key] = value
            multiplicities[key] = multiplicities.get(key, 0) + 1

    actual_hash = digest.hexdigest()
    if expected_records is not None and raw_records != expected_records:
        raise EvidenceError(
            f"{records_path}: read {raw_records} records, manifest expects {expected_records}"
        )
    if expected_hash is not None and actual_hash != expected_hash:
        raise EvidenceError(
            f"{records_path}: sha256 {actual_hash} != manifest {expected_hash}"
        )
    return {
        "source": str(input_path),
        "values": values,
        "multiplicities": multiplicities,
        "raw_records": raw_records,
        "included_records": included_records,
        "expected_records": expected_records,
        "records_sha256": actual_hash,
        "manifest_verified": expected_records is not None,
    }


def parse_args(argv):
    exclude = set(DEFAULT_EXCLUDE)
    inputs = []
    i = 0
    while i < len(argv):
        if argv[i] == "--exclude-payload":
            if i + 1 >= len(argv):
                raise EvidenceError("--exclude-payload requires a payload name")
            exclude.add(argv[i + 1])
            i += 2
        else:
            inputs.append(argv[i])
            i += 1
    if len(inputs) < 2:
        raise EvidenceError(
            "usage: aa3-determinism-compare.py [--exclude-payload P]... "
            "<solo-run-set> <cotenant-run-set> [...]"
        )
    return exclude, inputs


def main(argv):
    try:
        exclude, inputs = parse_args(argv)
        solo_input = load(inputs[0], exclude)
        cotenant_inputs = [load(path, exclude) for path in inputs[1:]]

        cotenant = {}
        cotenant_multiplicities = {}
        cotenant_sources = {}
        for lane in cotenant_inputs:
            for key, value in lane["values"].items():
                if key in cotenant:
                    raise EvidenceError(
                        f"duplicate co-tenant tuple {key} in {cotenant_sources[key]} "
                        f"and {lane['source']}"
                    )
                cotenant[key] = value
                cotenant_multiplicities[key] = lane["multiplicities"][key]
                cotenant_sources[key] = lane["source"]
    except WithinLaneDivergence as exc:
        report = {
            "error": str(exc),
            "verdict": "P0_WITHIN_LANE_DIVERGENCE",
            "work_clock_binding": WORK_CLOCK_BINDING,
        }
        print(json.dumps(report, indent=2, sort_keys=True))
        print(f"work clock binding: {WORK_CLOCK_BINDING}", file=sys.stderr)
        print(f"P0 DETERMINISM FINDING: {exc}", file=sys.stderr)
        return 1
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

    solo = solo_input["values"]
    solo_multiplicities = solo_input["multiplicities"]
    solo_keys = set(solo)
    cotenant_keys = set(cotenant)
    shared = sorted(solo_keys & cotenant_keys)
    solo_only = sorted(solo_keys - cotenant_keys)
    cotenant_only = sorted(cotenant_keys - solo_keys)
    multiplicity_mismatches = []
    divergences = []

    for key in shared:
        solo_reps = solo_multiplicities[key]
        cotenant_reps = cotenant_multiplicities[key]
        if solo_reps != cotenant_reps:
            mismatch = display_key(key)
            mismatch.update({"solo_repetitions": solo_reps, "cotenant_repetitions": cotenant_reps})
            multiplicity_mismatches.append(mismatch)
        if solo[key] != cotenant[key]:
            divergence = display_key(key)
            divergence.update({"solo": solo[key], "cotenant": cotenant[key]})
            divergences.append(divergence)

    full_join = not solo_only and not cotenant_only and not multiplicity_mismatches
    if not full_join:
        verdict = "INCOMPLETE_COVERAGE"
    elif divergences:
        verdict = "P0_DIVERGENCE"
    elif shared:
        verdict = "MATCH"
    else:
        verdict = "NO_OVERLAP"

    report = {
        "work_clock_binding": WORK_CLOCK_BINDING,
        "solo_tuples": len(solo),
        "cotenant_tuples": len(cotenant),
        "shared_tuples_compared": len(shared),
        "join_cardinality": {
            "solo_raw_records": solo_input["raw_records"],
            "solo_included_records": solo_input["included_records"],
            "solo_unique_keys": len(solo),
            "cotenant_raw_records": sum(lane["raw_records"] for lane in cotenant_inputs),
            "cotenant_included_records": sum(
                lane["included_records"] for lane in cotenant_inputs
            ),
            "cotenant_unique_keys": len(cotenant),
            "shared_keys": len(shared),
            "solo_only_keys": len(solo_only),
            "cotenant_only_keys": len(cotenant_only),
            "multiplicity_mismatches": len(multiplicity_mismatches),
            "full_both_sides": full_join,
        },
        "inputs": {
            "solo_manifest_verified": solo_input["manifest_verified"],
            "cotenant_manifests_verified": all(
                lane["manifest_verified"] for lane in cotenant_inputs
            ),
        },
        "excluded_payloads": sorted(exclude),
        "digests_compared_per_tuple": ["overflow.landed_digest", "state_digest"],
        "solo_only_examples": [display_key(key) for key in solo_only[:8]],
        "cotenant_only_examples": [display_key(key) for key in cotenant_only[:8]],
        "multiplicity_mismatch_examples": multiplicity_mismatches[:8],
        "divergences": divergences,
        "verdict": verdict,
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    print(f"work clock binding: {WORK_CLOCK_BINDING}", file=sys.stderr)
    print(
        "full join: "
        f"solo {len(solo)} keys/{solo_input['included_records']} included records; "
        f"co-tenant {len(cotenant)} keys/"
        f"{sum(lane['included_records'] for lane in cotenant_inputs)} included records; "
        f"shared {len(shared)}; solo-only {len(solo_only)}; "
        f"co-tenant-only {len(cotenant_only)}; "
        f"multiplicity mismatches {len(multiplicity_mismatches)}",
        file=sys.stderr,
    )

    if not full_join:
        print(
            "INCOMPLETE COVERAGE: solo and co-tenant inputs do not form a full join; "
            "MATCH is unknown.",
            file=sys.stderr,
        )
        return 2
    if not shared:
        print("NO SHARED TUPLES: nothing was compared; not a pass.", file=sys.stderr)
        return 2
    if divergences:
        print(
            f"P0 DETERMINISM FINDING: {len(divergences)} joined tuple(s) diverged.",
            file=sys.stderr,
        )
        return 1
    print(
        f"MATCH: all {len(shared)} tuples joined on both sides with equal repetition counts "
        "and bit-identical exact-landing + final-state digests.",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
