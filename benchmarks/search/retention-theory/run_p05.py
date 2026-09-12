#!/usr/bin/env python3
"""Bounded replay-only diagnostic on frozen R05 ordinary competitors."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time

SOURCE_AUDIT = "d7c2e2c30741d1bd7a7a44384ee57cb1817a01c6d85e81a0fee69be218e236bd"
CORE = "5a65587bf6faa5bc86ea05648b81b0e01e5f639ea5020166a14b5d96a92a3db0"
ROM = "e6e6b7014685adae447ebb3833242815747bc1e5df83ade79f693fb67cf565b6"
SEED, TRIALS, ACTIONS, SETUP_BOUND = 20261214, 16, 24, 4096
BUILDS = [("motion", "motion-probe-002", "metroid-kinematics-probe", "d48042d84b6f055773131046c66a55bc522a3f92cba4c7304626e5a0a793a08c"),
          ("suffix", "probe-001", "metroid-retention-probe", "20f0255bc4002d4e43ffecbfd9e0efc053c7656c2b06272bc4fbc16e7eff9672")]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def route_frames(value):
    assert value["actions"] and len(value["actions"]) <= 8192
    frames = sum(max(1, min(120, action["hold_frames"])) for action in value["actions"])
    assert frames <= 250_000
    return frames


def work_bound(pairs):
    prefix = sum(route_frames(pair[k]) + SETUP_BOUND for pair in pairs for k in ["candidate_input", "incumbent_input"])
    suffix = len(pairs) * 2 * TRIALS * ACTIONS * 120
    exports = TRIALS * sum(route_frames(pair["incumbent_input" if pair["replaces"] else "candidate_input"]) + SETUP_BOUND + ACTIONS * 120 for pair in pairs)
    return {"prefix_upper_bound": prefix, "suffix_upper_bound": suffix,
            "gain_exports_upper_bound": exports, "total_upper_bound": prefix + suffix + exports}


def run_bounded(command, log, out):
    started = time.monotonic()
    with log.open("w") as stream:
        process = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            while process.poll() is None:
                if time.monotonic() - started > 120:
                    raise RuntimeError("diagnostic reached 120-second wall bound")
                if sum(p.stat().st_size for p in out.rglob("*") if p.is_file()) > 64 * 1024**2:
                    raise RuntimeError("diagnostic reached output bound")
                time.sleep(.5)
            assert process.returncode == 0, f"diagnostic exit {process.returncode}"
            assert sum(p.stat().st_size for p in out.rglob("*") if p.is_file()) <= 64 * 1024**2
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
    return time.monotonic() - started


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    a = parser.parse_args()
    root = a.experiment.resolve()
    source = root / "runs/r05-ordinary/metroid-full-s20261210-w4-m8192/campaign/retention-audit.json"
    assert sha(source) == SOURCE_AUDIT
    assert sha(root / "assets/quicknes_libretro.so") == CORE and sha(root / "assets/metroid.nes") == ROM
    prior = json.loads((root / "runs/r05-analysis.json").read_text())
    assert not prior["decision"]["qualifies_one_longer_development_pair"]
    for path in (root / "runs").rglob("summary.json"):
        assert json.loads(path.read_text()).get("seed") != SEED, "probe seed already used"
    audit = json.loads(source.read_text())
    pairs = audit["samples"][3]
    assert len(pairs) == 16
    for pair in pairs:
        for key in ["health", "missiles", "equipment", "bosses", "missile_capacity", "energy_tanks"]:
            assert pair["candidate"][key] == pair["incumbent"][key]
    bound = work_bound(pairs)
    assert bound["total_upper_bound"] <= 12_000_000
    audit["samples"] = [pairs if index == 3 else [] for index in range(len(audit["samples"]))]
    out = root / "runs/p05"
    out.mkdir()
    subset = root / "p05-audit.json"
    assert not subset.exists()
    subset.write_text(json.dumps(audit, indent=2) + "\n")
    report = {"format": "p05-recorded-competitor-diagnostic-v1", "scope": "diagnostic only; failed R05 search family stays closed",
              "source_audit_sha256": SOURCE_AUDIT, "subset_audit_sha256": sha(subset),
              "seed": SEED, "trials_per_pair": TRIALS, "actions_per_trial": ACTIONS,
              "suffix_work_bound": bound, "records": [], "complete": False}
    result = out / "results.json"
    result.write_text(json.dumps(report, indent=2) + "\n")
    for label, name, binary, digest in BUILDS:
        build = root / "builds" / name
        assert sha(build / binary) == digest
        destination = out / label
        command = ["taskset", "-c", "8", str(build / binary), str(root / "assets/quicknes_libretro.so"),
                   str(root / "assets/metroid.nes"), str(subset), str(destination)]
        if label == "suffix":
            command += [str(TRIALS), str(ACTIONS), "death_or_bcd_underflow_or_ending_v3", str(SEED)]
        record = {"label": label, "build": json.loads((build / "build-info.json").read_text()), "command": command}
        report["records"].append(record)
        try:
            record["elapsed_seconds"] = run_bounded(command, out / f"{label}.log", out)
            summary = json.loads((destination / "summary.json").read_text())
            assert summary["audit_sha256"] == sha(subset)
            assert summary["core_sha256"] == CORE and summary["rom_sha256"] == ROM
            physical = summary["physical_frames"] if label == "motion" else summary["prefix_and_gain_export_frames"] + sum(summary["probe_frames_discarded_survivor"])
            assert physical <= (2_000_000 if label == "motion" else bound["total_upper_bound"])
            record.update({"summary": summary, "physical_frames": physical, "passed": True})
        except Exception as error:
            record.update({"passed": False, "error": str(error)})
            raise
        finally:
            result.write_text(json.dumps(report, indent=2) + "\n")
    report["complete"] = True
    report["physical_frames"] = sum(record["physical_frames"] for record in report["records"])
    result.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"complete": True, "physical_frames": report["physical_frames"]}), flush=True)


if __name__ == "__main__":
    main()
