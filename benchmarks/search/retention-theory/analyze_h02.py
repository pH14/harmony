#!/usr/bin/env python3
"""Complete-survivor coverage at the actual one-to-six-action job horizons."""
import argparse
import copy
import hashlib
import json
from pathlib import Path

from analyze_survivor_cover import cover_analysis
from analyze_survivor_union import analyze


def analyze_prefixes(registration, full_rows, prefix_rows):
    full = {(row["pair"], row["trial"]): row for row in full_rows}
    assert len(full) == len(full_rows) == len(prefix_rows)
    seen = set()
    by_horizon = [[] for _ in range(6)]
    for row in prefix_rows:
        key = row["pair"], row["trial"]
        assert key in full and key not in seen
        seen.add(key)
        for field in ["execution", "stratum", "candidate_replaces"]:
            assert row[field] == full[key][field]
        for side in ["discarded", "survivor"]:
            prefixes = row[side]
            assert len(prefixes) == 6
            for index, value in enumerate(prefixes):
                assert 0 <= value["frames"] <= full[key][side]["frames"]
                assert set(map(tuple, value["reached_maps"])) <= set(map(tuple, full[key][side]["reached_maps"]))
                if index:
                    previous = prefixes[index-1]
                    assert value["frames"] >= previous["frames"]
                    assert set(map(tuple, previous["reached_maps"])) <= set(map(tuple, value["reached_maps"]))
                    if previous["dead"]:
                        assert value == previous, "terminal prefix changed or accumulated extra work"
                if value["dead"]:
                    assert value == full[key][side], "full run continued after terminal prefix"
        for horizon in range(6):
            by_horizon[horizon].append({**{k: row[k] for k in ["pair", "trial", "stratum", "execution", "candidate_replaces"]},
                                        "discarded": row["discarded"][horizon], "survivor": row["survivor"][horizon]})
    records, joint = [], []
    for horizon, rows in enumerate(by_horizon, 1):
        result = analyze(registration, rows)
        cover = cover_analysis(registration, rows)
        records.append({"horizon": horizon, "totals": result["totals"],
                        "competitions": result["competitions"],
                        "minimum_cover_histogram": cover["minimum_representatives_histogram"],
                        "current_proposals_missing_events": cover["current_proposals_missing_events"],
                        "avoidable_with_two": cover["missing_events_avoidable_with_two_of_offered_three"]})
        joint.extend({**row, "trial": (horizon-1)*registration["trials_per_competition"] + row["trial"]} for row in rows)
    combined = copy.deepcopy(registration)
    combined["trials_per_competition"] *= 6
    joint_cover = cover_analysis(combined, joint)
    n = registration["trials_per_competition"]
    joint_cover["condition_mapping"] = f"virtual_trial = (horizon-1)*{n} + original_trial; six dependent prefixes, not{6*n} independent trials"
    means = {category: {key: sum(r["totals"][category][key] for r in records)/6
                        for key in records[0]["totals"][category]} for category in records[0]["totals"]}
    return {"format": "h02-search-horizon-diagnostic-v1", "horizons": records,
            "uniform_length_mean_totals": means,
            "mean_scope": "average of the six horizon totals across the same selected competitors and suffix bank; not a population estimate",
            "joint_horizon_cover": joint_cover, "qualifies_longer_search": False,
            "limitations": ["Actual search length distribution is uniform1–6, but these are selected development states.",
                            "Nested horizons and repeated competitors are dependent; no confidence interval is claimed.",
                            "Positive-event cover does not prove behavioral equivalence, cheap predictability or improved adaptive search.",
                            "Prefix frame fields are cumulative; physical work is recorded once in the full-run summary."]}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--registration", type=Path, required=True)
    p.add_argument("--outcomes", type=Path, required=True)
    p.add_argument("--prefixes", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    assert not a.out.exists()
    with a.outcomes.open() as stream:
        full = [json.loads(line) for line in stream]
    with a.prefixes.open() as stream:
        prefixes = [json.loads(line) for line in stream]
    result = analyze_prefixes(json.loads(a.registration.read_text()), full, prefixes)
    result["source_sha256"] = {name: hashlib.sha256(path.read_bytes()).hexdigest()
                              for name, path in [("registration", a.registration), ("full_outcomes", a.outcomes), ("prefixes", a.prefixes)]}
    a.out.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"horizons": [{k: v for k, v in r.items() if k != "competitions"} for r in result["horizons"]],
                      "joint_minimum_cover": result["joint_horizon_cover"]["minimum_representatives_histogram"]}), flush=True)


if __name__ == "__main__":
    main()
