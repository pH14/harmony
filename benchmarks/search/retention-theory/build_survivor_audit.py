#!/usr/bin/env python3
"""Attest two frozen native evaluator builds for the complete-survivor audit."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import eval as evaluator


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--source", type=Path, required=True)
    p.add_argument("--commit", required=True)
    p.add_argument("--revision", choices=["001", "002"], default="001")
    a = p.parse_args()
    root, source = a.experiment.resolve(), a.source.resolve()
    before = evaluator.source_identity(source)
    environment = {**os.environ, "HARMONY_SEARCH_SOURCE_SHA256": before["source_tree_sha256"]}
    for label, features in [(f"survivor-default-{a.revision}", ""),
                            (f"survivor-motion-{a.revision}", "metroid-motion-context,metroid-complete-retention-audit")]:
        out = root / "builds" / label
        out.mkdir()
        command = ["cargo", "build", "--release", "--locked", "--manifest-path",
                   str(source / "workloads/nes/Cargo.toml"), "--bin", "nes-eval",
                   "--target-dir", str(root / "target"), "-j", "4"]
        if features:
            command += ["--features", features]
        with (out / "build.log").open("w") as log:
            subprocess.run(command, env=environment, cwd=source, stdout=log,
                           stderr=subprocess.STDOUT, check=True, timeout=600)
        assert evaluator.source_identity(source) == before
        shutil.copy2(root / "target/release/nes-eval", out / "nes-eval")
        metadata = {"format": "harmony-search-build-v1", **before,
                    "source_commit": a.commit, "features": features,
                    "binary_sha256": evaluator.digest(out / "nes-eval"),
                    "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
                    "cargo": subprocess.check_output(["cargo", "-V"], text=True).strip(),
                    "profile": "release", "locked": True,
                    "rustflags": environment.get("RUSTFLAGS", ""), "target_cache": str(root / "target")}
        (out / "build-info.json").write_text(json.dumps(metadata, indent=2) + "\n")
        print(json.dumps({"label": label, **metadata}), flush=True)


if __name__ == "__main__":
    main()
