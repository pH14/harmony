#!/usr/bin/env python3
"""Reproduce descriptive P02 resource and spatial-refinement diagnostics."""
import json
from pathlib import Path


def main():
    root = Path(__file__).resolve().parent
    endpoints = json.loads((root / "p02-numeric-endpoints.json").read_text())
    disagreements = json.loads((root / "p02-disagreements.json").read_text())
    original = root.parent / "alternative-futures/results"
    probe = json.loads((original / "p02.json").read_text())
    outcomes = json.loads((original / "p02-per-pair.json").read_text())["pairs"]
    assert endpoints["audit_sha256"] == disagreements["audit_sha256"] == probe["audit_sha256"]
    assert len(endpoints["pairs"]) == len(outcomes) == len(disagreements["pairs"]) == 16
    rows = []
    for p, q, mismatch in zip(endpoints["pairs"], outcomes, disagreements["pairs"]):
        assert p["pair"] == q["pair"] == mismatch["pair"]
        assert p["execution"] == q["execution"] == mismatch["execution"]
        d, s = p["discarded"], p["survivor"]
        assert (d["x"] // 16, d["y"] // 16) == (s["x"] // 16, s["y"] // 16)
        dv = (d["health"] + 1) * (d["missiles"] + 1)
        sv = (s["health"] + 1) * (s["missiles"] + 1)
        # Compare exact rational rates without rounded table values.
        rate_delta = q["discarded"]["exit_trials"] * q["survivor"]["frames"] - q["survivor"]["exit_trials"] * q["discarded"]["frames"]
        split8 = (d["x"] // 8, d["y"] // 8) != (s["x"] // 8, s["y"] // 8)
        pose_split = d["pose"] != s["pose"]
        rows.append({
            "pair": p["pair"], "execution": p["execution"],
            "discarded_resources": [d["health"], d["missiles"]],
            "survivor_resources": [s["health"], s["missiles"]],
            "discarded_thresholds": dv, "survivor_thresholds": sv,
            "volume_prefers_discarded": dv > sv,
            "observed_exit_rate_prefers_discarded": rate_delta > 0,
            "volume_or_rate_tie": dv == sv or rate_delta == 0,
            "strict_preference_agreement": (dv - sv) * rate_delta > 0,
            "same_exact_position": (d["x"], d["y"]) == (s["x"], s["y"]),
            "split_by_8_pixel_position": split8, "split_by_raw_pose": pose_split,
            "split_by_8_pixel_or_raw_pose": split8 or pose_split,
            "probe_disagreements": mismatch["useful_disagreements"],
        })
    result = {
        "format": "p02-refinement-analysis-v1", "audit_sha256": endpoints["audit_sha256"],
        "outcomes_sha256": disagreements["outcomes_sha256"], "pairs": len(rows),
        "strict_volume_preference_agreements": sum(r["strict_preference_agreement"] for r in rows),
        "volume_or_rate_ties": sum(r["volume_or_rate_tie"] for r in rows),
        "discarded_exit_rate_wins": sum(r["observed_exit_rate_prefers_discarded"] for r in rows),
        "volume_prefers_discarded": sum(r["volume_prefers_discarded"] for r in rows),
        "pairs_with_distinguishing_probes": sum(r["probe_disagreements"] > 0 for r in rows),
        "refinements": {
            field: {
                "pairs_split": sum(r[field] for r in rows),
                "distinguishable_pairs_split": sum(r[field] and r["probe_disagreements"] > 0 for r in rows),
                "distinguishable_pairs_still_merged": sum(not r[field] and r["probe_disagreements"] > 0 for r in rows),
            } for field in ["split_by_8_pixel_position", "split_by_raw_pose", "split_by_8_pixel_or_raw_pose"]
        },
        "limitations": [
            "Retrospective, selected 16-pair sample; not independent fresh searches.",
            "Shared suffix trials are correlated; no significance test or confidence claim.",
            "Point volume association does not evaluate two-representative retention.",
            "Splitting a sampled pair avoids their direct competition; it does not establish future equivalence, memory cost, or campaign improvement.",
            "These local-exit probes contain no boss gains and do not identify the importance of resource thresholds for bosses.",
        ], "rows": rows,
    }
    (root / "p02-refinement-analysis.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k not in ("rows", "limitations")}))


if __name__ == "__main__":
    main()
