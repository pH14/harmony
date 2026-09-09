#!/usr/bin/env python3
"""Action-conditioned unretained futures versus the complete local survivor union."""
import argparse
import hashlib
import json
from pathlib import Path


def coverage(discarded, survivors, candidate_survivor_index):
    assert len(survivors) == 2
    union = survivors[0] | survivors[1]
    pairwise = (discarded - survivors[0]) | (discarded - survivors[1])
    unretained = discarded - union
    if candidate_survivor_index is None:
        old = union  # A rejected candidate never changed the incumbent set.
    else:
        assert candidate_survivor_index in (0, 1)
        old = discarded | survivors[1 - candidate_survivor_index]
    return {"unretained_only": len(unretained),
            "pairwise_unique_before_union": len(pairwise),
            "covered_by_other_survivor": len(pairwise - unretained),
            "retained_union_only": len(union - discarded),
            "actual_local_replacement_gained": len(union - old),
            "actual_local_replacement_lost": len(old - union)}


def capabilities(outcome):
    events = {f"equipment_bit_{bit}" for bit in range(8) if outcome["equipment_gained"] & (1 << bit)}
    if outcome["boss_gain"]:
        events.add("any_boss_gain")
    if outcome["capacity_gain"]:
        events.add("any_capacity_gain")
    return events


def compare(discarded, survivors, candidate_survivor_index):
    maps = lambda o: {tuple(point) for point in o["reached_maps"]}
    return {"map_events": coverage(maps(discarded), [maps(s) for s in survivors], candidate_survivor_index),
            "capability_events": coverage(capabilities(discarded), [capabilities(s) for s in survivors], candidate_survivor_index),
            "survival": {"unretained_alive": int(not discarded["dead"]),
                         "any_retained_alive": int(any(not s["dead"] for s in survivors)),
                         "unretained_only_survives": int(not discarded["dead"] and all(s["dead"] for s in survivors)),
                         "retained_only_survives": int(discarded["dead"] and any(not s["dead"] for s in survivors))}}


def analyze(registration, rows):
    trials = registration["trials_per_competition"]
    count = len(registration["competitions"])
    indexed = {}
    for row in rows:
        key = row["pair"], row["trial"]
        assert row["stratum"] == 3 and not row["candidate_replaces"]
        assert key not in indexed and 0 <= key[0] < 2 * count and 0 <= key[1] < trials
        indexed[key] = row
    assert len(indexed) == 2 * count * trials
    result = {"format": "u01-complete-local-survivor-analysis-v1",
              "scope": "selected development competitions; local proposals before global eviction",
              "competitions": [], "totals": {}, "qualifies_longer_search": False}
    for i, competition in enumerate(registration["competitions"]):
        candidate_index = competition["candidate_survivor_index"]
        record = {"source_sample": competition["source_sample"],
                  "execution": competition["execution"],
                  "candidate_admitted": candidate_index is not None,
                  "totals": {}, "trials_with_unretained_map_events": 0,
                  "trials_with_pairwise_map_difference_covered_by_union": 0}
        for trial in range(trials):
            left, right = indexed[2*i, trial], indexed[2*i+1, trial]
            assert left["execution"] == right["execution"] == competition["execution"]
            assert left["discarded"] == right["discarded"], "repeated unretained replay differs"
            values = compare(left["discarded"], [left["survivor"], right["survivor"]], candidate_index)
            record["trials_with_unretained_map_events"] += int(values["map_events"]["unretained_only"] > 0)
            record["trials_with_pairwise_map_difference_covered_by_union"] += int(
                values["map_events"]["covered_by_other_survivor"] > 0)
            for category, fields in values.items():
                for name, value in fields.items():
                    for totals in [record["totals"], result["totals"]]:
                        group = totals.setdefault(category, {})
                        group[name] = group.get(name, 0) + value
        result["competitions"].append(record)
    result["limitations"] = [
        "Shared suffixes preserve action conditions; finite failures do not certify equivalence.",
        "Local-rule proposal only; the audit callback precedes global population and memory eviction.",
        "Map events are local living visits relative to each start, not new global search coverage.",
        "Capability event categories and endpoint survival remain separate; no utility weights were fitted.",
        "Rejected-candidate opportunity loss is distinct from an actual replacement of an incumbent.",
        "A fixed, availability/work-limited development sample supplies no population or breakthrough estimate."]
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--registration", type=Path, required=True)
    p.add_argument("--outcomes", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    assert not a.out.exists()
    registration = json.loads(a.registration.read_text())
    with a.outcomes.open() as f:
        result = analyze(registration, [json.loads(line) for line in f])
    result["registration_sha256"] = hashlib.sha256(a.registration.read_bytes()).hexdigest()
    result["outcomes_sha256"] = hashlib.sha256(a.outcomes.read_bytes()).hexdigest()
    a.out.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
