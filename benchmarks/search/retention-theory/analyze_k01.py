#!/usr/bin/env python3
"""Compare a completed Metroid key pair at censored common work boundaries."""
import argparse
import json
from pathlib import Path

INITIAL = {"brinstar", "morph_ball"}
CEILING = 70_000_000


def at_boundary(rows, boundary):
    eligible = [row for row in rows if row["frames_emulated"] <= boundary]
    assert eligible, "no within-budget progress row"
    return eligible[-1]


def names(row):
    return {name for name, seen in row["workload_diagnostics"]["named_progress"]["first_seen"].items()
            if seen is not None}


def load(path):
    summary = json.loads(path.read_text())
    assert summary["status"] == "complete" and summary["result"]["verification"] == "witness"
    rows = [json.loads(line) for line in (path.parent / "campaign/progress.jsonl").open()]
    assert rows and all(a["frames_emulated"] <= b["frames_emulated"] for a, b in zip(rows, rows[1:]))
    intervals = {}
    previous = 0
    for row in rows:
        for name in names(row) - intervals.keys():
            intervals[name] = {"lower_exclusive": previous, "upper_inclusive": row["frames_emulated"]}
        previous = row["frames_emulated"]
    return summary, rows, intervals


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--default", type=Path, required=True)
    parser.add_argument("--refined", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    data = {label: load(path) for label, path in [("default", args.default), ("refined", args.refined)]}
    assert data["default"][0]["search_request"] == data["refined"][0]["search_request"], "comparison requests differ"
    assert data["default"][0]["search_request"]["metroid_terminal"] == "death_or_bcd_underflow_or_ending_v3"
    assert data["default"][0]["identity"]["source_tree_sha256"] == data["refined"][0]["identity"]["source_tree_sha256"], "comparison sources differ"
    common = min(CEILING, *(s["result"]["frames_emulated"] for s, _, _ in data.values()))
    boundaries = sorted({b for b in [10_000_000, 25_000_000, 50_000_000, common] if b <= common})
    report = {"scope": "development only; cumulative work counters, not route duration",
              "common_frame_ceiling": common, "arms": {}}
    final_names = {}
    for label, (summary, rows, intervals) in data.items():
        final = at_boundary(rows, common)
        final_names[label] = names(final)
        witnessed = summary["result"]["milestone_witnesses"]
        assert final_names[label] <= witnessed.keys(), "unverified named discovery"
        census = summary["last_progress"].get("retained_diagnostics")
        assert summary["last_progress"]["workload_diagnostics"]["underflow_candidate_eligible_endpoints"] == 0, "invalid terminal state became eligible"
        if census:
            assert census["underflow_endpoints_cached"] == 0, "invalid terminal state remains in the archive"
        retention = final["retention_diagnostics"]
        report["arms"][label] = {
            "summary_path": str(args.default if label == "default" else args.refined),
            "request": summary["search_request"], "identity": summary["identity"],
            "build": summary["build"],
            "result": summary["result"], "elapsed_seconds": summary["elapsed_seconds"],
            "peak_rss_bytes": summary["peak_process_tree_rss_bytes_sampled"],
            "milestone_frame_intervals": intervals,
            "unselected_removal_fraction_at_common_boundary":
                retention["removed_never_selected"] / retention["removed"] if retention["removed"] else None,
            "final_retained_census": census,
            "checkpoints": [{"boundary": b, "row": at_boundary(rows, b)} for b in boundaries],
        }
    speedups = []
    for name in (final_names["default"] & final_names["refined"]) - INITIAL:
        control, candidate = data["default"][2][name], data["refined"][2][name]
        # Use the candidate's latest possible arrival and control's earliest
        # possible arrival, so telemetry granularity cannot fabricate the gate.
        if 5 * candidate["upper_inclusive"] <= 4 * control["lower_exclusive"]:
            speedups.append(name)
    extra = sorted(final_names["refined"] - final_names["default"] - INITIAL)
    lost = sorted(final_names["default"] - final_names["refined"] - INITIAL)
    report["decision"] = {"additional_refined_names": extra, "additional_default_names": lost,
                          "conservative_twenty_percent_speedups": sorted(speedups),
                          "qualifies_bounded_replication": bool(extra or speedups),
                          "qualifies_breakthrough": False}
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["decision"], indent=2))


if __name__ == "__main__":
    main()
