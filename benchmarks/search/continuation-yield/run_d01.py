#!/usr/bin/env python3
"""Run the frozen, serial four-control calibration with bounded resources."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess

from audit_controls import fingerprint, milestone_interval


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    parser.add_argument("--registration", type=Path, required=True)
    parser.add_argument("--registration-commit", required=True)
    args = parser.parse_args()
    root = args.experiment.resolve()
    registration = json.loads(args.registration.read_text())
    assert sha(Path(__file__)) == registration["runner_sha256"]
    assert sha(Path(__file__).with_name("audit_controls.py")) == registration["analyzer_sha256"]
    build = root / registration["build"]
    binary = build / "nes-eval"
    assert sha(binary) == registration["baseline_fingerprint"]["binary_sha256"]
    template = root / registration["baseline_manifest"]
    assert sha(template) == registration["baseline_manifest_sha256"]
    deadline = datetime.fromisoformat(registration["phase_deadline_utc"])
    out = root / "runs/renewal-20260909/d01"
    assert not out.exists(), "refusing to replace prior calibration"
    seeds = [item["seed"] for item in registration["seeds"]]
    assert len(seeds) == len(set(seeds)) == 4
    for path in (root / "runs").rglob("summary.json"):
        used = json.loads(path.read_text()).get("search_request", {}).get("seed")
        assert used not in seeds, "calibration seed has already been used"
    out.mkdir(parents=True)
    report_path = out / "results.json"
    report = {"format": "continuation-yield-d01-results-v1",
              "registration_sha256": sha(args.registration),
              "source_registration_commit": args.registration_commit,
              "records": [], "execution_complete": False,
              "allocation_stop": None, "actual_admitted_frames": 0}

    def save():
        report_path.write_text(json.dumps(report, indent=2) + "\n")

    save()
    for index, seed in enumerate(seeds):
        remaining = (deadline - datetime.now(timezone.utc)).total_seconds()
        if remaining < registration["subprocess_timeout_seconds"]:
            report["allocation_stop"] = "insufficient phase time for another complete bounded cell"
            save()
            return
        suite = json.loads(template.read_text())
        suite["id"] = f"continuation-yield-d01-control-{index}"
        suite["seeds"] = [seed]
        suite["search"].update({"frames": registration["budget_frames_per_cell"],
                                 "executions": registration["executions_per_cell"],
                                 "wall_seconds": registration["search_wall_seconds"],
                                 "verification": registration["verification"]})
        manifest = out / f"control-{index}.json"
        manifest.write_text(json.dumps(suite, indent=2) + "\n")
        destination = out / f"control-{index}"
        command = ["taskset", "-c", registration["cpus"], "python3",
                   str(root / registration["source"] / "benchmarks/search/eval.py"),
                   "run", str(manifest), "--assets", str(root / "assets.json"),
                   "--binary", str(binary), "--build-info", str(build / "build-info.json"),
                   "--out", str(destination), "--jobs", "1", "--cpus", "4",
                   "--memory-capacity-mib", "10240", "--finish-seconds",
                   str(registration["finish_seconds"]), "--disk-limit-gib",
                   str(registration["disk_limit_gib_per_cell"])]
        print(json.dumps({"starting_control": index, "seed": seed,
                          "nominal_frame_ceiling": registration["budget_frames_per_cell"]}), flush=True)
        with (out / f"control-{index}.log").open("w") as log:
            process = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                                     timeout=registration["subprocess_timeout_seconds"])
        summaries = list(destination.glob("*/summary.json"))
        summary = json.loads(summaries[0].read_text()) if len(summaries) == 1 else None
        record = {"index": index, "seed": seed, "command": command,
                  "manifest_sha256": sha(manifest), "exit_code": process.returncode,
                  "summary": summary}
        report["records"].append(record)
        if summary is not None:
            record["summary_sha256"] = sha(summaries[0])
            progress = summaries[0].parent / "campaign/progress.jsonl"
            if progress.is_file():
                assert progress.stat().st_size <= 128 * 1024**2
                record["progress_sha256"] = sha(progress)
                record["endpoint_evidence"] = milestone_interval(
                    [json.loads(line) for line in progress.read_text().splitlines()],
                    registration["budget_frames_per_cell"])
            if "result" in summary:
                report["actual_admitted_frames"] += summary["result"]["frames_emulated"]
        save()
        assert process.returncode == 0 and summary is not None and summary["status"] == "complete", "infrastructure failure; stop and preserve"
        assert fingerprint(summary) == registration["baseline_fingerprint"], "baseline identity changed"
        assert summary["result"]["verification"] == "witness"
        if not record["endpoint_evidence"]["observed_full_budget"]:
            report["allocation_stop"] = "cell did not observe the full common frame budget"
            save()
            return
        print(json.dumps({"completed_control": index, "seed": seed,
                          "frames": summary["result"]["frames_emulated"],
                          "endpoint": record["endpoint_evidence"]}), flush=True)
    report["execution_complete"] = True
    save()


if __name__ == "__main__":
    main()
