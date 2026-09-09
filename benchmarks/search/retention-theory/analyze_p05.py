#!/usr/bin/env python3
"""Describe frozen motion splits on P05; never reopen the failed search gate."""
import argparse
from collections import Counter
import json
from pathlib import Path
from analyze_probe_objectives import totals
from run_p05 import ACTIONS, SEED, SOURCE_AUDIT, TRIALS, sha


def analyze(root):
    out = root / "runs/p05"
    record = json.loads((out / "results.json").read_text())
    assert record["complete"] and record["source_audit_sha256"] == SOURCE_AUDIT
    assert record["subset_audit_sha256"] == sha(root / "p05-audit.json")
    motion, suffix = [x["summary"] for x in record["records"]]
    assert motion["verified_replays_per_endpoint"] == 2 and motion["pairs"] == 16
    assert motion["read_only_snapshot_checks"] and motion["cached_context_matches_direct_read"] and motion["campaign_key_context_checked"]
    assert suffix["seed"] == SEED and suffix["trials_per_pair"] == TRIALS and suffix["actions_per_trial"] == ACTIONS
    assert suffix["suffix_sha256"] == sha(out / "suffix/suffixes.json")
    assert all(x["audit_sha256"] == record["subset_audit_sha256"] for x in [motion, suffix])
    endpoints = [json.loads(line) for line in (out / "motion/pairs.jsonl").read_text().splitlines()]
    trials = [json.loads(line) for line in (out / "suffix/outcomes.jsonl").read_text().splitlines()]
    assert len(endpoints) == 16 and len(trials) == 16 * TRIALS
    pairs = json.loads((root / "p05-audit.json").read_text())["samples"][3]
    rows = []
    for index, pair in enumerate(pairs):
        endpoint = endpoints[index]
        assert endpoint["stratum"] == 3 and endpoint["pair"] == index and endpoint["execution"] == pair["execution"]
        subset = [row for row in trials if row["pair"] == index]
        assert len(subset) == TRIALS and {row["trial"] for row in subset} == set(range(TRIALS))
        assert all(row["stratum"] == 3 and row["execution"] == pair["execution"] and row["candidate_replaces"] == pair["replaces"] for row in subset)
        for side in ["candidate", "incumbent"]:
            assert endpoint[side]["endpoint"] == pair[side]
        useful = sum(row["discarded_only_exit"] or row["survivor_only_exit"] or row["discarded"]["dead"] != row["survivor"]["dead"]
                     or row["discarded_only_gain"] or row["survivor_only_gain"] for row in subset)
        rows.append({"pair": index, "execution": pair["execution"],
                     "motion_contexts": {side: endpoint[side]["cached_context"] for side in ["candidate", "incumbent"]},
                     "motion_contexts_differ": endpoint["candidate"]["cached_context"] != endpoint["incumbent"]["cached_context"],
                     "raw_seven_motion_fields_equal": endpoint["candidate"]["motion"] == endpoint["incumbent"]["motion"],
                     "mechanical_endpoints_equal": pair["candidate"] == pair["incumbent"],
                     "useful_disagreements": useful,
                     "discarded_only_gain_trials": sum(row["discarded_only_gain"] for row in subset),
                     "survivor_only_gain_trials": sum(row["survivor_only_gain"] for row in subset), **totals(subset)})
    counts = Counter((row["motion_contexts_differ"], row["useful_disagreements"] > 0) for row in rows)
    return {"format": "p05-new-seed-descriptor-analysis-v1", "scope": "post-failure diagnostic; no fresh-search or population-rate claim",
            "source_audit_sha256": SOURCE_AUDIT, "subset_audit_sha256": record["subset_audit_sha256"],
            "outcomes_sha256": sha(out / "suffix/outcomes.jsonl"), "motion_rows_sha256": sha(out / "motion/pairs.jsonl"),
            "suffix_sha256": suffix["suffix_sha256"], "suffix_seed": SEED, "trials_per_pair": TRIALS,
            "physical_frames": record["physical_frames"], "work_bound": record["suffix_work_bound"],
            "contingency": [{"motion_contexts_differ": split, "useful_disagreement_observed": different,
                              "pairs": counts[(split, different)]} for split in [False, True] for different in [False, True]],
            "aggregate": totals(trials), "rows": rows,
            "decision": {"reopens_motion_retention": False, "qualifies_longer_search": False, "qualifies_breakthrough": False},
            "limitations": ["All sixteen equal-preference audit pairs in recorded order; no pair chosen by its probe outcome.",
                            "Shared sixteen suffixes, one development campaign and a post-failure question; no confidence or population-rate claim.",
                            "Matching finite suffixes never establishes equivalent futures or descriptor sufficiency.",
                            "Local map events and survival are separate objectives; capability-gain flags here are diagnostic observations.",
                            "The source ordinary policy has one incumbent; this does not measure marginal coverage against two survivors."]}


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    result = analyze(a.experiment.resolve())
    a.out.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"contingency": result["contingency"], "physical_frames": result["physical_frames"], "decision": result["decision"]}, indent=2))
