#!/usr/bin/env python3
"""Reproduce the finite paired-outcome analysis from private raw probe output."""
import argparse
import hashlib
import json
from pathlib import Path


def analyze(root, outcomes):
    def read(name):
        return json.loads((root / name).read_text())

    metadata = read("p03-provenance.json")
    summary = read("p03-results.json")
    partitions = [read(f"p03-{mode}-partition.json") for mode in ["default", "refined"]]
    numeric = root / "p03-numeric-endpoints.json"
    audit = metadata["selected_audit_sha256"]
    assert summary["audit_sha256"] == audit
    assert summary["terminal_policy"] == "death_or_bcd_underflow_or_ending_v3"
    assert summary["seed"] == 20261201 and summary["trials_per_pair"] == 64
    for partition in partitions:
        assert partition["source_audit_sha256"] == audit
        assert partition["input_sha256"] == hashlib.sha256(numeric.read_bytes()).hexdigest()
        assert partition["all_coarser_groups_equal"]
    assert partitions[0]["retention_splits"] == 0
    raw = outcomes.read_bytes()
    trials = [json.loads(line) for line in raw.splitlines()]
    assert len(trials) == 16 * 64
    rows = []
    for pair in metadata["pairs"]:
        index, execution = pair["pair"], pair["execution"]
        subset = [t for t in trials if t["pair"] == index]
        assert len(subset) == 64 and {t["trial"] for t in subset} == set(range(64))
        assert all(t["execution"] == execution and t["stratum"] == 3 for t in subset)
        a, b = pair["candidate"], pair["incumbent"]
        for key in ["health", "missiles", "equipment", "bosses", "missile_capacity", "energy_tanks"]:
            assert a[key] == b[key]
        compiled = partitions[1]["rows"][index]
        assert compiled["pair"] == index and compiled["execution"] == execution
        rows.append({
            "pair": index, "execution": execution,
            "useful_disagreements": sum(t["discarded_only_exit"] or t["survivor_only_exit"] or t["discarded"]["dead"] != t["survivor"]["dead"] for t in subset),
            "discarded_only_exits": sum(t["discarded_only_exit"] for t in subset),
            "survivor_only_exits": sum(t["survivor_only_exit"] for t in subset),
            "discarded_survives_survivor_dies": sum(not t["discarded"]["dead"] and t["survivor"]["dead"] for t in subset),
            "survivor_survives_discarded_dies": sum(not t["survivor"]["dead"] and t["discarded"]["dead"] for t in subset),
            "discarded_frames": sum(t["discarded"]["frames"] for t in subset),
            "survivor_frames": sum(t["survivor"]["frames"] for t in subset),
            "split_by_8_pixel_position": (a["x"] // 8, a["y"] // 8) != (b["x"] // 8, b["y"] // 8),
            "split_by_raw_pose": a["pose"] != b["pose"],
            "compiled_refinement_splits": compiled["retention_split"],
            "candidate_extends_incumbent": pair["candidate_extends_incumbent"],
            "incumbent_admitted_parent_selections": pair["exposure"]["selected"],
        })
    assert [sum(row[f"{side}_frames"] for row in rows) for side in ["discarded", "survivor"]] == summary["probe_frames_discarded_survivor"]
    return {
        "format": "p03-paired-alias-analysis-v1", "outcomes_sha256": hashlib.sha256(raw).hexdigest(),
        "audit_sha256": audit, "pairs": len(rows),
        "pairs_with_useful_disagreement": sum(r["useful_disagreements"] > 0 for r in rows),
        "distinguishable_pairs_split": sum(r["useful_disagreements"] > 0 and r["compiled_refinement_splits"] for r in rows),
        "distinguishable_pairs_still_merged": sum(r["useful_disagreements"] > 0 and not r["compiled_refinement_splits"] for r in rows),
        "physical_frames": summary["prefix_and_gain_export_frames"] + sum(summary["probe_frames_discarded_survivor"]),
        "limitations": [
            "Selected finite equal-preference sample, with shared suffixes; no confidence or population-rate claim.",
            "Local exits and survival are not boss attainment.",
            "Splitting these competitors does not establish a beneficial fresh campaign.",
        ], "rows": rows,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--research", type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument("--outcomes", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.write_text(json.dumps(analyze(args.research, args.outcomes), indent=2) + "\n")
