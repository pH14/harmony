#!/usr/bin/env python3
"""Execute a frozen finite panel, preserving each completed or failed cell."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess

from audit_controls import milestone_interval


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
    if "screen" in registration:
        from score_screen import score_screen
        assert sha(Path(__file__).with_name("score_screen.py")) == registration["screen_scorer_sha256"]
    if "victory_endpoint_sha256" in registration:
        from victory_endpoint import victory_endpoint
        assert sha(Path(__file__).with_name("victory_endpoint.py")) == registration["victory_endpoint_sha256"]
    build = root / registration["build"]
    binary = build / "nes-eval"
    assert sha(binary) == registration["binary_sha256"]
    assert sha(build / "build-info.json") == registration["build_info_sha256"]
    for required in registration.get("required_evidence", []):
        evidence_path = root / required["path"]
        assert sha(evidence_path) == required["sha256"], "qualification evidence changed"
        evidence = json.loads(evidence_path.read_text())
        if "screen_decision" in required:
            assert evidence["execution_complete"] and evidence["allocation_stop"] is None
            assert evidence["screen"]["decision"] == required["screen_decision"], "required screen gate did not pass"
        for cell_id in required["passed_cells"]:
            matching = [row for row in evidence["records"] if row["id"] == cell_id]
            assert len(matching) == 1 and matching[0].get("checks_passed"), "missing qualification"
    deadline = datetime.fromisoformat(registration["deadline_utc"])
    if "screen" in registration:
        seeds = [pair["seed"] for pair in registration["screen"]["pairs"]]
        assert len(seeds) == len(set(seeds)) == 4
        for path in (root / "runs").rglob("summary.json"):
            used = json.loads(path.read_text()).get("search_request", {}).get("seed")
            assert used not in seeds, "paired development seed has already been used"
    out = root / registration["output"]
    out.mkdir(parents=True, exist_ok=False)
    report_path = out / "results.json"
    report = {"format": "continuation-yield-panel-results-v1",
              "registration_sha256": sha(args.registration),
              "registration_commit": args.registration_commit,
              "records": [], "execution_complete": False, "allocation_stop": None}

    def save():
        report_path.write_text(json.dumps(report, indent=2) + "\n")

    save()
    for cell in registration["cells"]:
        remaining = (deadline - datetime.now(timezone.utc)).total_seconds()
        if remaining < cell["subprocess_timeout_seconds"]:
            report["allocation_stop"] = "insufficient time for the next complete bounded cell"
            save()
            return
        manifest = out / (cell["id"] + ".json")
        manifest.write_text(json.dumps(cell["manifest"], indent=2) + "\n")
        destination = out / cell["id"]
        command = ["taskset", "-c", registration["cpus"], "python3",
                   str(root / registration["source"] / "benchmarks/search/eval.py"),
                   "run", str(manifest), "--assets", str(root / "assets.json"),
                   "--binary", str(binary), "--build-info", str(build / "build-info.json"),
                   "--out", str(destination), "--jobs", "1", "--cpus", "4",
                   "--memory-capacity-mib", "10240", "--finish-seconds",
                   str(cell["finish_seconds"]), "--disk-limit-gib",
                   str(cell["disk_limit_gib"])]
        record = {"id": cell["id"], "command": command,
                  "manifest_sha256": sha(manifest), "started_utc": datetime.now(timezone.utc).isoformat(),
                  "exit_code": None, "summary": None}
        report["records"].append(record)
        save()
        print(json.dumps({"starting": cell["id"]}), flush=True)
        try:
            with (out / (cell["id"] + ".log")).open("w") as log:
                process = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                                         timeout=cell["subprocess_timeout_seconds"])
            record["exit_code"] = process.returncode
            summaries = list(destination.glob("*/summary.json"))
            if len(summaries) == 1:
                summary = json.loads(summaries[0].read_text())
                record["summary"] = summary
                record["summary_sha256"] = sha(summaries[0])
                progress = summaries[0].parent / "campaign/progress.jsonl"
                if progress.is_file():
                    assert progress.stat().st_size <= 128 * 1024**2
                    record["progress_sha256"] = sha(progress)
                    if (summary.get("identity") or {}).get("game") == "metroid":
                        record["endpoint_evidence"] = milestone_interval(
                            [json.loads(line) for line in progress.read_text().splitlines()],
                            cell["manifest"]["search"]["frames"])
                if (summary.get("identity") or {}).get("game") == "mm2" and summary.get("result") and "victory_endpoint_sha256" in registration:
                    record["endpoint_evidence"] = victory_endpoint(summary["result"], cell["manifest"]["search"]["frames"])
            save()
            assert process.returncode == 0 and record["summary"] is not None, "execution failure"
            summary = record["summary"]
            assert summary["status"] == "complete", "incomplete cell"
            assert summary["build"]["binary_sha256"] == registration["binary_sha256"]
            assert summary["result"]["verification"] == cell["manifest"]["search"]["verification"]
            assert summary["result"]["stop_reason"] in cell["allowed_stops"], "unexpected censoring"
            for key, value in cell.get("expected_result", {}).items():
                assert summary["result"][key] == value, "frozen compatibility result differs: " + key
            for key, value in cell.get("expected_identity", {}).items():
                assert summary["identity"][key] == value, "frozen identity differs: " + key
            record["checks_passed"] = True
        except Exception as error:
            record["failure"] = str(error)
            report["allocation_stop"] = "cell failure; no subsequent cells dispatched"
            save()
            raise
        finally:
            record["finished_utc"] = datetime.now(timezone.utc).isoformat()
            save()
        print(json.dumps({"completed": cell["id"], "frames": summary["result"]["frames_emulated"]}), flush=True)
        if "screen" in registration:
            report["screen"] = score_screen(report["records"], registration)
            save()
            if report["screen"]["decision"] == "fail_impossible_win_count":
                report["allocation_stop"] = "registered strict-win criterion is mathematically unattainable"
                save()
                return
    report["execution_complete"] = True
    save()


if __name__ == "__main__":
    main()
