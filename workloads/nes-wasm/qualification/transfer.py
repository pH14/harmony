#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Replay actual exported workload checkpoints from every host producer."""
import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser()
    for name in ("package", "states", "output", "runner"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    producers = sorted(args.states.iterdir())
    if len(producers) != 4:
        raise SystemExit("all four producer artifacts are required")
    oracle = json.loads((producers[0] / "nova.json").read_text())
    for producer in producers:
        trace = json.loads((producer / "nova.json").read_text())
        if trace["initial_state"] != oracle["initial_state"] or trace["trace"] != oracle["trace"]:
            raise SystemExit(f"complete producer state mismatch: {producer}")
        output = args.output / f"{producer.name}.json"
        subprocess.run([str(args.runner), "restore", str(args.package), str(producer / "nova.json-3.snapshot"), str(output), "none"], check=True)
        restored = json.loads(output.read_text())
        future = json.loads((producer / "future.json").read_text())
        if restored["initial_state"] != trace["trace"][-1]["state"] or restored["trace"] != future["trace"]:
            raise SystemExit(f"complete future state mismatch: {producer}")
    (args.output / "summary.json").write_text(json.dumps({"producers": [p.name for p in producers], "all_complete_states_and_futures_equal": True}, indent=2))


if __name__ == "__main__":
    main()
