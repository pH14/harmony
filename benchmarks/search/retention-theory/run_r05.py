#!/usr/bin/env python3
"""One fresh replication with both capacity and ordinary-retention controls."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

BINARY = "2a727ac12dbc4ff07272e0119bab39179aaa45bc0a6f422c2cc8fb77cdf3f981"
SEED = 20261210
ARMS = [("ordinary", None), ("context", "context_representatives_2_v1"),
        ("quality", "quality_representatives_2_v1")]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    a = p.parse_args()
    root = a.experiment.resolve()
    source = root / "source-context-002"
    build = root / "builds/context-002"
    sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    assert sha(build / "nes-eval") == BINARY
    gate_path = root / "runs/r04b-development-analysis.json"
    gate = json.loads(gate_path.read_text())
    assert gate["decision"]["qualifies_one_new_development_seed"]
    # Inspect seed identifiers only; outcomes do not select the new seed.
    for path in (root / "runs").rglob("summary.json"):
        assert json.loads(path.read_text()).get("search_request", {}).get("seed") != SEED, "seed already used"
    path = root / "runs/r05-results.json"
    assert not path.exists(), "refusing to replace R05"
    report = {"format": "r05-fresh-context-replication-v1", "seed": SEED,
              "scope": "one fresh development seed, not held-out validation",
              "prior_gate_sha256": sha(gate_path), "binary_sha256": BINARY,
              "cpus": "8-11", "fixed_arm_order": [label for label, _ in ARMS],
              "records": [], "execution_complete": False}
    path.write_text(json.dumps(report, indent=2) + "\n")
    for label, policy in ARMS:
        suite = json.loads((source / "benchmarks/search/retention-theory/k01-smoke.json").read_text())
        suite["id"] = f"retention-theory-r05-{label}"
        suite["seeds"] = [SEED]
        suite["search"].update({"metroid_terminal": "death_or_bcd_underflow_or_ending_v3",
                                 "executions": 500000, "frames": 50000000,
                                 "wall_seconds": 1800, "verification": "witness"})
        if policy:
            suite["search"]["slot_retention"] = policy
        manifest = root / f"r05-{label}.json"
        out = root / f"runs/r05-{label}"
        assert not out.exists()
        manifest.write_text(json.dumps(suite, indent=2) + "\n")
        command = ["taskset", "-c", "8-11", "python3", str(source / "benchmarks/search/eval.py"),
                   "run", str(manifest), "--assets", str(root / "assets.json"),
                   "--binary", str(build / "nes-eval"), "--build-info", str(build / "build-info.json"),
                   "--out", str(out), "--jobs", "1", "--cpus", "4",
                   "--memory-capacity-mib", "10240", "--finish-seconds", "120", "--disk-limit-gib", "4"]
        with (root / f"runs/r05-{label}.log").open("w") as log:
            status = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=1980)
        paths = list(out.glob("*/summary.json"))
        summary = json.loads(paths[0].read_text()) if len(paths) == 1 else None
        report["records"].append({"label": label, "policy": policy, "command": command,
                                  "manifest_sha256": sha(manifest), "exit_code": status.returncode,
                                  "summary": summary})
        path.write_text(json.dumps(report, indent=2) + "\n")
        assert status.returncode == 0 and summary is not None and summary["status"] == "complete", "infrastructure failure; preserve and stop"
        print(json.dumps({"label": label, "frames": summary["result"]["frames_emulated"],
                          "stop_reason": summary["result"]["stop_reason"]}), flush=True)
    report["execution_complete"] = True
    path.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
