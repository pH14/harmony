#!/usr/bin/env python3
"""Independently verify and summarize a terminal registered screen."""
import argparse
import hashlib
import json
from pathlib import Path

from score_screen import score_screen


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--registration", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    results = json.loads(args.results.read_text())
    registration = json.loads(args.registration.read_text())
    assert results["registration_sha256"] == sha(args.registration)
    assert sha(Path(__file__).with_name("score_screen.py")) == registration["screen_scorer_sha256"]
    records = results["records"]
    for index, record in enumerate(records):
        cell = registration["cells"][index]
        assert record["id"] == cell["id"] and record["checks_passed"]
        assert record["exit_code"] == 0 and record["summary"]["status"] == "complete"
        summary = record["summary"]
        assert summary["build"]["binary_sha256"] == registration["binary_sha256"]
        assert summary["result"]["verification"] == "witness"
        assert summary["result"]["stop_reason"] in cell["allowed_stops"]
        for key, value in cell["expected_identity"].items():
            assert summary["identity"][key] == value, "identity differs: " + key
    score = score_screen(records, registration)
    assert score["decision"] != "continue", "incomplete panel is not a scientific result"
    assert json.loads(json.dumps(score)) == results["screen"], "independent score differs"
    if score["remaining_pairs"]:
        assert score["decision"] == "fail_impossible_win_count"
        assert not results["execution_complete"]
        assert results["allocation_stop"] == "registered strict-win criterion is mathematically unattainable"
    else:
        assert results["execution_complete"] and results["allocation_stop"] is None
    rows = []
    for record in records:
        summary = record["summary"]
        result = summary["result"]
        twice_replayed = 2 * (result["witness"]["physical_suffix_frames"] + sum(
            witness["replay"]["physical_suffix_frames"]
            for witness in result["milestone_witnesses"].values()))
        rows.append({"id": record["id"], "seed": summary["identity"]["seed"],
                     "admitted_frames": result["frames_emulated"],
                     "frame_drain": result["frame_budget_overshoot"],
                     "executions": result["executions"], "search_seconds": result["search_seconds"],
                     "elapsed_seconds": summary["elapsed_seconds"], "cpu_seconds": summary["cpu_seconds"],
                     "max_process_rss_bytes": summary["max_process_rss_bytes"],
                     "peak_disk_allocated_bytes_sampled": summary["peak_disk_allocated_bytes_sampled"],
                     "twice_replayed_witness_suffix_frames": twice_replayed,
                     "stream_sha256": result["stream_sha256"],
                     "summary_sha256": record["summary_sha256"],
                     "progress_sha256": record["progress_sha256"],
                     "endpoint_evidence": record["endpoint_evidence"]})
    report = {"format": "continuation-yield-screen-analysis-v1",
              "registration_format": registration["format"],
              "stage_purpose": registration["purpose"],
              "results_sha256": sha(args.results), "registration_sha256": sha(args.registration),
              "score": score, "rows": rows,
              "admitted_frames": sum(row["admitted_frames"] for row in rows),
              "twice_replayed_witness_suffix_frames": sum(row["twice_replayed_witness_suffix_frames"] for row in rows),
              "unrun_pairs": score["remaining_pairs"],
              "interpretation": ["The score is the registered allocation gate, not a significance test or proof of seed independence.",
                                 "This panel's role is given by stage_purpose. Independence requires the separate registration and prior-seed audit; the scorer alone does not establish it.",
                                 "Unrun pairs and untouched validation have no observed outcomes.",
                                 "Map/gear/boss anecdotes cannot override this fixed primary endpoint.",
                                 "Physical setup, snapshot reconstruction and unadmitted work may be additional.",
                                 "A pass supports only this registered stage; further claims require their specified gates and sufficient remaining budget. It is not a breakthrough."]}
    assert not args.out.exists(), "do not replace frozen analysis"
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({key: value for key, value in report.items() if key != "rows"}, indent=2))


if __name__ == "__main__":
    main()
