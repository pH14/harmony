#!/usr/bin/env python3
"""Fresh ordinary-retention comparator at J01's observed per-stage work limits."""
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
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
assert digest(build / "nes-eval") == "11249fff2bce1401d46d21db1f081678caae28c63124793886e78b5eb3992a7e"
prior_path = root / "runs/j01-analysis.json"
prior = json.loads(prior_path.read_text())
arm = prior["arms"]["sample"]
assert arm["status"] == "stage_not_solved" and arm["solved_stages"] == ["metal", "heat", "air"]
frames = [stage["frames"] for stage in arm["stages"]]
assert frames == [2489148, 66634037, 39647446, 72056120]
for seed in (20261101, 20261102):
    previous = json.loads((root / f"runs/l01-s{seed}-results.json").read_text())
    assert not previous["decision"]["control_wall_censored"]
out = root / "runs/j02-ordinary-s20261102"
report_path = root / "runs/j02-results.json"
assert not out.exists() and not report_path.exists(), "refusing to overwrite J02"
command = ["taskset", "-c", "8-11", "python3", str(a.driver),
           "--root", str(root / "source-job-sample-001"), "--out", str(out),
           "--binary", str(build / "nes-eval"), "--build-info", str(build / "build-info.json"),
           "--progress-binary", str(root / "builds/nes-progress-001"),
           "--assets", str(root / "assets.json"), "--seed", "20261102",
           "--suffix", "one_to_six_within_3_longest_actions_full_hold",
           "--executions", "1000000", "--stage-seconds", "2400", "--chain-seconds", "5400",
           "--max-stages", "4", "--stage-frames", *map(str, frames)]
report = {"format": "j02-stage-work-matched-production-chain-v1",
          "scope": "adaptive development comparison reusing J01 candidate; fresh ordinary control, not validation",
          "driver_sha256": digest(a.driver), "binary_sha256": digest(build / "nes-eval"),
          "reused_candidate_analysis_sha256": digest(prior_path), "seed": 20261102,
          "stage_frame_ceilings": frames, "cpus": "8-11", "command": command,
          "execution_complete": False, "qualifies_breakthrough": False}
report_path.write_text(json.dumps(report, indent=2) + "\n")
with (root / "runs/j02-ordinary.log").open("w") as log:
    result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=5460)
report["exit_code"] = result.returncode
chain_path = out / "chain.json"
report["chain"] = json.loads(chain_path.read_text()) if chain_path.exists() else None
report["execution_complete"] = True
report_path.write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps({"exit_code": result.returncode, "status": report["chain"]["status"] if report["chain"] else "missing_chain"}), flush=True)
