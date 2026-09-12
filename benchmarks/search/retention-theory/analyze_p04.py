#!/usr/bin/env python3
"""Compare preregistered motion descriptors with existing P03 disagreements."""
import argparse
import hashlib
import json
from pathlib import Path


def sign(byte):
    signed = byte if byte < 128 else byte - 256
    return (signed > 0) - (signed < 0)


def context(motion):
    return [motion["direction"], sign(motion["horizontal_speed"]), sign(motion["vertical_speed"])]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--research", type=Path, default=Path(__file__).resolve().parent)
    p.add_argument("--probe", type=Path, required=True)
    p.add_argument("--p03-outcomes", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    read = lambda path: json.loads(path.read_text())
    prior = read(a.research / "p03-analysis.json")
    metadata = read(a.research / "p03-provenance.json")
    summary = read(a.probe / "summary.json")
    raw = (a.probe / "pairs.jsonl").read_bytes()
    rows = [json.loads(line) for line in raw.splitlines()]
    trials_bytes = a.p03_outcomes.read_bytes()
    assert hashlib.sha256(trials_bytes).hexdigest() == prior["outcomes_sha256"]
    trials = [json.loads(line) for line in trials_bytes.splitlines()]
    assert summary["audit_sha256"] == prior["audit_sha256"] == metadata["selected_audit_sha256"]
    assert summary["pairs"] == 16 == len(rows)
    assert summary["verified_replays_per_endpoint"] == 2 and summary["read_only_snapshot_checks"]
    assert summary["terminal_policy"] == "death_or_bcd_underflow_or_ending_v3"
    assert rows[-1]["cumulative_physical_frames"] == summary["physical_frames"]
    still_merged = {r["pair"] for r in prior["rows"] if r["useful_disagreements"] and not r["compiled_refinement_splits"]}
    assert len(still_merged) == 8
    result = []
    examples = []
    for index, row in enumerate(rows):
        previous, provenance = prior["rows"][index], metadata["pairs"][index]
        assert row["pair"] == index and row["stratum"] == 3
        assert row["execution"] == previous["execution"] == provenance["execution"]
        assert row["candidate_replaces"] == provenance["replaces"]
        assert row["verified_replays_per_endpoint"] == 2
        c, i = row["candidate"], row["incumbent"]
        assert c["endpoint"] == provenance["candidate"] and i["endpoint"] == provenance["incumbent"]
        assert c["motion"]["direction"] in (0, 1) and i["motion"]["direction"] in (0, 1)
        cc, ic = context(c["motion"]), context(i["motion"])
        same_observation = c["endpoint"] == i["endpoint"]
        record = {"pair": index, "execution": row["execution"],
                  "useful_disagreements": previous["useful_disagreements"],
                  "still_merged_by_finer_key": index in still_merged,
                  "same_mechanical_state": same_observation,
                  "candidate_context": cc, "incumbent_context": ic,
                  "coarse_motion_splits": cc != ic, "raw_motion_splits": c["motion"] != i["motion"],
                  "candidate_motion": c["motion"], "incumbent_motion": i["motion"]}
        if same_observation and cc != ic and previous["useful_disagreements"]:
            trial = next(t for t in trials if t["pair"] == index and
                         (t["discarded_only_exit"] or t["survivor_only_exit"] or t["discarded"]["dead"] != t["survivor"]["dead"]))
            examples.append({"pair": index, "trial": trial["trial"], "outcome": trial})
        result.append(record)
    separated = [r["pair"] for r in result if r["still_merged_by_finer_key"] and r["coarse_motion_splits"]]
    report = {"format": "p04-motion-partition-analysis-v1",
              "scope": "adaptive descriptor diagnostic on existing P03 pairs; no causal sufficiency or population claim",
              "probe_summary": summary, "pairs_sha256": hashlib.sha256(raw).hexdigest(),
              "p03_outcomes_sha256": prior["outcomes_sha256"],
              "coarse_context": ["direction", "signed horizontal speed sign", "signed vertical speed sign"],
              "still_merged_useful_pairs": sorted(still_merged),
              "coarse_motion_separates_still_merged": separated,
              "same_observation_distinguishing_examples": examples,
              "rows": result,
              "decision": {"at_least_six_of_eight_separated": len(separated) >= 6,
                           "qualifies_memory_bounded_retention_design": len(separated) >= 6 and bool(examples),
                           "qualifies_fresh_campaign": False, "qualifies_breakthrough": False},
              "limitations": ["Motion covaries with other hidden machine state; correlation does not identify the cause of a divergent future.",
                              "The seven-byte raw tuple is descriptive only and cannot rescue a failed coarse-context gate.",
                              "No new suffix trials, fresh search, or default changes."]}
    a.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["decision"], indent=2))


if __name__ == "__main__":
    main()
