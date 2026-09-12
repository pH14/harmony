#!/usr/bin/env python3
"""Retrospective exact cover of measured positive suffix events; no emulation."""
import argparse
from collections import Counter
import hashlib
from itertools import combinations
import json
from pathlib import Path

from analyze_survivor_union import analyze


def minimum_cover(features):
    union = set().union(*features)
    for size in range(len(features) + 1):
        covers = [list(indices) for indices in combinations(range(len(features)), size)
                  if set().union(*(features[i] for i in indices)) == union]
        if covers:
            return size, covers
    raise AssertionError("all offered states must cover their own union")


def positive_features(outcome, trial):
    features = {("map", trial, *point) for point in outcome["reached_maps"]}
    features |= {("equipment", trial, bit) for bit in range(8) if outcome["equipment_gained"] & (1 << bit)}
    for category, present in [("alive", not outcome["dead"]), ("boss", outcome["boss_gain"]),
                              ("capacity", outcome["capacity_gain"])]:
        if present:
            features.add((category, trial))
    return features


def cover_analysis(registration, rows):
    checked = analyze(registration, rows)  # Require complete grid and repeated replay equality.
    indexed = {(r["pair"], r["trial"]): r for r in rows}
    records = []
    for index, competition in enumerate(registration["competitions"]):
        features = [set(), set(), set()]  # Unretained, first survivor, second survivor.
        for trial in range(registration["trials_per_competition"]):
            left, right = indexed[2*index, trial], indexed[2*index+1, trial]
            for slot, outcome in enumerate([left["discarded"], left["survivor"], right["survivor"]]):
                features[slot] |= positive_features(outcome, trial)
        universe = sorted(set().union(*features))
        dictionary = {feature: i for i, feature in enumerate(universe)}
        minimum, covers = minimum_cover(features)
        current_union = features[1] | features[2]
        missed = set(universe) - current_union
        certificates = []
        if minimum == 3:
            # Excluding any state loses its unique event; these three witnesses
            # certify that no two-state subset of the offered triple suffices.
            for slot in range(3):
                others = set().union(*(features[i] for i in range(3) if i != slot))
                certificates.append({"state": slot, "unique_event": min(features[slot] - others)})
        current_missed = Counter(f[0] for f in missed)
        prior = checked["competitions"][index]["totals"]
        assert current_missed["map"] == prior["map_events"]["unretained_only"]
        assert current_missed["alive"] == prior["survival"]["unretained_only_survives"]
        records.append({"source_sample": competition["source_sample"], "execution": competition["execution"],
                        "state_roles": [competition["discarded_role"], *competition["survivor_roles"]],
                        "event_dictionary": universe,
                        "state_feature_masks_hex": [hex(sum(1 << dictionary[f] for f in state)) for state in features],
                        "minimum_representatives_for_measured_positive_events": minimum,
                        "minimum_cover_subsets": covers, "current_survivor_indices": [1, 2],
                        "current_covers_all_measured_positive_events": not missed,
                        "current_missed_by_category": dict(current_missed),
                        "three_state_necessity_certificate": certificates})
    return {"format": "u01-retrospective-finite-cover-v1",
            "scope": "exact hindsight cover of fixed measured positive events; not an online policy or behavioral equivalence",
            "minimum_representatives_histogram": dict(Counter(r["minimum_representatives_for_measured_positive_events"] for r in records)),
            "current_proposals_missing_events": sum(not r["current_covers_all_measured_positive_events"] for r in records),
            "missing_events_avoidable_with_two_of_offered_three": sum(not r["current_covers_all_measured_positive_events"] and r["minimum_representatives_for_measured_positive_events"] <= 2 for r in records),
            "competitions": records, "qualifies_longer_search": False,
            "limitations": ["Every suffix remains a distinct condition; pooling across actions would answer a different question.",
                            "Coverage preserves observed positive events, not all transitions, costs or unobserved future outcomes.",
                            "The candidate set is exactly the three offered states; another unobserved state could cover more.",
                            "This is retrospective after U01 outcomes, with no fresh performance or statistical confirmation claim.",
                            "An oracle's subset choice does not supply a cheap, generalizable way to choose that subset online.",
                            "Local proposals precede global eviction; all retained-memory and probing costs remain separate obligations."]}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--registration", type=Path, required=True)
    p.add_argument("--outcomes", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    assert not a.out.exists()
    with a.outcomes.open() as stream:
        rows = [json.loads(line) for line in stream]
    result = cover_analysis(json.loads(a.registration.read_text()), rows)
    result["registration_sha256"] = hashlib.sha256(a.registration.read_bytes()).hexdigest()
    result["outcomes_sha256"] = hashlib.sha256(a.outcomes.read_bytes()).hexdigest()
    a.out.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k not in ["competitions", "limitations"]}), flush=True)


if __name__ == "__main__":
    main()
