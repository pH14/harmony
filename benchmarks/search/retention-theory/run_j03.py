#!/usr/bin/env python3
"""One fresh development replication, ordinary then sampling on identical CPUs."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument("--experiment", type=Path, required=True)
p.add_argument("--driver", type=Path, required=True)
a = p.parse_args()
root = a.experiment.resolve()
build = root / "builds/job-sample-001"
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
binary = sha(build / "nes-eval")
assert binary == "11249fff2bce1401d46d21db1f081678caae28c63124793886e78b5eb3992a7e"
gate_path = root / "runs/j02-analysis.json"
assert json.loads(gate_path.read_text())["decision"]["qualifies_bounded_fresh_replication"]
out_path = root / "runs/j03-results.json"
assert not out_path.exists(), "refusing to overwrite J03"
report = {"format": "j03-fresh-ordinary-sample-depth-replication-v1", "seed": 20261103,
          "scope": "one prospective development seed, through Wood only; not Wily validation",
          "stage_frame_ceilings": [12000000, 85000000, 60000000, 85000000],
          "driver_sha256": sha(a.driver), "binary_sha256": binary,
          "prior_gate_sha256": sha(gate_path), "cpus": "8-11",
          "fixed_arm_order": ["ordinary", "sample"], "records": [],
          "execution_complete": False, "qualifies_breakthrough": False}
out_path.write_text(json.dumps(report, indent=2) + "\n")
for label, policy in (("ordinary", None), ("sample", "representative_job_sample_2_v1")):
    out = root / f"runs/j03-{label}-s20261103"
    assert not out.exists(), "refusing to replace a chain"
    command = ["taskset", "-c", "8-11", "python3", str(a.driver),
               "--root", str(root / "source-job-sample-001"), "--out", str(out),
               "--binary", str(build / "nes-eval"), "--build-info", str(build / "build-info.json"),
               "--progress-binary", str(root / "builds/nes-progress-001"),
               "--assets", str(root / "assets.json"), "--seed", "20261103",
               "--suffix", "one_to_six_within_3_longest_actions_full_hold",
               "--executions", "1000000", "--stage-seconds", "2700", "--chain-seconds", "5400",
               "--max-stages", "4", "--stage-frames", *map(str, report["stage_frame_ceilings"])]
    if policy:
        command += ["--slot-retention", policy]
    with (root / f"runs/j03-{label}.log").open("w") as log:
        result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=5460)
    path = out / "chain.json"
    chain = json.loads(path.read_text()) if path.exists() else None
    report["records"].append({"label": label, "policy": policy, "command": command,
                              "exit_code": result.returncode, "chain": chain})
    out_path.write_text(json.dumps(report, indent=2) + "\n")
    assert result.returncode == 0 and chain is not None, "infrastructure failure; preserve outputs and stop"
    assert chain["status"] in ("stage_not_solved", "stage_limit", "victory_after_frame_limit", "chain_wall_limit")
    print(json.dumps({"label": label, "status": chain["status"]}), flush=True)
report["execution_complete"] = True
out_path.write_text(json.dumps(report, indent=2) + "\n")
