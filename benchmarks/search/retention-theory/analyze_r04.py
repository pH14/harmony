#!/usr/bin/env python3
"""Evaluate R04 without mistaking retained-context counts for progress."""
import argparse
import hashlib
import json
from pathlib import Path
from analyze_k01 import INITIAL, at_boundary, load, names
from run_r04 import CONTEXT, QUALITY, TERMINAL


def analyze(root):
    data, paths = {}, {}
    for label in ["quality", "context"]:
        matches = list((root / f"runs/r04b-development-{label}").glob("*/summary.json"))
        assert len(matches) == 1
        paths[label] = matches[0]
        data[label] = load(matches[0])
    for field in ["search_request", "identity"]:
        normalized = []
        for label, policy in [("quality", QUALITY), ("context", CONTEXT)]:
            value = dict(data[label][0][field])
            assert value.pop("slot_retention") == policy
            normalized.append(value)
        assert normalized[0] == normalized[1], f"unmatched {field}"
    assert data["quality"][0]["build"]["binary_sha256"] == data["context"][0]["build"]["binary_sha256"]
    common = min(50_000_000, *(s["result"]["frames_emulated"] for s, _, _ in data.values()))
    report = {"format": "r04-motion-context-analysis-v1", "scope": "one fresh development seed; no validation claim",
              "common_frame_ceiling": common, "requested_frame_ceiling": 50_000_000, "arms": {}}
    final_names = {}
    for label, (summary, rows, intervals) in data.items():
        assert summary["search_request"]["metroid_terminal"] == TERMINAL
        assert summary["identity"]["policies"]["key_policy"] == "metroid_items_tanks_spatial_16_posture_motion_context_selection_32_legacy_progress_v10"
        assert "result_digest=metroid-semantic-postcard-1.1.3-sha256-hex-motion-v5;" in summary["identity"]["policies"]["emulator_backend"]
        final_names[label] = names(at_boundary(rows, common))
        witnesses = summary["result"]["milestone_witnesses"]
        assert final_names[label] <= witnesses.keys()
        assert summary["last_progress"]["workload_diagnostics"]["underflow_candidate_eligible_endpoints"] == 0
        assert summary["last_progress"]["retained_diagnostics"]["underflow_endpoints_cached"] == 0
        physical = summary["result"]["frames_emulated"] + 2 * summary["result"]["witness"]["physical_suffix_frames"]
        physical += sum(2 * witness["replay"]["physical_suffix_frames"] for witness in witnesses.values())
        report["arms"][label] = {
            "summary_sha256": hashlib.sha256(paths[label].read_bytes()).hexdigest(),
            "request": summary["search_request"], "identity": summary["identity"], "build": summary["build"],
            "result": summary["result"], "milestone_frame_intervals": intervals,
            "final_names_at_common_work": sorted(final_names[label]),
            "peak_rss_bytes": summary["peak_process_tree_rss_bytes_sampled"],
            "retained_census": summary["last_progress"]["retained_diagnostics"],
            "physical_frames_lower_bound": physical,
            "physical_cost_limitations": "Includes admitted search and two final/milestone suffix replays; setup and unadmitted worker work are additional.",
            "checkpoints": [{"boundary": n, "row": at_boundary(rows, n)}
                            for n in sorted({n for n in [5_000_000, 10_000_000, 25_000_000, common] if n <= common})]}
    extra = sorted(final_names["context"] - final_names["quality"] - INITIAL)
    lost = sorted(final_names["quality"] - final_names["context"] - INITIAL)
    speedups = sorted(name for name in {"missile_capacity", "norfair", "energy_tank"}
                      & final_names["quality"] & final_names["context"]
                      if 5 * data["context"][2][name]["upper_inclusive"] <= 4 * data["quality"][2][name]["lower_exclusive"])
    censored = [label for label, (summary, _, _) in data.items() if summary["result"]["stop_reason"] == "wall_limit"]
    report["decision"] = {"additional_context_names": extra, "additional_quality_names": lost,
                          "conservative_twenty_percent_speedups": speedups, "wall_censored_arms": censored,
                          "qualifies_one_new_development_seed": not censored and not lost and bool(extra or speedups),
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
