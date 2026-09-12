#!/usr/bin/env python3
"""Compare each observed Metal gain with the ordinary production retention rule."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

SAMPLE = "representative_job_sample_2_v1"
BINARY = "11249fff2bce1401d46d21db1f081678caae28c63124793886e78b5eb3992a7e"


def read(path):
    return json.loads(path.read_text())


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--seed", type=int, choices=[20261101, 20261102], required=True)
    p.add_argument("--cpus", choices=["0-3", "8-11"], required=True)
    p.add_argument("--recheck", action="store_true")
    a = p.parse_args()
    root = a.experiment.resolve()
    if a.seed == 20261101:
        source_manifest = root / f"r03b-mm2-mm2-{SAMPLE}.json"
        sample_path = root / f"runs/r03b-mm2-mm2-{SAMPLE}/mm2-metal-s20261101-w4-m8192/summary.json"
    else:
        source_manifest = root / "runs/j01-sample-s20261102/00-metal.json"
        sample_path = root / "runs/j01-sample-s20261102/00-metal/mm2-metal-s20261102-w4-m8192/summary.json"
    suite = read(source_manifest)
    sample = read(sample_path)
    assert sample["status"] == "complete" and sample["result"]["solved"]
    assert sample["result"]["witness"]["victory"]
    assert suite["search"].pop("slot_retention") == SAMPLE
    assert not suite["search"].get("prefix_input"), "L01 must start fresh"
    suite["id"] = f"l01-production-retention-metal-{a.seed}"
    suite["search"].update(frames=12000000, wall_seconds=600)
    build = root / "builds/job-sample-001"
    assert sha(build / "nes-eval") == BINARY == sample["build"]["binary_sha256"]
    manifest = root / f"l01-metal-s{a.seed}.json"
    out = root / f"runs/l01-metal-s{a.seed}"
    if not a.recheck:
        assert not out.exists() and not manifest.exists(), "refusing to overwrite L01"
        manifest.write_text(json.dumps(suite, indent=2) + "\n")
        subprocess.run([
            "taskset", "-c", a.cpus, "python3",
            str(root / "source-job-sample-001/benchmarks/search/eval.py"),
            "run", str(manifest), "--assets", str(root / "assets.json"),
            "--binary", str(build / "nes-eval"), "--build-info", str(build / "build-info.json"),
            "--out", str(out), "--jobs", "1", "--cpus", "4",
            "--memory-capacity-mib", "10240", "--finish-seconds", "120", "--disk-limit-gib", "4",
        ], check=True, timeout=780)
    else:
        assert read(manifest) == suite
    control_path = out / f"mm2-metal-s{a.seed}-w4-m8192/summary.json"
    control = read(control_path)
    assert control["status"] == "complete" and control["build"]["binary_sha256"] == BINARY
    for field in ("search_request", "identity"):
        identities = []
        for summary, policy in ((sample, SAMPLE), (control, None)):
            identity = dict(summary[field])
            assert identity.pop("slot_retention", None) == policy
            identity.pop("frames")
            identity.pop("wall_seconds")
            identities.append(identity)
        assert identities[0] == identities[1], f"unmatched {field} beyond retention and stopping limits"
    result = control["result"]
    assert result["verification"] == "witness"
    if result["solved"]:
        assert result["witness"]["victory"]
    sample_cost = sample["result"]["frames_to_first_victory"]
    control_bound = result["frames_to_first_victory"] if result["solved"] else min(12000000, result["frames_emulated"])
    censored = result["stop_reason"] == "wall_limit"
    report = {
        "format": "l01-production-retention-comparison-v1", "seed": a.seed, "cpus": a.cpus,
        "scope": "selected development seeds; fresh Metal only, not a Wily chain or validation panel",
        "reused_sample_summary_sha256": sha(sample_path), "new_control_summary_sha256": sha(control_path),
        "sample": sample, "control": control,
        "decision": {
            "sample_frame_cost": sample_cost, "control_frame_cost_or_lower_bound": control_bound,
            "control_cost_is_lower_bound": not result["solved"], "control_wall_censored": censored,
            "twenty_percent_gain_over_production": not censored and 5 * sample_cost <= 4 * control_bound,
            "qualifies_breakthrough": False,
        },
        "work_accounting": "Completed sample reused, zero additional sample search. New control search, setup and replay remain charged in its summary and chain-cost where applicable.",
    }
    (root / f"runs/l01-s{a.seed}-results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["decision"], indent=2), flush=True)


if __name__ == "__main__":
    main()
