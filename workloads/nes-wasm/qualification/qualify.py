#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Bounded, local full-state feasibility checks. Cross-host results are separate."""
import argparse
import json
from pathlib import Path
import statistics
import subprocess

BUDGETS = {
    "profile_memory_bytes": 16 * 1024 * 1024,
    "nova_frames": 120,
    "run_seconds": 0.5,
    "capture_seconds": 0.015,
    "capture_and_page_comparison_seconds": 0.015,
    "fresh_restore_including_decode_and_translation_seconds": 0.1,
    "snapshot_bytes": 17 * 1024 * 1024,
    "samples": 11,
}

def invoke(binary, module, mode, snapshot, report, rom=None):
    args = [str(binary), str(module), mode, str(snapshot), str(report)]
    if rom:
        args.extend((str(rom), str(BUDGETS["nova_frames"])))
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL)
    return json.loads(report.read_text())

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--floats", type=Path, required=True)
    parser.add_argument("--workload", type=Path, required=True)
    parser.add_argument("--rom", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    results = {}
    for name, module, rom in (("fixture", args.fixture, None), ("floats", args.floats, None), ("nova", args.workload, args.rom)):
        directory = output / name
        directory.mkdir(exist_ok=True)
        full = invoke(args.binary, module, "full", directory / "full.bin", directory / "full.json", rom)
        stopped = invoke(args.binary, module, "stop-import", directory / "stop.bin", directory / "stop.json", rom)
        resumed = invoke(args.binary, module, "resume", directory / "stop.bin", directory / "resume.json")
        if not stopped["suspended"] or stopped["pending"] is None:
            raise SystemExit(f"{name} did not stop at a pending import")
        if full["state_sha256"] != resumed["state_sha256"]:
            raise SystemExit(f"{name} fresh-process continuation differs from uninterrupted state")
        if name == "fixture":
            invoke(args.binary, module, "stop-loop", directory / "loop.bin", directory / "loop.json")
            loop = invoke(args.binary, module, "resume", directory / "loop.bin", directory / "loop-resume.json")
            if loop["state_sha256"] != full["state_sha256"]:
                raise SystemExit("unmodified-source loop continuation differs")
        results[name] = {"full": full, "stopped": stopped, "resumed": resumed}
    samples = []
    directory = output / "nova"
    for index in range(BUDGETS["samples"]):
        full = invoke(args.binary, args.workload, "full", directory / "sample.bin", directory / "sample.json", args.rom)
        restored = invoke(args.binary, args.workload, "resume", directory / "stop.bin", directory / "restore-sample.json")
        if full["state_sha256"] != restored["state_sha256"]:
            raise SystemExit("sampled state mismatch")
        sample = {"run_seconds": full["run_seconds"], "capture_seconds": full["capture_seconds"],
            "capture_and_page_comparison_seconds": restored["capture_seconds"] + restored["exact_page_comparison_seconds"],
            "fresh_restore_including_decode_and_translation_seconds": restored["setup_or_restore_seconds"],
            "snapshot_bytes": full["snapshot_bytes"]}
        for key, measurement in sample.items():
            if measurement > BUDGETS[key]:
                raise SystemExit(f"feasibility budget exceeded: {key} = {measurement}")
        samples.append(sample)
    summary = {key: {"median": statistics.median(s[key] for s in samples), "maximum": max(s[key] for s in samples)} for key in samples[0]}
    report = {"candidate": "wasmi-0.46.0 + continuation extension + canonical arithmetic transform",
        "status": "local_checks_passed_cross_host_and_miri_evidence_required", "budgets": BUDGETS,
        "fixtures": results, "samples": samples, "measurements": summary}
    (output / "qualification.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(summary, indent=2))

if __name__ == "__main__":
    main()
