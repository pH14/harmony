#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Prepare pinned upstream scalar tests inside the fixed-memory profile."""
import argparse
import json
from pathlib import Path
import re
import subprocess

REVISION = "957c932e7158c5a6891be68ca424aaa0aa505f97"
SUITES = ("i32", "i64", "f32", "f64", "f32_bitwise", "f64_bitwise", "f32_cmp", "f64_cmp", "conversions")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--spec", type=Path, required=True)
    parser.add_argument("--wast2json", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    revision = subprocess.check_output(["git", "-C", str(args.spec), "rev-parse", "HEAD"], text=True).strip()
    if revision != REVISION:
        raise SystemExit("upstream conformance revision mismatch")
    args.output.mkdir(parents=True, exist_ok=True)
    for suite in SUITES:
        text = (args.spec / "test/core" / f"{suite}.wast").read_text()
        text = re.sub(r"\(module(?=\s*\()", "(module (memory 1 1)", text)
        source = args.output / f"{suite}.wast"
        source.write_text(text)
        subprocess.run([str(args.wast2json), str(source), "-o", str(args.output / f"{suite}.json")], check=True)
    (args.output / "pin.json").write_text(json.dumps({"revision": REVISION, "suites": SUITES, "adaptation": "add one unused fixed 64 KiB memory; preserve operators and assertions"}, indent=2))


if __name__ == "__main__":
    main()
