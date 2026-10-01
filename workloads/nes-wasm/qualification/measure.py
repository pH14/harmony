#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run the production measurement in its own process and record peak RSS."""
import argparse
import json
from pathlib import Path
import resource
import subprocess
import sys

parser = argparse.ArgumentParser()
for name in ("runner", "package", "rom", "output"):
    parser.add_argument(f"--{name}", type=Path, required=True)
args = parser.parse_args()
subprocess.run([str(args.runner), str(args.package), str(args.rom), str(args.output)], check=True)
report = json.loads(args.output.read_text())
report["process_peak_rss_bytes"] = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss * (1 if sys.platform == "darwin" else 1024)
report["fresh_restore_scope"] = "postcard decoding, artifact validation, one eager compilation, instance construction and captured-state installation; admission/package loading excluded"
report["cycle_scope"] = "branch, 120 frames, exact page capture, export/serialization, fresh restore, full-state comparison and retained-payload measurement"
args.output.write_text(json.dumps(report, indent=2) + "\n")
