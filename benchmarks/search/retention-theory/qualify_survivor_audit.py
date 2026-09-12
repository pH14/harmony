#!/usr/bin/env python3
"""Four bounded stream-identity checks; no long-search escalation."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_bounded(command, log, out):
    started = time.monotonic()
    with log.open("w") as stream:
        process = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            while process.poll() is None:
                if time.monotonic() - started > 120:
                    raise RuntimeError("qualification reached120-second external watchdog")
                if sum(p.stat().st_size for p in out.rglob("*") if p.is_file()) > 1024**3:
                    raise RuntimeError("qualification reached1GiB cell output bound")
                time.sleep(.5)
            assert process.returncode == 0, f"qualification exit {process.returncode}"
            assert sum(p.stat().st_size for p in out.rglob("*") if p.is_file()) <= 1024**3
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
    return time.monotonic() - started


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--source", type=Path, required=True)
    p.add_argument("--revision", choices=["001", "002"], default="001")
    a = p.parse_args()
    root, source = a.experiment.resolve(), a.source.resolve()
    prior = json.loads((root / "runs/r04b-qualify-results.json").read_text())
    assert prior["passed"]
    out = root / ("runs/u01-qualify" if a.revision == "001" else "runs/u01-qualify-002")
    out.mkdir()
    result = out / "results.json"
    report = {"format": "u01-complete-local-survivors-qualification-v1",
              "scope": "reporting-only stream identity; no performance claim",
              "prior_sha256": sha(root / "runs/r04b-qualify-results.json"),
              "cpus": "8-11", "helper_wall_seconds": 120, "records": [], "passed": False}
    report["revision"] = a.revision

    def save():
        result.write_text(json.dumps(report, indent=2) + "\n")

    save()
    labels = ["default-metroid", "default-mm2", "quality", "context"] if a.revision == "001" else ["default-metroid", "quality"]
    for label in labels:
        old = next(r for r in prior["records"] if r["label"] == label)
        suite = json.loads((root / f"r04b-qualify-{label}.json").read_text())
        # Retain every prior search/header field, including its original bounds.
        # The external120s watchdog is tighter and does not change stream identity.
        suite["id"] = f"retention-theory-u01-qualify-{label}"
        manifest = out / f"{label}.json"
        manifest.write_text(json.dumps(suite, indent=2) + "\n")
        build = root / "builds" / (f"survivor-default-{a.revision}" if label.startswith("default") else f"survivor-motion-{a.revision}")
        info = json.loads((build / "build-info.json").read_text())
        assert sha(build / "nes-eval") == info["binary_sha256"]
        destination = out / label
        command = ["taskset", "-c", "8-11", "python3", str(source / "benchmarks/search/eval.py"),
                   "run", str(manifest), "--assets", str(root / "assets.json"),
                   "--binary", str(build / "nes-eval"), "--build-info", str(build / "build-info.json"),
                   "--out", str(destination), "--jobs", "1", "--cpus", "4",
                   "--memory-capacity-mib", "10240", "--finish-seconds", "60", "--disk-limit-gib", "1"]
        record = {"label": label, "build": info, "passed": False}
        report["records"].append(record)
        try:
            record["elapsed_seconds"] = run_bounded(command, out / f"{label}.log", destination)
            paths = list(destination.glob("*/summary.json"))
            assert len(paths) == 1
            summary = json.loads(paths[0].read_text())
            record["summary"] = summary
            assert summary["status"] == "complete"
            measured = summary["result"]
            assert measured["verification"] == "campaign" and measured["stop_reason"] != "wall_limit"
            assert measured["stream_sha256"] == old["summary"]["result"]["stream_sha256"]
            if label != "default-mm2":
                audit_path = paths[0].parent / "campaign/retention-audit.json"
                audit = json.loads(audit_path.read_text())
                record["audit_sha256"] = sha(audit_path)
                record["audit_format"] = audit["format"]
                if a.revision == "002":
                    previous = list((root / f"runs/u01-qualify/{label}").glob("*/campaign/retention-audit.json"))
                    assert len(previous) == 1 and sha(previous[0]) == sha(audit_path), "capacity repair changed audit bytes"
                if label == "default-metroid":
                    previous = list((root / f"runs/r04b-qualify-{label}").glob("*/campaign/retention-audit.json"))
                    assert len(previous) == 1 and sha(previous[0]) == sha(audit_path)
                    assert audit["format"] == "metroid-retention-audit-v1"
                else:
                    assert audit["format"] == "metroid-retention-audit-v2-local-survivors"
                    complete = [p["complete_competition"] for group in audit["samples"] for p in group if p["complete_competition"]]
                    assert complete and all(1 <= len(c["incumbents"]) <= 2 for c in complete)
                    record["complete_records"] = len(complete)
                    record["two_incumbent_records"] = sum(len(c["incumbents"]) == 2 for c in complete)
                    record["diagnostic_action_capacity_bytes"] = audit["diagnostic_action_capacity_bytes"]
                    record["incomplete_counters"] = {k: audit[k] for k in ["complete_missing_snapshot", "complete_oversized_input", "complete_unsupported_slot"]}
            record["passed"] = True
            print(json.dumps({"label": label, "stream_sha256": measured["stream_sha256"], "passed": True}), flush=True)
        except Exception as error:
            record["error"] = str(error)
            raise
        finally:
            save()
    report["passed"] = True
    save()


if __name__ == "__main__":
    main()
