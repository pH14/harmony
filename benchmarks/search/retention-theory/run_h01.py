#!/usr/bin/env python3
"""One shared-prefix Heat control; reuse J01's completed sampling result."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

SAMPLE = "representative_job_sample_2_v1"
EXTREMES = "resource_extremes_2_v1"
BINARY = "11249fff2bce1401d46d21db1f081678caae28c63124793886e78b5eb3992a7e"
PREFIX = "98483c03561d44fb8d6f037b6dbec83a47853a8cee821334ec10a4a9f619c6a3"


def read(path):
    return json.loads(path.read_text())


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--recheck", action="store_true")
    a = p.parse_args()
    root = a.experiment.resolve()
    prior = root / "runs/j01-sample-s20261102"
    sample_path = prior / "01-heat/mm2-heat-s20261102-w4-m8192/summary.json"
    sample = read(sample_path)
    assert sample["status"] == "complete" and sample["result"]["solved"]
    assert sample["result"]["frames_to_first_victory"] == 66633523
    assert sample["result"]["witness"]["victory"]
    build = root / "builds/job-sample-001"
    assert sha(build / "nes-eval") == BINARY == sample["build"]["binary_sha256"]
    suite = read(prior / "01-heat.json")
    assert suite["search"]["slot_retention"] == SAMPLE
    assert sha(Path(suite["search"]["prefix_input"])) == PREFIX == suite["search"]["prefix_sha256"]
    suite["id"] = "h01-shared-prefix-heat-control"
    suite["cases"][0]["origin"] = "diagnostic reuse of J01 sampling Metal prefix; not a fresh chain"
    suite["search"].update(slot_retention=EXTREMES, frames=85000000, wall_seconds=3000)
    manifest = root / "h01-heat-control.json"
    out = root / "runs/h01-heat-control"
    if not a.recheck:
        assert not out.exists() and not manifest.exists(), "refusing to overwrite H01"
        manifest.write_text(json.dumps(suite, indent=2) + "\n")
        subprocess.run([
            "taskset", "-c", "0-3", "python3",
            str(root / "source-job-sample-001/benchmarks/search/eval.py"),
            "run", str(manifest), "--assets", str(root / "assets.json"),
            "--binary", str(build / "nes-eval"), "--build-info", str(build / "build-info.json"),
            "--out", str(out), "--jobs", "1", "--cpus", "4",
            "--memory-capacity-mib", "10240", "--finish-seconds", "120", "--disk-limit-gib", "4",
        ], check=True, timeout=3180)
    else:
        assert read(manifest) == suite
    control_path = out / "mm2-heat-s20261102-w4-m8192/summary.json"
    control = read(control_path)
    assert control["status"] == "complete"
    assert control["build"]["binary_sha256"] == BINARY
    # Limits stop the run; they do not seed the policy or determine its choices.
    for field in ("search_request", "identity"):
        identities = []
        for summary, policy in ((sample, SAMPLE), (control, EXTREMES)):
            identity = dict(summary[field])
            assert identity.pop("slot_retention") == policy
            identity.pop("frames")
            identity.pop("wall_seconds")
            identities.append(identity)
        assert identities[0] == identities[1], f"unmatched {field} beyond retention and stop limits"
    result = control["result"]
    assert result["verification"] == "witness"
    candidate_cost = sample["result"]["frames_to_first_victory"]
    control_cost = result["frames_to_first_victory"]
    censored = result["stop_reason"] == "wall_limit"
    observed_limit = min(85000000, result["frames_emulated"])
    control_bound = control_cost if result["solved"] else observed_limit
    qualifies = not censored and 5 * candidate_cost <= 4 * control_bound
    report = {
        "format": "h01-shared-prefix-heat-v1",
        "scope": "selected development prefix and seed; conditional retention diagnostic, not fresh search or validation",
        "reused_sample_summary_sha256": sha(sample_path),
        "new_control_summary_sha256": sha(control_path),
        "prefix_sha256": PREFIX,
        "sample": sample, "control": control,
        "decision": {
            "control_wall_censored": censored,
            "control_cost_is_lower_bound": not result["solved"],
            "control_frame_cost_or_lower_bound": control_bound,
            "sample_frame_cost": candidate_cost,
            "conditional_twenty_percent_gain": qualifies,
            "qualifies_breakthrough": False,
        },
        "work_accounting": "Sample search reused at zero additional search cost. Control setup, export and replay are additional; summary and campaign chain-cost preserve those counts.",
    }
    (root / "runs/h01-results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["decision"], indent=2), flush=True)


if __name__ == "__main__":
    main()
