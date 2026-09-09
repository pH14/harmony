#!/usr/bin/env python3
"""Describe exact input-prefix relations without exporting the input tapes."""
import argparse
import hashlib
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audit", type=Path, required=True)
    parser.add_argument("--registration", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    assert not args.out.exists()
    registration = json.loads(args.registration.read_text())
    digest = hashlib.sha256(args.audit.read_bytes()).hexdigest()
    assert digest == registration["source_audit_sha256"]
    audit = json.loads(args.audit.read_text())
    records = []
    for item in registration["competitions"]:
        pair = audit["samples"][3][item["source_sample"]]
        states = {"candidate": pair["candidate_input"]["actions"]}
        states.update({f"incumbent_{m['id']}": m["input"]["actions"] for m in pair["complete_competition"]["incumbents"]})
        discarded = states[item["discarded_role"]]
        comparisons = []
        for role in item["survivor_roles"]:
            survivor = states[role]
            common = 0
            for left, right in zip(discarded, survivor):
                if left != right:
                    break
                common += 1
            prefix = len(survivor) < len(discarded) and common == len(survivor)
            comparisons.append({"survivor_role": role, "survivor_actions": len(survivor),
                                "common_prefix_actions": common, "survivor_is_strict_input_prefix": prefix,
                                "recorded_tail_actions_if_prefix": len(discarded)-len(survivor) if prefix else None})
        records.append({"source_sample": item["source_sample"], "discarded_actions": len(discarded), "comparisons": comparisons})
    result = {"format": "u01-input-prefix-relations-v1", "source_audit_sha256": digest,
              "scope": "exact recorded input prefixes only; no claim about other routes or a waiting-state mechanism",
              "emulator_work": False, "records": records}
    args.out.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
