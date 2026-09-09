#!/usr/bin/env python3
"""Require fresh evidence against capacity and ordinary controls at common work."""
import argparse
import hashlib
import json
from pathlib import Path
from analyze_k01 import INITIAL, at_boundary, load, names
from run_r05 import ARMS, BINARY, SEED


def analyze(root):
    data, paths, normalized = {}, {}, {"search_request": [], "identity": []}
    for label, policy in ARMS:
        matches = list((root / f"runs/r05-{label}").glob("*/summary.json"))
        assert len(matches) == 1
        paths[label] = matches[0]
        summary, rows, intervals = load(matches[0])
        data[label] = summary, rows, intervals
        assert summary["build"]["binary_sha256"] == BINARY
        assert summary["search_request"]["seed"] == SEED
        assert summary["search_request"]["metroid_terminal"] == "death_or_bcd_underflow_or_ending_v3"
        for field in normalized:
            value = dict(summary[field])
            assert value.pop("slot_retention", None) == policy
            normalized[field].append(value)
        assert summary["identity"]["policies"]["key_policy"] == "metroid_items_tanks_spatial_16_posture_motion_context_selection_32_legacy_progress_v10"
        assert summary["last_progress"]["workload_diagnostics"]["underflow_candidate_eligible_endpoints"] == 0
        assert summary["last_progress"]["retained_diagnostics"]["underflow_endpoints_cached"] == 0
    for field, values in normalized.items():
        assert values[0] == values[1] == values[2], f"unmatched {field}"
    common = min(50_000_000, *(s["result"]["frames_emulated"] for s, _, _ in data.values()))
    final_names = {label: names(at_boundary(rows, common)) for label, (_, rows, _) in data.items()}
    report = {"format": "r05-fresh-context-replication-analysis-v1", "seed": SEED,
              "scope": "one fresh development seed, not held-out validation",
              "common_frame_ceiling": common, "arms": {}, "comparisons": {}}
    censored = [label for label, (summary, _, _) in data.items() if summary["result"]["stop_reason"] == "wall_limit"]
    for label, (summary, rows, intervals) in data.items():
        assert final_names[label] <= summary["result"]["milestone_witnesses"].keys()
        report["arms"][label] = {"summary_sha256": hashlib.sha256(paths[label].read_bytes()).hexdigest(),
                                  "request": summary["search_request"], "identity": summary["identity"],
                                  "build": summary["build"], "result": summary["result"],
                                  "final_names_at_common_work": sorted(final_names[label]),
                                  "milestone_frame_intervals": intervals,
                                  "peak_rss_bytes": summary["peak_process_tree_rss_bytes_sampled"],
                                  "retained_census": summary["last_progress"]["retained_diagnostics"],
                                  "context_census": summary["last_progress"].get("retention_context_census")}
    for control in ["ordinary", "quality"]:
        extra = sorted(final_names["context"] - final_names[control] - INITIAL)
        lost = sorted(final_names[control] - final_names["context"] - INITIAL)
        speedups = sorted(name for name in {"missile_capacity", "norfair", "energy_tank"}
                          & final_names[control] & final_names["context"]
                          if 5 * data["context"][2][name]["upper_inclusive"] <= 4 * data[control][2][name]["lower_exclusive"])
        report["comparisons"][control] = {"additional_context_names": extra, "additional_control_names": lost,
                                           "conservative_twenty_percent_speedups": speedups,
                                           "passes": not censored and not lost and bool(extra or speedups)}
    report["decision"] = {"wall_censored_arms": censored,
                          "replicates_capacity_control_advantage": report["comparisons"]["quality"]["passes"],
                          "improves_over_ordinary_retention": report["comparisons"]["ordinary"]["passes"],
                          "qualifies_one_longer_development_pair": all(c["passes"] for c in report["comparisons"].values()),
                          "qualifies_breakthrough": False}
    return report


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    report = analyze(a.experiment.resolve())
    a.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["decision"], indent=2))
