#!/usr/bin/env python3
"""Check J02 lineage and compare in-budget awards with the reused J01 chain."""
import argparse
import json
from pathlib import Path
import re
from analyze_j01 import BINARY, STAGES, point, read, sha


def normalized(value):
    value = json.loads(json.dumps(value))
    for key in ("slot_retention", "prefix_input", "prefix_sha256", "frames", "wall_seconds"):
        value.pop(key, None)
    if "policies" in value:
        backend = value["policies"]["emulator_backend"]
        value["policies"]["emulator_backend"] = re.sub(
            r"prefix-sha256=[0-9a-f]{64}", "prefix-sha256=<own-prefix>", backend)
    return value


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    root = a.experiment.resolve()
    directory = root / "runs/j02-ordinary-s20261102"
    manifest = read(directory / "chain.json")
    assert manifest["status"] in ("stage_not_solved", "stage_limit", "victory_after_frame_limit", "chain_wall_limit")
    assert manifest["seed"] == 20261102 and manifest["binary_sha256"] == BINARY
    assert manifest["attempts_per_stage"] == 1 and not manifest["external_gameplay_inputs"]
    assert manifest["limits"]["slot_retention"] is None and manifest["limits"]["max_stages"] == 4
    ceilings = [2489148, 66634037, 39647446, 72056120]
    assert manifest["limits"]["stage_frames"] == ceilings
    prior_path = root / "runs/j01-analysis.json"
    prior = read(prior_path)["arms"]["sample"]
    assert prior["solved_stages"] == ["metal", "heat", "air"]
    records, incomplete, ordinary_awards = [], [], []
    prefix_hash, prefix_path, weapons = None, None, 0
    for index, record in enumerate(manifest["stages"]):
        assert index < 4
        name, number = STAGES[index]
        assert record["index"] == index and record["stage"] == name
        assert record["parent_prefix_sha256"] == prefix_hash
        suite_path = directory / f"{index:02}-{name}.json"
        suite = read(suite_path)
        assert sha(suite_path) == record["suite_sha256"]
        assert suite["search"].get("prefix_input") == prefix_path
        assert suite["search"].get("prefix_sha256") == prefix_hash
        assert suite["search"]["frames"] == ceilings[index]
        cell = directory / f"{index:02}-{name}" / f"mm2-{name}-s20261102-w4-m8192"
        summary_path = cell / "summary.json"
        summary = read(summary_path)
        assert sha(summary_path) == record["summary_sha256"] and summary["result"] == record["result"]
        assert summary["status"] == "complete" and summary["build"]["binary_sha256"] == BINARY
        sample_path = root / f"runs/j01-sample-s20261102/{index:02}-{name}/mm2-{name}-s20261102-w4-m8192/summary.json"
        sample = read(sample_path)
        assert sha(sample_path) == prior["stages"][index]["summary_sha256"]
        assert summary["search_request"].get("slot_retention") is None
        for field in ("identity", "search_request"):
            assert normalized(summary[field]) == normalized(sample[field]), f"unmatched {field}"
        result = summary["result"]
        in_budget = result["solved"] and result["frames_to_first_victory"] <= ceilings[index]
        if in_budget:
            assert result["witness"]["victory"] and result["verification"] == "witness"
            weapons |= 1 << number
            bridge = record["bridge_replay"]
            endpoint = bridge["result"]["endpoint"]
            assert bridge["verified_replays"] == 2 and endpoint["stage"] == STAGES[index + 1][1]
            assert endpoint["weapons_obtained"] == weapons and endpoint["health"] > 0 and endpoint["lives"] > 0
            prefix_path = str(cell / "campaign/next-prefix.json")
            prefix_hash = sha(Path(prefix_path))
            assert prefix_hash == record["next_prefix_sha256"] == bridge["input_sha256"]
            ordinary_awards.append(name)
        else:
            assert index == len(manifest["stages"]) - 1
            if result["frames_emulated"] < ceilings[index]:
                incomplete.append(name)
        rows = [json.loads(line) for line in (cell / "campaign/progress.jsonl").read_text().splitlines()]
        assert rows and all(x["frames_emulated"] <= y["frames_emulated"] for x, y in zip(rows, rows[1:]))
        records.append({"stage": name, "frame_ceiling": ceilings[index], "ordinary_in_budget_victory": in_budget,
                        "ordinary_result": result, "ordinary_summary_sha256": sha(summary_path),
                        "ordinary_at_or_before_ceiling": point(rows, ceilings[index]),
                        "candidate_stage": prior["stages"][index],
                        "scope": "fresh isolated retention comparison" if index == 0 else "fresh end-to-end own-prefix comparison"})
        if index == 0:
            previous = read(root / "runs/l01-s20261102-results.json")["control"]["result"]
            assert result["frames_to_first_victory"] == previous["frames_to_first_victory"]
            assert result["witness"] == previous["witness"], "changing the stop limit changed the completed Metal witness"
    assert records
    if manifest["status"] == "stage_limit":
        assert len(records) == 4 and len(ordinary_awards) == 4
    elif manifest["status"] == "stage_not_solved":
        assert not records[-1]["ordinary_result"]["solved"]
    elif manifest["status"] == "victory_after_frame_limit":
        assert records[-1]["ordinary_result"]["solved"] and not records[-1]["ordinary_in_budget_victory"]
    assert manifest["admitted_frames_all_attempts"] == sum(r["ordinary_result"]["frames_emulated"] for r in records)
    candidate_only_depth = len(ordinary_awards) < 3
    # A total wall limit can stop between stages without observing a failed one.
    if manifest["status"] == "chain_wall_limit" and len(ordinary_awards) == len(records):
        incomplete.append("next_unattempted_stage")
    report = {"format": "j02-matched-stage-work-analysis-v1",
              "scope": "adaptive development comparison; stage budgets selected from J01, not held-out validation",
              "candidate_analysis_sha256": sha(prior_path), "ordinary_manifest_sha256": sha(directory / "chain.json"),
              "ordinary_status": manifest["status"], "ordinary_in_budget_awards": ordinary_awards,
              "candidate_in_budget_awards": prior["solved_stages"], "unobserved_control_work": incomplete,
              "stage_frame_ceilings": ceilings, "stages": records,
              "ordinary_admitted_frames": manifest["admitted_frames_all_attempts"],
              "ordinary_physical_frames_lower_bound": manifest["accounted_physical_frames_lower_bound"],
              "decision": {"candidate_has_greater_observed_depth": candidate_only_depth,
                           "qualifies_bounded_fresh_replication": candidate_only_depth and not incomplete,
                           "qualifies_breakthrough": False}}
    a.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["decision"], indent=2))


if __name__ == "__main__":
    main()
