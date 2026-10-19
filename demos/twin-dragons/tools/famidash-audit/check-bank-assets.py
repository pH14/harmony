# SPDX-License-Identifier: AGPL-3.0-or-later
"""Require banked audio assets to retain bytes, CPU addresses and ROM placement."""

import argparse
import hashlib
import json
from pathlib import Path
import re


def segments(debug):
    result = {}
    for line in debug.splitlines():
        if line.startswith("seg\t"):
            fields = dict(re.findall(r'(\w+)=("[^"]*"|[^,\t]+)', line))
            if "ooffs" in fields:
                result[fields["name"].strip('"')] = {
                    key: int(fields[key], 0) for key in ("start", "size", "ooffs")}
    return result


def compare(original_rom, original_debug, repaired_rom, repaired_debug, names):
    original, repaired = segments(original_debug), segments(repaired_debug)
    checked = []
    for name in names:
        assert name in original and name in repaired, ("missing asset segment", name)
        before, after = original[name], repaired[name]
        assert before == after, ("asset placement changed", name, before, after)
        offset, size = before["ooffs"], before["size"]
        old, new = original_rom[offset:offset + size], repaired_rom[offset:offset + size]
        assert len(old) == len(new) == size and old == new, ("asset bytes changed", name)
        checked.append({"segment": name, "cpu_address": before["start"], "rom_offset": offset,
                        "bytes": size, "sha256": hashlib.sha256(old).hexdigest()})
    return checked


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("original", "repaired", "output"):
        parser.add_argument(name, type=Path)
    parser.add_argument("--segment", action="append", dest="segments")
    args = parser.parse_args()
    names = args.segments or ["SFX_BANK", "PCM_BANK", "PCM_BANK_B"]
    cases = []
    for build in sorted(p for p in args.repaired.iterdir() if p.is_dir()):
        baseline = args.original / build.name
        old, new = (baseline / "famidash.nes").read_bytes(), (build / "famidash.nes").read_bytes()
        checked = compare(old, (baseline / "famidash.dbg").read_text(),
                          new, (build / "famidash.dbg").read_text(), names)
        cases.append({"build": build.name, "original_rom_sha256": hashlib.sha256(old).hexdigest(),
                      "repaired_rom_sha256": hashlib.sha256(new).hexdigest(), "segments": checked})
    assert cases, "no builds found"
    result = {"status": "verified", "cases": cases,
              "limitation": "Verifies selected compiled assets and placement, not audio playback or mapper timing."}
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"status": "verified", "builds": len(cases), "segments_per_build": len(names)}))


if __name__ == "__main__":
    main()
