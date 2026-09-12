#!/usr/bin/env python3
"""Run the preregistered new-seed fresh MM2 chain pair after bridge qualification."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--experiment", type=Path, required=True)
args = parser.parse_args()
root = args.experiment.resolve()
source = root / "source-job-sample-001"
driver = root / "source-metal-export-002/benchmarks/search/alternative-futures/mm2_chain.py"
binary = root / "builds/job-sample-001/nes-eval"
assert hashlib.sha256(binary.read_bytes()).hexdigest() == "11249fff2bce1401d46d21db1f081678caae28c63124793886e78b5eb3992a7e"
qualification = json.loads((root / "runs/j01-export-qualification-r2/results.json").read_text())
assert qualification["passed"] and len(qualification["records"]) == 2
assert {r["label"] for r in qualification["records"]} == {"sample", "extremes"}
for record in qualification["records"]:
    assert record["bridge"]["verified_replays"] == 2
    endpoint = record["bridge"]["result"]["endpoint"]
    assert endpoint["stage"] == 0 and endpoint["weapons_obtained"] == 64
    assert endpoint["health"] == 28 and endpoint["lives"] > 0
gate = json.loads((root / "runs/r03b-mm2-analysis.json").read_text())
assert gate["decision"]["qualifies_chain_followup"]
report_path = root / "runs/j01-results.json"
assert not report_path.exists(), "refusing to overwrite a chain pair"
report = {"format": "j01-fresh-paired-chain-v1", "seed": 20261102,
          "scope": "development only; no external gameplay inputs",
          "driver_sha256": hashlib.sha256(driver.read_bytes()).hexdigest(),
          "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
          "bridge_qualification_sha256": hashlib.sha256((root / "runs/j01-export-qualification-r2/results.json").read_bytes()).hexdigest(),
          "execution_complete": False, "qualifies_breakthrough": False, "records": []}
report_path.write_text(json.dumps(report, indent=2) + "\n")


def run(label, policy, cpus):
    out = root / "runs" / f"j01-{label}-s20261102"
    command = ["taskset", "-c", cpus, "python3", str(driver), "--root", str(source),
               "--out", str(out), "--binary", str(binary),
               "--build-info", str(root / "builds/job-sample-001/build-info.json"),
               "--progress-binary", str(root / "builds/nes-progress-001"),
               "--assets", str(root / "assets.json"), "--seed", "20261102",
               "--slot-retention", policy, "--suffix", "one_to_six_within_3_longest_actions_full_hold",
               "--executions", "1000000", "--frames", "120000000", "--stage-seconds", "1200",
               "--chain-seconds", "5400"]
    assert not out.exists(), "refusing to overwrite an existing chain"
    with (root / "runs" / f"j01-{label}.log").open("w") as log:
        result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=5460)
    chain_path = out / "chain.json"
    record = {"label": label, "cpus": cpus, "command": command, "exit_code": result.returncode,
              "chain": json.loads(chain_path.read_text()) if chain_path.exists() else None}
    print(json.dumps({"label": label, "exit_code": result.returncode,
                      "status": record["chain"]["status"] if record["chain"] else "missing_chain"}), flush=True)
    return record


with ThreadPoolExecutor(max_workers=2) as pool:
    futures = [pool.submit(run, "sample", "representative_job_sample_2_v1", "8-11"),
               pool.submit(run, "extremes", "resource_extremes_2_v1", "0-3")]
    for future in as_completed(futures):
        report["records"].append(future.result())
        report_path.write_text(json.dumps(report, indent=2) + "\n")
report["execution_complete"] = True
report_path.write_text(json.dumps(report, indent=2) + "\n")
