# SPDX-License-Identifier: AGPL-3.0-or-later
"""Replay the combined-ROM negative regressions and actual clearing controls."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]


def run(roms, runtime):
    entries = json.loads((ROOT / "regressions.json").read_text())["cases"]
    tool = REPO / "demos/twin-dragons/tools/famidash-audit/dissonance-replay.mjs"
    for entry in entries:
        case = REPO / entry["case"]
        if hashlib.sha256(case.read_bytes()).hexdigest() != entry["sha256"]:
            raise ValueError(f"case checksum mismatch: {case}")
        if entry["kind"] not in ["strict-negative", "clearing-control"]:
            continue
        args = ["node", str(tool), str(case), str(roms / entry["rom"]), str(runtime)]
        if entry["kind"] == "strict-negative":
            args += ["--fixed", "--intro-frames", "40"]
        subprocess.run(args, check=True)
    contacts = {e["kind"]: e for e in entries if e["kind"].startswith("contact-")}
    subprocess.run(["node", str(ROOT / "contact.mjs"),
                    str(REPO / contacts["contact-original"]["case"]),
                    str(REPO / contacts["contact-fixed"]["case"]), str(roms / "famidash.nes"), str(runtime)], check=True)
    subprocess.run(["node", str(REPO / "workloads/nes/src/famidash/repros/fofii/replay.mjs"),
                    str(roms / "famidash-b-sides.nes"), str(runtime), "fixed"], check=True)
    print("11 strict negatives, floor contact/reset, Fofii pit/control, and 3 full clearing traces passed.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("roms", type=Path)
    parser.add_argument("runtime", type=Path)
    args = parser.parse_args()
    run(args.roms.resolve(), args.runtime.resolve())
