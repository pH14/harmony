#!/usr/bin/env python3
"""Check public finite-cover certificates with integer masks; no private inputs."""
import argparse
from collections import Counter
import json
from pathlib import Path


def verify(value):
    assert value["format"] == "u01-retrospective-finite-cover-v1"
    histogram, missing, avoidable = Counter(), 0, 0
    for record in value["competitions"]:
        dictionary = record["event_dictionary"]
        assert len({tuple(event) for event in dictionary}) == len(dictionary)
        masks = [int(mask, 16) for mask in record["state_feature_masks_hex"]]
        assert len(masks) == 3 and all(0 <= m < (1 << len(dictionary)) for m in masks)
        full = (1 << len(dictionary)) - 1
        assert masks[0] | masks[1] | masks[2] == full
        solutions = []
        for selection in range(8):
            covered = 0
            members = []
            for state, mask in enumerate(masks):
                if selection & (1 << state):
                    covered |= mask
                    members.append(state)
            if covered == full:
                solutions.append(members)
        minimum = min(map(len, solutions))
        assert minimum == record["minimum_representatives_for_measured_positive_events"]
        assert sorted(s for s in solutions if len(s) == minimum) == sorted(record["minimum_cover_subsets"])
        assert record["current_survivor_indices"] == [1, 2]
        missed = full & ~(masks[1] | masks[2])
        categories = Counter(event[0] for i, event in enumerate(dictionary) if missed & (1 << i))
        assert dict(categories) == record["current_missed_by_category"]
        assert record["current_covers_all_measured_positive_events"] == (missed == 0)
        if minimum == 3:
            certificates = record["three_state_necessity_certificate"]
            assert sorted(c["state"] for c in certificates) == [0, 1, 2]
            for certificate in certificates:
                bit = 1 << dictionary.index(certificate["unique_event"])
                assert masks[certificate["state"]] & bit
                assert all(not (mask & bit) for i, mask in enumerate(masks) if i != certificate["state"])
        histogram[str(minimum)] += 1
        missing += int(missed != 0)
        avoidable += int(missed != 0 and minimum <= 2)
    assert dict(histogram) == value["minimum_representatives_histogram"]
    assert missing == value["current_proposals_missing_events"]
    assert avoidable == value["missing_events_avoidable_with_two_of_offered_three"]
    return {"verified_competitions": sum(histogram.values()), "minimum_representatives_histogram": dict(histogram),
            "missing_events_avoidable_with_two_of_offered_three": avoidable, "emulator_work": False}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("certificate", type=Path)
    a = p.parse_args()
    print(json.dumps(verify(json.loads(a.certificate.read_text())), indent=2))


if __name__ == "__main__":
    main()
