#!/usr/bin/env python3
"""Validate the prospective J03 pair against its registered depth/work gate."""
import argparse
import json
from pathlib import Path
from analyze_j01 import BINARY, STAGES, read, sha
from analyze_j02 import normalized

CEILINGS = [12000000, 85000000, 60000000, 85000000]
SEED = 20261103


def analyze(root):
    arms, raw = {}, {}
    for label, policy in [("ordinary", None), ("sample", "representative_job_sample_2_v1")]:
        directory = root / f"runs/j03-{label}-s{SEED}"
        manifest = read(directory / "chain.json")
        assert manifest["status"] in ("stage_not_solved", "stage_limit", "victory_after_frame_limit", "chain_wall_limit")
        assert manifest["seed"] == SEED and manifest["binary_sha256"] == BINARY
        assert manifest["attempts_per_stage"] == 1 and manifest["external_gameplay_inputs"] is False
        assert manifest["limits"]["slot_retention"] == policy and manifest["limits"]["max_stages"] == 4
        assert manifest["limits"]["stage_frames"] == CEILINGS
        assert manifest["limits"]["stage_seconds"] == 2700 and manifest["limits"]["chain_seconds"] == 5400
        prefix_hash, prefix_path, weapons = None, None, 0
        records, incomplete = [], []
        raw[label] = []
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
            assert suite["search"].get("slot_retention") == policy
            assert suite["search"]["frames"] == CEILINGS[index]
            assert suite["seeds"] == [SEED] and suite["workers"] == [4] and suite["memory_mib"] == [8192]
            assert suite["cases"][0]["stage"] == number
            cell = directory / f"{index:02}-{name}/mm2-{name}-s{SEED}-w4-m8192"
            summary_path = cell / "summary.json"
            summary = read(summary_path)
            assert sha(summary_path) == record["summary_sha256"]
            assert summary["status"] == "complete" and summary["result"] == record["result"]
            assert summary["build"]["binary_sha256"] == BINARY
            result = summary["result"]
            in_budget = result["solved"] and result["frames_to_first_victory"] <= CEILINGS[index]
            compact = {"stage": name, "ceiling": CEILINGS[index], "in_budget_victory": in_budget,
                       "summary_sha256": sha(summary_path), "result": result,
                       "parent_prefix_sha256": prefix_hash}
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
                compact["bridge"] = bridge
            else:
                assert index == len(manifest["stages"]) - 1
                if result["frames_emulated"] < CEILINGS[index]:
                    incomplete.append(name)
            records.append(compact)
            raw[label].append(summary)
        awards = [r["stage"] for r in records if r["in_budget_victory"]]
        if manifest["status"] == "stage_limit":
            assert len(awards) == 4
        if manifest["status"] == "chain_wall_limit" and len(awards) == len(records):
            incomplete.append("next_unattempted_stage")
        assert manifest["admitted_frames_all_attempts"] == sum(r["result"]["frames_emulated"] for r in records)
        third = (sum(r["result"]["frames_emulated"] for r in records[:2])
                 + records[2]["result"]["frames_to_first_victory"]) if len(awards) >= 3 else None
        arms[label] = {"status": manifest["status"], "manifest_sha256": sha(directory / "chain.json"),
                       "awards": awards, "stages": records, "incomplete_blocking_work": incomplete,
                       "admitted_work_to_third_victory": third,
                       "admitted_frames": manifest["admitted_frames_all_attempts"],
                       "physical_frames_lower_bound": manifest["accounted_physical_frames_lower_bound"],
                       "physical_cost_limitations": manifest["cost_limitations"]}
    for ordinary, sample in zip(raw["ordinary"], raw["sample"]):
        for field in ["search_request", "identity"]:
            assert normalized(ordinary[field]) == normalized(sample[field]), f"unmatched {field}"
        for field in ["frames", "wall_seconds"]:
            assert ordinary["search_request"][field] == sample["search_request"][field]
    ordinary, sample = arms["ordinary"], arms["sample"]
    depth = len(sample["awards"]) > len(ordinary["awards"]) and not ordinary["incomplete_blocking_work"]
    common_third = all(a["admitted_work_to_third_victory"] is not None for a in arms.values())
    faster = common_third and 5 * sample["admitted_work_to_third_victory"] <= 4 * ordinary["admitted_work_to_third_victory"]
    return {"format": "j03-prospective-depth-analysis-v1", "seed": SEED,
            "scope": "one prospectively bounded development replication; own searched prefixes; not Wily validation",
            "stage_frame_ceilings": CEILINGS, "arms": arms,
            "decision": {"greater_in_budget_depth": depth, "twenty_percent_less_work_to_common_third_victory": faster,
                         "qualifies_another_sampling_chain": depth or faster, "qualifies_breakthrough": False}}


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    report = analyze(a.experiment.resolve())
    a.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["decision"], indent=2))
