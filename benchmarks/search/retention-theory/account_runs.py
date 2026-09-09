#!/usr/bin/env python3
"""Count executed cells from unique run directories, excluding copied analyses."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    root = a.experiment.resolve()
    cells, probes, other = [], [], []
    for path in sorted((root / "runs").rglob("summary.json")):
        raw = path.read_bytes()
        value = json.loads(raw)
        common = {"path": str(path.relative_to(root)), "sha256": hashlib.sha256(raw).hexdigest()}
        if "search_request" in value and "status" in value:
            result = value.get("result") or {}
            suffix = (result.get("witness") or {}).get("physical_suffix_frames")
            milestone_suffix = sum(w["replay"].get("physical_suffix_frames", 0)
                                   for w in result.get("milestone_witnesses", {}).values())
            cells.append({**common, "status": value["status"],
                          "game": value["search_request"].get("game"), "seed": value["search_request"].get("seed"),
                          "verification": result.get("verification"), "stop_reason": result.get("stop_reason"),
                          "admitted_frames": result.get("frames_emulated"),
                          "executions": result.get("executions"), "search_seconds": result.get("search_seconds"),
                          "cell_elapsed_seconds": value.get("elapsed_seconds"),
                          "twice_replayed_suffix_frames": 2 * (suffix + milestone_suffix) if suffix is not None else None})
        elif isinstance(value.get("physical_frames"), int):
            probes.append({**common, "format": value.get("format"), "physical_frames": value["physical_frames"]})
        else:
            other.append({**common, "format": value.get("format"), "status": value.get("status")})
    complete = [cell for cell in cells if cell["status"] == "complete"]
    assert all(cell["admitted_frames"] is not None for cell in complete)
    report = {"format": "retention-tranche-work-accounting-v1",
              "scope": "one record per actual evaluation/probe summary path; embedded copies in analyses excluded",
              "cell_status_counts": dict(Counter(cell["status"] for cell in cells)),
              "completed_evaluation_admitted_frames": sum(cell["admitted_frames"] for cell in complete),
              "completed_evaluation_executions": sum(cell["executions"] for cell in complete),
              "sum_completed_search_phase_seconds": sum(cell["search_seconds"] for cell in complete),
              "completed_full_replay_admitted_frames_reexecuted": sum(cell["admitted_frames"] for cell in complete if cell["verification"] == "campaign"),
              "completed_witness_suffix_frames_reported": sum(cell["twice_replayed_suffix_frames"] or 0 for cell in complete),
              "standalone_probe_physical_frames_reported": sum(probe["physical_frames"] for probe in probes),
              "limitations": ["Not a complete machine-wide physical-frame or CPU-time total.",
                              "Full replay work is inferred from exact campaign verification; suffix/probe counters are reported.",
                              "Setup, bridges, exports, unadmitted worker work and incomplete runs may add physical work.",
                              "Search-phase seconds sum across overlapping cells and are not tranche wall time.",
                              "Missing/incomplete results have unknown cost, not zero-cost success."],
              "cells": cells, "standalone_probes": probes, "other_summaries": other}
    a.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k: v for k, v in report.items() if k not in ["cells", "standalone_probes", "other_summaries"]}, indent=2))


if __name__ == "__main__":
    main()
