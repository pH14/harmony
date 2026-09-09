#!/usr/bin/env python3
"""Validate fresh-chain lineage and separate matched work from wall censoring."""
import argparse
import hashlib
import json
from pathlib import Path

STAGES = [("metal", 6), ("heat", 0), ("air", 1), ("wood", 2),
          ("bubble", 3), ("quick", 4), ("flash", 5), ("crash", 7),
          ("wily1", 8), ("wily2", 9), ("wily3", 10)]
POLICIES = {"sample": "representative_job_sample_2_v1", "extremes": "resource_extremes_2_v1"}
BINARY = "11249fff2bce1401d46d21db1f081678caae28c63124793886e78b5eb3992a7e"


def read(path):
    return json.loads(path.read_text())


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def point(rows, ceiling):
    row = next((r for r in reversed(rows) if r["frames_emulated"] <= ceiling), None)
    if row is None:
        return None
    return {k: row[k] for k in ("frames_emulated", "executions", "milestones", "progress")}


def analyze(root):
    output = {"format": "j01-fresh-chain-analysis-v1", "seed": 20261102,
              "scope": "one adaptive development seed; fixed historical stage order; not validation",
              "arms": {}, "common_stages": [], "qualifies_breakthrough": False}
    raw = {}
    for label, policy in POLICIES.items():
        directory = root / "runs" / f"j01-{label}-s20261102"
        manifest = read(directory / "chain.json")
        assert manifest["status"] != "running", "analyze after the chain stops"
        assert manifest["seed"] == 20261102 and manifest["binary_sha256"] == BINARY
        assert manifest["external_gameplay_inputs"] is False and manifest["attempts_per_stage"] == 1
        assert manifest["order"] == [s for s, _ in STAGES] + ["wily4 reach"]
        assert manifest["limits"]["slot_retention"] == policy
        expected_prefix = None
        expected_prefix_path = None
        expected_weapons = 0
        stages = []
        raw[label] = []
        for index, record in enumerate(manifest["stages"]):
            name, number = STAGES[index]
            assert record["index"] == index and record["stage"] == name
            assert record["parent_prefix_sha256"] == expected_prefix
            suite_path = directory / f"{index:02}-{name}.json"
            assert sha(suite_path) == record["suite_sha256"]
            suite = read(suite_path)
            assert suite["search"].get("prefix_sha256") == expected_prefix
            assert suite["search"].get("prefix_input") == expected_prefix_path
            assert suite["seeds"] == [20261102] and suite["workers"] == [4] and suite["memory_mib"] == [8192]
            assert suite["cases"][0]["stage"] == number
            cell = directory / f"{index:02}-{name}" / f"mm2-{name}-s20261102-w4-m8192"
            summary_path = cell / "summary.json"
            assert sha(summary_path) == record["summary_sha256"]
            summary = read(summary_path)
            assert summary["status"] == "complete" and summary["result"] == record["result"]
            assert summary["build"]["binary_sha256"] == BINARY
            assert summary["search_request"]["slot_retention"] == policy
            result = summary["result"]
            rows = [json.loads(line) for line in (cell / "campaign/progress.jsonl").read_text().splitlines()]
            assert rows and all(a["frames_emulated"] <= b["frames_emulated"] for a, b in zip(rows, rows[1:]))
            compact = {"stage": name, "solved": result["solved"], "stop_reason": result["stop_reason"],
                       "frames": result["frames_emulated"], "jobs": result["executions"],
                       "first_victory_frames": result["frames_to_first_victory"],
                       "stream_sha256": result["stream_sha256"], "milestones": result["milestones"],
                       "peak_rss_bytes": record["peak_rss_bytes"],
                       "witness": result["witness"], "summary_sha256": sha(summary_path),
                       "prefix_sha256": expected_prefix}
            if result["solved"]:
                assert result["witness"]["victory"] and result["verification"] == "witness"
                if number < 8:
                    expected_weapons |= 1 << number
                bridge = record["bridge_replay"]
                endpoint = bridge["result"]["endpoint"]
                next_number = STAGES[index + 1][1] if index + 1 < len(STAGES) else 11
                assert bridge["verified_replays"] == 2 and endpoint["stage"] == next_number
                assert endpoint["weapons_obtained"] == expected_weapons
                assert endpoint["health"] > 0 and endpoint["lives"] > 0
                expected_prefix_path = str(cell / "campaign/next-prefix.json")
                expected_prefix = sha(Path(expected_prefix_path))
                assert expected_prefix == record["next_prefix_sha256"] == bridge["input_sha256"]
                compact["next_prefix_sha256"] = expected_prefix
                compact["bridge_endpoint"] = endpoint
                compact["verified_bridge_replays"] = 2
            else:
                assert index == len(manifest["stages"]) - 1, "chain continued after an unsolved stage"
            stages.append(compact)
            raw[label].append((summary, rows))
        output["arms"][label] = {
            "status": manifest["status"], "manifest_sha256": sha(directory / "chain.json"),
            "stages": stages, "solved_stages": [s["stage"] for s in stages if s["solved"]],
            "admitted_frames": manifest["admitted_frames_all_attempts"],
            "accounted_physical_frames_lower_bound": manifest["accounted_physical_frames_lower_bound"],
            "cost_limitations": manifest["cost_limitations"],
            "wily4_all_weapons": manifest["status"] == "wily4_all_weapons",
        }
    for index, ((sample, sample_rows), (control, control_rows)) in enumerate(zip(raw["sample"], raw["extremes"])):
        requests = []
        for summary, policy in ((sample, POLICIES["sample"]), (control, POLICIES["extremes"])):
            request = dict(summary["search_request"])
            assert request.pop("slot_retention") == policy
            request.pop("prefix_input", None)
            request.pop("prefix_sha256", None)
            requests.append(request)
        assert requests[0] == requests[1], "paired stages differ beyond policy and their own prefix"
        ceiling = min(120000000, sample["result"]["frames_emulated"], control["result"]["frames_emulated"])
        censored = [label for label, summary in (("sample", sample), ("extremes", control))
                    if summary["result"]["stop_reason"] == "wall_limit"]
        output["common_stages"].append({
            "stage": STAGES[index][0], "common_frame_ceiling": ceiling,
            "sample_at_or_before_ceiling": point(sample_rows, ceiling),
            "extremes_at_or_before_ceiling": point(control_rows, ceiling),
            "wall_censored_arms": censored,
            "scope": "isolated fresh retention comparison" if index == 0 else "end-to-end own-prefix comparison; starting emulator states differ",
        })
    return output


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    output = analyze(a.experiment.resolve())
    a.out.write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps({label: {k: arm[k] for k in ("status", "solved_stages", "admitted_frames")}
                      for label, arm in output["arms"].items()}, indent=2))


if __name__ == "__main__":
    main()
