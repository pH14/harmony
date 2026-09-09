#!/usr/bin/env python3
"""Freeze a diagnostic executable with before/after source attestation."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
for name in ["root", "out", "target"]:
    parser.add_argument(name, type=Path)
parser.add_argument("binary", choices=["metroid-boss-probe", "mm2-metal-export", "metroid-kinematics-probe"])
parser.add_argument("--source-commit", required=True)
parser.add_argument("--features", default="")
args = parser.parse_args()
spec = importlib.util.spec_from_file_location("evaluation", args.root / "benchmarks/search/eval.py")
evaluation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluation)
before = evaluation.source_identity(args.root)
args.out.mkdir(parents=True, exist_ok=False)
env = {**os.environ, "HARMONY_SEARCH_SOURCE_SHA256": before["source_tree_sha256"]}
with (args.out / "build.log").open("wb") as log:
    subprocess.run(["cargo", "build", "--release", "--locked", "--manifest-path",
                    str(args.root / "workloads/nes/Cargo.toml"), "--bin", args.binary,
                    "--target-dir", str(args.target), "-j", "4",
                    *(["--features", args.features] if args.features else [])],
                   env=env, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=1200)
assert before == evaluation.source_identity(args.root), "source changed during compilation"
shutil.copy2(args.target / "release" / args.binary, args.out / args.binary)
metadata = {"format": "harmony-diagnostic-build-v1", **before,
            "source_commit": args.source_commit, "binary": args.binary,
            "binary_sha256": evaluation.digest(args.out / args.binary),
            "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
            "cargo": subprocess.check_output(["cargo", "-V"], text=True).strip(),
            "profile": "release", "locked": True, "features": args.features, "target_cache": str(args.target)}
evaluation.write_json(args.out / "build-info.json", metadata)
print(json.dumps(metadata), flush=True)
