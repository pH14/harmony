#!/usr/bin/env python3
"""A03: two preregistered paired Heat diagnostics from one fixed searched start."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    args = parser.parse_args()
    experiment = args.experiment.resolve()
    source = experiment / "source-coverage-001"
    base = json.loads((experiment / "a02.json").read_text())
    prefix = Path(base["search"]["prefix_input"])
    assert hashlib.sha256(prefix.read_bytes()).hexdigest() == base["search"]["prefix_sha256"]
    for seed, policies in [
        (20261102, ["extremes", "coverage"]),
        (20261103, ["coverage", "extremes"]),
    ]:
        suite = json.loads(json.dumps(base))
        suite["id"] = f"retention-theory-a03-s{seed}"
        suite["seeds"] = [seed]
        suite["search"].update(executions=100000, frames=12000000, wall_seconds=600)
        suite["search"].pop("slot_retention")
        suite["cases"] = [
            {**base["cases"][0], "id": f"mm2-heat-{policy}",
             "origin": "diagnostic from A01's searched Metal victory; not fresh validation",
             "search": {"slot_retention": f"resource_{policy}_2_v1"}}
            for policy in policies
        ]
        manifest = experiment / f"a03-s{seed}.json"
        manifest.write_text(json.dumps(suite, indent=2) + "\n")
        out = experiment / "runs" / f"a03-s{seed}"
        subprocess.run([
            "python3", str(source / "benchmarks/search/eval.py"), "run", str(manifest),
            "--assets", str(experiment / "assets.json"),
            "--binary", str(experiment / "builds/coverage-001/nes-eval"),
            "--build-info", str(experiment / "builds/coverage-001/build-info.json"),
            "--out", str(out), "--jobs", "2", "--cpus", "8",
            "--memory-capacity-mib", "20480", "--finish-seconds", "120",
            "--disk-limit-gib", "4",
        ], check=True)
        for path in sorted(out.glob("*/summary.json")):
            summary = json.loads(path.read_text())
            assert summary["status"] == "complete"
            assert summary["result"]["stop_reason"] != "wall_limit", "censored diagnostic"
            records = [json.loads(line) for line in (path.parent / "campaign/progress.jsonl").open()]
            eligible = [r for r in records if r["frames_emulated"] <= 12000000]
            assert eligible, "no within-budget evidence"
            score = max(r["milestones"]["max_screen"] for r in eligible)
            print(json.dumps({"seed": seed, "case": summary["case"], "cpu_set": summary["cpu_set"],
                              "logged_screen_at_12m": score,
                              "frames": summary["result"]["frames_emulated"],
                              "stop": summary["result"]["stop_reason"]}), flush=True)


if __name__ == "__main__":
    main()
