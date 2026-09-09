#!/usr/bin/env python3
"""Audit alternative finite objectives on the already completed P03 probes."""
import argparse
import hashlib
import json
from pathlib import Path
from analyze_p03 import analyze


def totals(trials):
    result = {"discarded_survives": 0, "survivor_survives": 0,
              "discarded_exclusive_map_suffix_events": 0, "survivor_exclusive_map_suffix_events": 0,
              "discarded_exclusive_exit_trials": 0, "survivor_exclusive_exit_trials": 0,
              "both_sides_have_exclusive_exit": 0}
    for trial in trials:
        maps = {side: {tuple(m) for m in trial[side]["reached_maps"]}
                for side in ["discarded", "survivor"]}
        assert all(len(m) == 3 for states in maps.values() for m in states)
        for side, other in [("discarded", "survivor"), ("survivor", "discarded")]:
            exclusive = maps[side] - maps[other]
            assert bool(exclusive) == trial[f"{side}_only_exit"]
            result[f"{side}_survives"] += not trial[side]["dead"]
            result[f"{side}_exclusive_map_suffix_events"] += len(exclusive)
            result[f"{side}_exclusive_exit_trials"] += bool(exclusive)
        result["both_sides_have_exclusive_exit"] += trial["discarded_only_exit"] and trial["survivor_only_exit"]
    result["net_survival_count"] = result["discarded_survives"] - result["survivor_survives"]
    result["net_map_suffix_event_count"] = result["discarded_exclusive_map_suffix_events"] - result["survivor_exclusive_map_suffix_events"]
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--research", type=Path, default=Path(__file__).resolve().parent)
    p.add_argument("--outcomes", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    checked = analyze(a.research, a.outcomes)
    raw = a.outcomes.read_bytes()
    trials = [json.loads(line) for line in raw.splitlines()]
    report = {"format": "p03-finite-objective-audit-v1",
              "scope": "post-hoc descriptive reanalysis of existing selected probes; no new gate or emulator work",
              "outcomes_sha256": hashlib.sha256(raw).hexdigest(), "audit_sha256": checked["audit_sha256"],
              "trials": len(trials), "aggregate": totals(trials),
              "rows": [{"pair": index, **totals([t for t in trials if t["pair"] == index])} for index in range(16)],
              "interpretation": [
                  "A feature is the pair-specific suffix index plus the raw map tuple, not a globally new map.",
                  "Both sides can expose exclusive maps on the same suffix; exclusive-exit flags are not complementary successes.",
                  "Map visits occurred while alive inside actions; endpoint survival is a separate objective.",
                  "Same 64 suffixes are shared across 16 selected pairs; no 1024-independent-observation confidence claim.",
                  "Equal map weights and survivor counts are illustrative objectives, not identified task values.",
                  "Only one incumbent was probed; this does not measure marginal coverage against two survivors.",
                  "No candidate, descriptor, running campaign, or predeclared escalation gate changes."]}
    a.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["aggregate"], indent=2))


if __name__ == "__main__":
    main()
