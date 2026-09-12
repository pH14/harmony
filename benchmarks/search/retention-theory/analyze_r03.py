#!/usr/bin/env python3
"""Compare R03's two retention policies without pooling unequal work."""
import argparse
import json
from pathlib import Path
from analyze_k01 import INITIAL, at_boundary, load, names
from run_r03 import EXTREMES, SAMPLE, TERMINAL


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    parser.add_argument("--game", choices=["metroid", "mm2"], required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--run-id", default="r03")
    args = parser.parse_args()
    data = {}
    for label, policy in [("sample", SAMPLE), ("extremes", EXTREMES)]:
        paths = list((args.experiment / "runs" / f"{args.run_id}-{args.game}-{args.game}-{policy}").glob("*/summary.json"))
        assert len(paths) == 1
        path = paths[0]
        if args.game == "metroid":
            data[label] = load(path)
        else:
            summary = json.loads(path.read_text())
            assert summary["status"] == "complete" and summary["result"]["verification"] == "witness"
            rows = [json.loads(line) for line in (path.parent / "campaign/progress.jsonl").open()]
            assert rows and all(a["frames_emulated"] <= b["frames_emulated"] for a, b in zip(rows, rows[1:]))
            data[label] = summary, rows, {}
    requests = []
    for label, policy in [("sample", SAMPLE), ("extremes", EXTREMES)]:
        request = dict(data[label][0]["search_request"])
        assert request.pop("slot_retention") == policy
        requests.append(request)
    assert requests[0] == requests[1], "not an isolated retention comparison"
    a, b = (data[label][0] for label in ["sample", "extremes"])
    identities = []
    for summary, policy in [(a, SAMPLE), (b, EXTREMES)]:
        identity = dict(summary["identity"])
        assert identity.pop("slot_retention") == policy
        identities.append(identity)
    assert identities[0] == identities[1], "game/core/source identities differ"
    assert a["build"]["binary_sha256"] == b["build"]["binary_sha256"]
    ceiling = 50_000_000 if args.game == "metroid" else 12_000_000
    common = min(ceiling, a["result"]["frames_emulated"], b["result"]["frames_emulated"])
    censored = [label for label, (s, _, _) in data.items() if s["result"]["stop_reason"] == "wall_limit"]
    report = {"format": "r03-matched-retention-analysis-v1", "game": args.game,
              "scope": "development seeds only; no breakthrough qualification",
              "requested_frame_ceiling": ceiling, "common_frame_ceiling": common,
              "wall_censored_cells": censored, "arms": {}}
    final_names = {}
    for label, (summary, rows, intervals) in data.items():
        checkpoints = sorted({n for n in [5_000_000, 10_000_000, 25_000_000, 50_000_000, common] if n <= common})
        final = at_boundary(rows, common)
        report["arms"][label] = {
            "request": summary["search_request"], "identity": summary["identity"], "build": summary["build"],
            "result": summary["result"], "elapsed_seconds": summary["elapsed_seconds"],
            "peak_rss_bytes": summary["peak_process_tree_rss_bytes_sampled"],
            "milestone_frame_intervals": intervals,
            "checkpoints": [{"boundary": n, "row": at_boundary(rows, n)} for n in checkpoints],
            "final_retained_census": summary["last_progress"].get("retained_diagnostics"),
        }
        if args.game == "metroid":
            assert summary["search_request"]["metroid_terminal"] == TERMINAL
            assert summary["last_progress"]["workload_diagnostics"]["underflow_candidate_eligible_endpoints"] == 0
            assert summary["last_progress"]["retained_diagnostics"]["underflow_endpoints_cached"] == 0
            final_names[label] = names(final)
            assert final_names[label] <= summary["result"]["milestone_witnesses"].keys()
    if args.game == "metroid":
        extra = sorted(final_names["sample"] - final_names["extremes"] - INITIAL)
        lost = sorted(final_names["extremes"] - final_names["sample"] - INITIAL)
        speedups = sorted(name for name in (final_names["sample"] & final_names["extremes"]) - INITIAL
                          if 5 * data["sample"][2][name]["upper_inclusive"] <= 4 * data["extremes"][2][name]["lower_exclusive"])
        report["decision"] = {"additional_sample_names": extra, "additional_extremes_names": lost,
                              "conservative_twenty_percent_speedups": speedups,
                              "qualifies_bounded_replication": not censored and bool(extra or speedups)}
    else:
        sample, control = a["result"], b["result"]
        win = sample["solved"] and sample["frames_to_first_victory"] <= ceiling
        control_win = control["solved"] and control["frames_to_first_victory"] <= ceiling
        speedup = win and control_win and 5 * sample["frames_to_first_victory"] <= 4 * control["frames_to_first_victory"]
        report["decision"] = {"sample_solved": win, "extremes_solved": control_win,
                              "twenty_percent_victory_speedup": speedup,
                              "qualifies_chain_followup": not censored and bool(win and (not control_win or speedup))}
    report["decision"]["qualifies_breakthrough"] = False
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["decision"], indent=2))


if __name__ == "__main__":
    main()
