#!/usr/bin/env python3
"""Qualify corrected Metroid builds, then isolate archive identity in one pair."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import subprocess

CORRECTED = "death_or_bcd_underflow_or_ending_v3"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    parser.add_argument("--phase", choices=["qualify", "development"], required=True)
    args = parser.parse_args()
    root = args.experiment.resolve()
    source = root / "source-corrected-refinement-001"
    records = []

    def run(stage, label, variant, terminal, cpus):
        build = root / "builds" / f"corrected-{variant}-001"
        suite = json.loads((root / f"k01-{stage}.json").read_text())
        assert "slot_retention" not in suite["search"]
        if terminal:
            suite["search"]["metroid_terminal"] = terminal
        manifest = root / f"k01-{stage}-{label}.json"
        manifest.write_text(json.dumps(suite, indent=2) + "\n")
        out = root / "runs" / f"k01-{stage}-{label}"
        subprocess.run([
            "taskset", "-c", cpus, "python3", str(source / "benchmarks/search/eval.py"),
            "run", str(manifest), "--assets", str(root / "assets.json"),
            "--binary", str(build / "nes-eval"), "--build-info", str(build / "build-info.json"),
            "--out", str(out), "--jobs", "1", "--cpus", "4",
            "--memory-capacity-mib", "10240", "--finish-seconds", "120",
            "--disk-limit-gib", "4",
        ], check=True)
        paths = list(out.glob("*/summary.json"))
        assert len(paths) == 1, "missing or duplicate cell"
        summary = json.loads(paths[0].read_text())
        assert summary["status"] == "complete", "cell did not finish and verify"
        assert summary["result"]["stop_reason"] != "wall_limit", "work budget was censored"
        assert summary["result"]["verification"] == ("campaign" if stage == "smoke" else "witness")
        record = {"stage": stage, "label": label,
                  "build": json.loads((build / "build-info.json").read_text()), "summary": summary}
        print(json.dumps({"stage": stage, "label": label, "status": "verified",
                          "stream_sha256": summary["result"]["stream_sha256"]}), flush=True)
        return record

    def panel(cells):
        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(run, *cell) for cell in cells]
            return [f.result() for f in futures]

    def save(passed):
        (root / "runs" / f"k01-{args.phase}-results.json").write_text(
            json.dumps({"passed": passed, "records": records}, indent=2) + "\n")

    if args.phase == "qualify":
        baseline = json.loads((root / "runs/b02-results.json").read_text())
        assert len(baseline) == 2
        records.extend(panel([
            ("smoke", "legacy-default", "default", None, "0-3"),
            ("smoke", "corrected-refined", "refined", CORRECTED, "8-11"),
        ]))
        save(False)
        assert records[0]["summary"]["result"]["stream_sha256"] == baseline[0]["result"]["stream_sha256"], "legacy smoke changed"
        records.extend(panel([
            ("compatibility", "legacy-default", "default", None, "0-3"),
            ("smoke", "corrected-default", "default", CORRECTED, "8-11"),
        ]))
        save(False)
        assert records[2]["summary"]["result"]["stream_sha256"] == baseline[1]["result"]["stream_sha256"], "legacy compatibility stream changed"
        save(True)
    else:
        qualification = json.loads((root / "runs/k01-qualify-results.json").read_text())
        assert qualification["passed"] and len(qualification["records"]) == 4
        records.extend(panel([
            ("development", "corrected-default", "default", CORRECTED, "0-3"),
            ("development", "corrected-refined", "refined", CORRECTED, "8-11"),
        ]))
        save(True)


if __name__ == "__main__":
    main()
