#!/usr/bin/env python3
"""Audit compatible controls and retain interval/censoring evidence without emulation."""
import argparse
import hashlib
import json
from pathlib import Path

BUDGET = 25_000_000
ENDPOINT = "missile_capacity"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fingerprint(summary):
    identity = summary["identity"]
    keys = ("game", "actions", "workers", "window", "result_slots", "memory_mib",
            "selector", "suffix", "mixture", "slot_retention", "policies",
            "rom_sha256", "core_sha256", "prefix_sha256", "stage", "level",
            "whole_game", "mm2_chain")
    return {"binary_sha256": summary["build"]["binary_sha256"],
            **{key: identity.get(key) for key in keys}}


def milestone_interval(rows, budget=BUDGET):
    previous_frames = 0
    previous_execution = -1
    arrival = None
    final_frames = 0
    for row in rows:
        frames, execution = row["frames_emulated"], row["executions"]
        assert frames >= previous_frames and execution >= previous_execution
        named = row.get("workload_diagnostics", {}).get("named_progress", {})
        seen = named.get("first_seen", {}).get(ENDPOINT)
        if seen is not None and arrival is None:
            assert 0 <= seen["execution"] <= execution
            assert seen["execution"] > previous_execution
            arrival = {"lower_open": previous_frames, "upper_closed": frames,
                       "first_execution": seen["execution"]}
        previous_frames, previous_execution = frames, execution
        final_frames = frames
    complete = final_frames >= budget
    if arrival is not None:
        lower, upper = arrival["lower_open"], arrival["upper_closed"]
        # A detection bracket crossing the budget does not establish a hit.
        hit = True if upper <= budget else False if lower >= budget else None
        restricted = [min(lower, budget), min(upper, budget)] if complete or upper <= budget else None
    else:
        hit = False if complete else None
        restricted = [budget, budget] if complete else None
    return {"endpoint": ENDPOINT, "budget_frames": budget,
            "arrival_interval": arrival, "observed_through_frames": final_frames,
            "observed_full_budget": complete, "hit_by_budget": hit,
            "restricted_cost_interval": restricted}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    root = args.experiment.resolve()
    reference = list((root / "runs/r05-ordinary").glob("*/summary.json"))
    assert len(reference) == 1
    base = json.loads(reference[0].read_text())
    expected = fingerprint(base)
    accepted, rejected, seed_ids = [], {}, set()
    for path in sorted((root / "runs").rglob("summary.json")):
        summary = json.loads(path.read_text())
        request = summary.get("search_request", {})
        if isinstance(request.get("seed"), int):
            seed_ids.add(request["seed"])
        if summary.get("status") != "complete" or summary.get("identity", {}).get("game") != "metroid":
            continue
        actual = fingerprint(summary)
        differences = [key for key in expected if actual[key] != expected[key]]
        if differences:
            key = ",".join(differences)
            rejected[key] = rejected.get(key, 0) + 1
            continue
        progress = path.parent / "campaign/progress.jsonl"
        assert progress.is_file() and progress.stat().st_size <= 128 * 1024**2
        rows = [json.loads(line) for line in progress.read_text().splitlines()]
        accepted.append({"summary_path": str(path.relative_to(root)), "summary_sha256": sha(path),
                         "progress_sha256": sha(progress), "seed": request["seed"],
                         "stop_reason": summary["result"]["stop_reason"],
                         "cpu_set": summary.get("cpu_set"), **milestone_interval(rows)})
    # Repeated qualification cells are not independent seeds. Choose by exposure,
    # then path, without using milestone outcomes to choose a replicate.
    per_seed = {}
    for row in accepted:
        rank = (row["observed_through_frames"], row["summary_path"])
        old = per_seed.get(row["seed"])
        if old is None or rank > (old["observed_through_frames"], old["summary_path"]):
            per_seed[row["seed"]] = row
    report = {"format": "continuation-yield-control-audit-v1", "emulator_frames_added": 0,
              "reference_summary_sha256": sha(reference[0]), "fingerprint": expected,
              "budget_frames": BUDGET, "endpoint": ENDPOINT,
              "compatible_cells": accepted, "unique_seed_records": list(per_seed.values()),
              "incompatible_metroid_cells_by_difference": rejected,
              "previously_used_seed_identifiers": sorted(seed_ids),
              "limitations": ["Empirical control calibration, not a mechanism effect or power calculation.",
                              "Different binaries are excluded even if historical short streams matched.",
                              "Long-run records contain progress intervals, not exact milestone frame times.",
                              "Observation below the common budget is incomplete, not a budgeted failure.",
                              "Historical seed allocation was exploratory; fresh calibration is reported separately."]}
    assert not args.out.exists(), "do not replace frozen evidence"
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"compatible_cells": len(accepted), "unique_seeds": len(per_seed),
                      "full_budget_seeds": sum(r["observed_full_budget"] for r in per_seed.values()),
                      "known_hits": sum(r["hit_by_budget"] is True for r in per_seed.values())}))


if __name__ == "__main__":
    main()
