#!/usr/bin/env python3
"""Freeze one prefix-observing build and require unchanged full U01 outputs."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import eval as evaluator
from run_p05 import CORE, ROM, run_bounded, sha

DEADLINE = datetime(2026, 9, 9, 8, 35, tzinfo=timezone.utc).timestamp()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--source", type=Path, required=True)
    p.add_argument("--commit", required=True)
    a = p.parse_args()
    root, source = a.experiment.resolve(), a.source.resolve()
    registration = root / "runs/u01/registration.json"
    assert sha(registration) == "5062c0dd65883f2e94929a7fd8d3010b029177d2e7cf2ae64ce1eb636ceb1192"
    audit = root / "runs/u01/expanded-audit.json"
    assert sha(audit) == "051df1c0cb73b43920b7ae61f45b77736b84fde9a382ef64e2b814f56049f95f"
    old = root / "runs/u01/suffix"
    assert sha(old / "outcomes.jsonl") == "2fe0e9e4dcc8602cb9c54d8a88f45d6fb8542a66d8cfbccddd5f7b590e9cf8f8"
    assert sha(old / "suffixes.json") == "7d3c0dcb69c1ac7c70dda88a0b3e9386a4c3d25b090d6510bdb25de8cfe8793d"
    assert sha(root / "assets/quicknes_libretro.so") == CORE and sha(root / "assets/metroid.nes") == ROM
    out = root / "runs/h02"
    out.mkdir()
    result = {"format": "h02-prefix-probe-execution-v1", "complete": False,
              "registration_sha256": sha(registration), "scope": "same frozen development competitors; no fresh search"}
    result_path = out / "results.json"
    result_path.write_text(json.dumps(result, indent=2) + "\n")
    try:
        build = root / "builds/prefix-probe-001"
        build.mkdir()
        identity = evaluator.source_identity(source)
        environment = {**os.environ, "HARMONY_SEARCH_SOURCE_SHA256": identity["source_tree_sha256"]}
        assert DEADLINE - time.time() > 150
        with (build / "build.log").open("w") as log:
            subprocess.run(["cargo", "build", "--release", "--locked", "--manifest-path", str(source / "workloads/nes/Cargo.toml"),
                            "--bin", "metroid-retention-probe", "--target-dir", str(root / "target"), "-j", "4"],
                           env=environment, cwd=source, stdout=log, stderr=subprocess.STDOUT,
                           check=True, timeout=min(300, DEADLINE-time.time()-125))
        assert evaluator.source_identity(source) == identity
        binary = build / "metroid-retention-probe"
        shutil.copy2(root / "target/release/metroid-retention-probe", binary)
        info = {"format": "harmony-diagnostic-build-v1", **identity, "source_commit": a.commit,
                "binary": "metroid-retention-probe", "binary_sha256": sha(binary), "features": "", "locked": True,
                "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
                "cargo": subprocess.check_output(["cargo", "-V"], text=True).strip()}
        (build / "build-info.json").write_text(json.dumps(info, indent=2) + "\n")
        result["build"] = info
        assert DEADLINE - time.time() > 125
        destination = out / "suffix"
        command = ["taskset", "-c", "8", str(binary), str(root / "assets/quicknes_libretro.so"), str(root / "assets/metroid.nes"),
                   str(audit), str(destination), "16", "24", "death_or_bcd_underflow_or_ending_v3", "20261215", "6"]
        result["elapsed_seconds"] = run_bounded(command, out / "probe.log", out)
        result["full_output_sha256"] = {name: sha(destination / name) for name in ["summary.json", "outcomes.jsonl", "suffixes.json"]}
        assert all(digest == sha(old / name) for name, digest in result["full_output_sha256"].items()), "prefix observation changed full U01 result"
        summary = json.loads((destination / "summary.json").read_text())
        physical = summary["prefix_and_gain_export_frames"] + sum(summary["probe_frames_discarded_survivor"])
        assert physical <= 3_474_703 and physical == 431_144
        metadata = json.loads((destination / "prefix-metadata.json").read_text())
        assert metadata["prefix_horizons"] == list(range(1, 7))
        result.update({"summary": summary, "prefix_metadata": metadata,
                       "physical_frames": physical, "complete": True, "full_outputs_unchanged": True})
    except Exception as error:
        result["error"] = str(error)
        raise
    finally:
        result_path.write_text(json.dumps(result, indent=2) + "\n")
    subprocess.run(["python3", str(source / "benchmarks/search/retention-theory/analyze_h02.py"),
                    "--registration", str(registration), "--outcomes", str(destination / "outcomes.jsonl"),
                    "--prefixes", str(destination / "prefixes.jsonl"), "--out", str(out / "analysis.json")], check=True, timeout=30)


if __name__ == "__main__":
    main()
