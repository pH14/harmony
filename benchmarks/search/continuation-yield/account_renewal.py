#!/usr/bin/env python3
"""Account distinct completed cells in this tranche; disclose physical-cost gaps."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    rows = []
    for path in sorted(args.runs.rglob("summary.json")):
        raw = path.read_bytes()
        summary = json.loads(raw)
        result = summary.get("result") or {}
        relative = path.relative_to(args.runs)
        assert relative.parts[0] in ("d01", "d02", "d02r", "s01"), "declare the new stage's budget class before accounting it"
        qualification = relative.parts[0] in ("d02", "d02r")
        witness = result.get("witness", {}).get("physical_suffix_frames")
        milestones = [value["replay"]["physical_suffix_frames"]
                      for value in result.get("milestone_witnesses", {}).values()]
        rows.append({"path": str(relative), "sha256": hashlib.sha256(raw).hexdigest(),
                     "budget_class": "qualification" if qualification else "search",
                     "status": summary["status"], "admitted_frames": result.get("frames_emulated"),
                     "inferred_full_replay_admitted_frames": result.get("frames_emulated")
                     if result.get("verification") == "campaign" else 0,
                     "twice_replayed_witness_suffix_frames": 2 * (witness + sum(milestones))
                     if witness is not None else None,
                     "elapsed_seconds": summary.get("elapsed_seconds"),
                     "cpu_seconds": summary.get("cpu_seconds"),
                     "search_stop": result.get("stop_reason")})
    completed = [row for row in rows if row["status"] == "complete"]
    search = sum(row["admitted_frames"] for row in completed if row["budget_class"] == "search")
    auxiliary_search = sum(row["admitted_frames"] for row in completed if row["budget_class"] == "qualification")
    full_replay = sum(row["inferred_full_replay_admitted_frames"] for row in completed)
    witnesses = sum(row["twice_replayed_witness_suffix_frames"] for row in completed)
    report = {"format": "continuation-yield-work-accounting-v1",
              "as_of_utc": datetime.now(timezone.utc).isoformat(), "cells": rows,
              "completed_search_admitted_frames": search,
              "completed_auxiliary_qualification_admitted_frames": auxiliary_search,
              "completed_full_replay_admitted_frames_reexecuted": full_replay,
              "completed_twice_replayed_witness_suffix_frames": witnesses,
              "known_auxiliary_frame_charges": auxiliary_search + full_replay + witnesses,
              "nominal_limits": {"search": 500_000_000, "auxiliary": 50_000_000},
              "limitations": ["Distinct actual summary paths only; embedded analysis copies excluded.",
                              "Running cells without summaries are not included and have additional accrued work.",
                              "Setup, unadmitted worker frames, physical snapshot-reconstruction work and failed/incomplete work may be additional; unavailable counters are unknown.",
                              "Full campaign replay charge is inferred from exact report/checkpoint verification; it is not a complete physical replay counter.",
                              "Both final and milestone witnesses are replayed twice; both copies are charged.",
                              "Frame totals include bounded in-flight drain and are not wall-time totals."]}
    assert not args.out.exists(), "do not replace frozen accounting"
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({key: value for key, value in report.items() if key != "cells"}, indent=2))


if __name__ == "__main__":
    main()
