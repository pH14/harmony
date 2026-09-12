#!/usr/bin/env python3
"""Describe the complete registered control panel without dropping censoring."""
import argparse
import json
import math
from pathlib import Path


def summarize(records, expected_seeds, budget):
    assert len(records) == len(expected_seeds) == 4, "the fixed panel is incomplete"
    costs, hits, rows = [], [], []
    for index, (record, seed) in enumerate(zip(records, expected_seeds)):
        assert record["index"] == index and record["seed"] == seed
        evidence = record["endpoint_evidence"]
        assert evidence["observed_full_budget"] and evidence["budget_frames"] == budget
        assert evidence["restricted_cost_interval"] is not None
        lower, upper = evidence["restricted_cost_interval"]
        assert 0 <= lower <= upper <= budget
        costs.append((lower, upper))
        hits.append(evidence["hit_by_budget"])
        rows.append({"seed": seed, "hit_by_budget": evidence["hit_by_budget"],
                     "arrival_interval": evidence["arrival_interval"],
                     "restricted_cost_interval": [lower, upper]})
    cv = None
    if all(hit is True for hit in hits) and all(cost[0] > 0 for cost in costs):
        # For n observations, sample variance equals the sum of pairwise
        # squared distances / (n*(n-1)). Pairwise interval distance bounds
        # remain valid even when their extrema cannot occur jointly.
        pair_lower = pair_upper = 0
        for i, (lo, hi) in enumerate(costs):
            for other_lo, other_hi in costs[i + 1:]:
                pair_lower += max(0, lo - other_hi, other_lo - hi) ** 2
                pair_upper += max(abs(lo - other_hi), abs(hi - other_lo)) ** 2
        divisor = len(costs) * (len(costs) - 1)
        mean_lower = sum(c[0] for c in costs) / len(costs)
        mean_upper = sum(c[1] for c in costs) / len(costs)
        cv = [math.sqrt(pair_lower / divisor) / mean_upper,
              math.sqrt(pair_upper / divisor) / mean_lower]
    return {"sample_size": len(rows), "known_hits": sum(hit is True for hit in hits),
            "known_nonattainments": sum(hit is False for hit in hits),
            "budget_boundary_uncertain_hits": sum(hit is None for hit in hits),
            "hit_fraction_interval": [sum(hit is True for hit in hits) / len(hits),
                                      sum(hit is not False for hit in hits) / len(hits)],
            "mean_restricted_cost_interval": [sum(cost[side] for cost in costs) / len(costs)
                                               for side in (0, 1)],
            "observed_restricted_cost_envelope": [min(c[0] for c in costs), max(c[1] for c in costs)],
            "rows": rows,
            "cv_of_uncensored_milestone_time": cv,
            "cv_note": "Descriptive sample-CV interval bounds only when every fixed-panel outcome is attained before the cap; not a confidence interval, population CV, or sample-size rule."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--registration", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    result = json.loads(args.results.read_text())
    registration = json.loads(args.registration.read_text())
    assert result["execution_complete"] and result["allocation_stop"] is None
    seeds = [row["seed"] for row in registration["seeds"]]
    descriptive = summarize(result["records"], seeds, registration["budget_frames_per_cell"])
    nominal = len(seeds) * registration["budget_frames_per_cell"]
    report = {"format": "continuation-yield-d01-analysis-v1", "panel": "fresh controls only",
              "endpoint": registration["endpoint"], "nominal_admitted_frame_ceiling": nominal,
              "actual_admitted_frames": result["actual_admitted_frames"],
              "bounded_inflight_drain_frames": result["actual_admitted_frames"] - nominal,
              **descriptive,
              "interpretation": ["Control dispersion at one fixed restricted horizon; not a mechanism effect.",
                                 "Four seeds do not establish statistical power for a 15% or 20% effect.",
                                 "The historical compatible control is kept separate from this prospectively registered panel.",
                                 "This panel does not reopen any failed retention gate or count as held-out validation."]}
    assert not args.out.exists(), "do not replace frozen analysis"
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
