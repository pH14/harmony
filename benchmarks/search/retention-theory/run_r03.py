#!/usr/bin/env python3
"""Bounded job-sample qualification and matched-capacity development panels."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import json
from pathlib import Path
import re
import subprocess

SAMPLE = "representative_job_sample_2_v1"
EXTREMES = "resource_extremes_2_v1"
TERMINAL = "death_or_bcd_underflow_or_ending_v3"
EXPECTED = "9df5aefec98188306cf68f7b5959a0b4eef88df9a6786b38a31bfad73c5ae94e"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    parser.add_argument("--phase", choices=["qualify", "metroid", "mm2"], required=True)
    parser.add_argument("--recheck", action="store_true")
    parser.add_argument("--resume", action="store_true", help="Reuse completed cells and run only absent cells")
    parser.add_argument("--run-id", default="r03")
    args = parser.parse_args()
    assert re.fullmatch(r"[a-z0-9-]+", args.run_id)
    root = args.experiment.resolve()
    source = root / "source-job-sample-001"
    research = source / "benchmarks/search/retention-theory"
    build = root / "builds/job-sample-001"
    records = []

    def save(passed):
        (root / "runs" / f"{args.run_id}-{args.phase}-results.json").write_text(
            json.dumps({"passed": passed, "records": sorted(records, key=lambda r: r["label"])}, indent=2) + "\n")

    def run(game, policy, cpus):
        label = f"{game}-{policy or 'legacy'}"
        suite = json.loads((research / ("k01-smoke.json" if game == "metroid" else "b01.json")).read_text())
        suite["id"] = f"retention-theory-{args.run_id}-{args.phase}-{label}"
        suite["cases"] = [case for case in suite["cases"] if case["game"] == game]
        suite["memory_mib"] = [8192]
        if game == "metroid":
            suite["search"]["metroid_terminal"] = TERMINAL
        if policy:
            suite["search"]["slot_retention"] = policy
        if args.phase != "qualify":
            suite["seeds"] = [3 if game == "metroid" else 20261101]
            suite["search"].update({
                "actions": 4096, "executions": 500000 if game == "metroid" else 100000,
                "frames": 50000000 if game == "metroid" else 12000000,
                "wall_seconds": 1800 if game == "metroid" else 600,
                "verification": "witness",
            })
            if game == "metroid":
                suite["search"]["retention_audit"] = True
        manifest = root / f"{args.run_id}-{args.phase}-{label}.json"
        out = root / "runs" / f"{args.run_id}-{args.phase}-{label}"
        if not args.recheck and not (args.resume and out.exists()):
            assert not out.exists(), "refusing to replace an experiment"
            manifest.write_text(json.dumps(suite, indent=2) + "\n")
            subprocess.run([
                "taskset", "-c", cpus, "python3", str(source / "benchmarks/search/eval.py"),
                "run", str(manifest), "--assets", str(root / "assets.json"),
                "--binary", str(build / "nes-eval"), "--build-info", str(build / "build-info.json"),
                "--out", str(out), "--jobs", "1", "--cpus", "4",
                "--memory-capacity-mib", "10240", "--finish-seconds", "120", "--disk-limit-gib", "4",
            ], check=True, timeout=suite["search"]["wall_seconds"] + 180)
        else:
            assert json.loads(manifest.read_text()) == suite
        paths = list(out.glob("*/summary.json"))
        assert len(paths) == 1, "missing or duplicate cell"
        summary = json.loads(paths[0].read_text())
        record = {"label": label, "game": game, "policy": policy, "cpus": cpus,
                  "build": json.loads((build / "build-info.json").read_text()), "summary": summary}
        return record

    def validate(record):
        summary = record["summary"]
        assert summary["status"] == "complete", "cell did not finish and verify"
        result = summary["result"]
        assert result["verification"] == ("campaign" if args.phase == "qualify" else "witness")
        if args.phase == "qualify":
            assert result["stop_reason"] != "wall_limit"
            if record["policy"] is None:
                assert result["stream_sha256"] == EXPECTED, "legacy corrected stream changed"
            elif record["policy"] == SAMPLE:
                assert summary["last_progress"]["retention_diagnostics"]["alternative_admissions"] > 0
            else:
                assert summary["last_progress"]["retention_diagnostics"]["resource_decisions"] > 0
        print(json.dumps({"label": record["label"], "verified": True,
                          "stream_sha256": result["stream_sha256"], "stop_reason": result["stop_reason"]}), flush=True)

    def panel(cells):
        with ThreadPoolExecutor(max_workers=2) as pool:
            for future in as_completed([pool.submit(run, *cell) for cell in cells]):
                record = future.result()
                records.append(record)
                save(False)
                validate(record)

    if args.phase == "qualify":
        # Establish exact default compatibility before any mechanism panel.
        panel([("metroid", None, "0-3")])
        for game in ["metroid", "mm2"]:
            panel([(game, SAMPLE, "0-3"), (game, EXTREMES, "8-11")])
    else:
        qualification = json.loads((root / "runs/r03-qualify-results.json").read_text())
        assert qualification["passed"] and len(qualification["records"]) == 5
        digest = json.loads((build / "build-info.json").read_text())["binary_sha256"]
        assert all(r["build"]["binary_sha256"] == digest for r in qualification["records"])
        if args.phase == "mm2":
            assert (root / "runs/r03-metroid-results.json").exists(), "Metroid pair must finish first"
        panel([(args.phase, SAMPLE, "0-3"), (args.phase, EXTREMES, "8-11")])
    save(True)


if __name__ == "__main__":
    main()
