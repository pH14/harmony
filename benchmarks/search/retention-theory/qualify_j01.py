#!/usr/bin/env python3
"""Replay existing R03b Metal victories through the standard Heat bridge."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--experiment", type=Path, required=True)
parser.add_argument("--out", type=Path)
parser.add_argument("--build", type=Path)
args = parser.parse_args()
root = args.experiment.resolve()
out = args.out or root / "runs/j01-export-qualification"
out.mkdir()
build = args.build or root / "builds/metal-export-001"
metadata = json.loads((build / "build-info.json").read_text())
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
assert digest(build / "mm2-metal-export") == metadata["binary_sha256"]
progress = root / "builds/nes-progress-001"
assert digest(progress) == "39d69981dd8086e48914a8c5e53789be282ac16924668dcd3327c18f721e564e"
qualified = json.loads((root / "runs/r03b-mm2-results.json").read_text())
assert qualified["passed"] and len(qualified["records"]) == 2
report = {"format": "j01-existing-victory-bridge-qualification-v1", "passed": False,
          "scope": "no search; existing development victories only", "build": metadata,
          "progress_binary_sha256": digest(progress), "records": []}
for label, policy in [("sample", "representative_job_sample_2_v1"), ("extremes", "resource_extremes_2_v1")]:
    cell = root / "runs" / f"r03b-mm2-mm2-{policy}" / "mm2-metal-s20261101-w4-m8192"
    summary = json.loads((cell / "summary.json").read_text())
    assert summary["status"] == "complete" and summary["result"]["solved"]
    assert summary["search_request"]["slot_retention"] == policy
    victory = cell / "campaign/victory-input.json"
    destination = out / label
    with (out / f"{label}-export.log").open("w") as log:
        subprocess.run([str(build / "mm2-metal-export"), str(root / "assets/quicknes_libretro.so"),
                        str(root / "assets/mm2.nes"), str(victory), str(destination)],
                       stdout=log, stderr=subprocess.STDOUT, timeout=120, check=True)
    exported = json.loads((destination / "summary.json").read_text())
    assert exported["input_sha256"] == digest(victory)
    prefix = destination / "next-prefix.json"
    assert exported["prefix_sha256"] == digest(prefix)
    bridge = out / f"{label}-bridge.json"
    with bridge.open("w") as log:
        subprocess.run([str(progress), "mm2", str(root / "assets/quicknes_libretro.so"),
                        str(root / "assets/mm2.nes"), str(prefix), "heat"],
                       stdout=log, stderr=subprocess.PIPE, timeout=120, check=True)
    replay = json.loads(bridge.read_text())
    assert replay["verified_replays"] == 2
    assert replay["result"]["endpoint"]["stage"] == 0
    assert replay["result"]["endpoint"]["weapons_obtained"] == 64
    prefix_frames = sum(a["hold_frames"] for a in json.loads(prefix.read_text())["actions"])
    record = {"label": label, "source_summary_sha256": digest(cell / "summary.json"),
              "export": exported, "bridge": replay,
              "bridge_physical_frames": 2 * (prefix_frames + replay["result"]["setup_frames_after_tape"])}
    report["records"].append(record)
    (out / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"label": label, "verified_heat_bridge": True}), flush=True)
report["passed"] = True
(out / "results.json").write_text(json.dumps(report, indent=2) + "\n")
