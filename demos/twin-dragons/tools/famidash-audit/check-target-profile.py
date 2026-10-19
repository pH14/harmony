# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check a ROM slot's search alias against the pinned installed release catalog."""

import argparse
import hashlib
import json
from pathlib import Path
import re


def normalized(name):
    return re.sub(r"[^a-z0-9]", "", name.lower())


def check(index, reference, side, level):
    if reference.get("source_revision") != "0df680d0e02f0ccb438f1224d162a0ab073b7204" or len(reference["levels"]) != 109:
        raise ValueError("reference must contain the full pinned 109-level installed release catalog")
    layout = next((entry for entry in index["levels"]
                   if entry["side"] == side and entry["level"] == level), None)
    if layout is None:
        raise ValueError(f"source slot {side}:{level} is absent")
    target_side = layout.get("release_target_side", side)
    target_level = layout.get("release_target_level", level)
    target = next((entry for entry in reference["levels"]
                   if entry["side"] == target_side and entry["level"] == target_level), None)
    if target is None or normalized(target["name"]) != normalized(layout["name"]):
        matches = [f"{entry['side']}:{entry['level']}" for entry in reference["levels"]
                   if normalized(entry["name"]) == normalized(layout["name"])]
        actual = target["name"] if target else "missing profile"
        raise ValueError(f"{side}:{level} is {layout['name']}, but {target_side}:{target_level} selects {actual}; "
                         f"matching release slots: {', '.join(matches) or 'none'}")
    game = ("famidash-community" if target_level >= 22 else "famidash") if target_side == "main" else f"famidash-{target_side}"
    return {"status": "release-profile-identity-verified", "source_slot": [side, level],
            "release_target": [target_side, target_level], "name": layout["name"], "game": game,
            "limitation": "Checks catalog identity, not changed level geometry, runtime behavior or route intent."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("index", type=Path)
    parser.add_argument("release_index", type=Path)
    parser.add_argument("side", choices=["main", "b", "c", "d", "e"])
    parser.add_argument("level", type=int)
    args = parser.parse_args()
    try:
        result = check(json.loads(args.index.read_text()), json.loads(args.release_index.read_text()),
                       args.side, args.level)
    except ValueError as error:
        parser.error(str(error))
    result["index_sha256"] = hashlib.sha256(args.index.read_bytes()).hexdigest()
    result["release_index_sha256"] = hashlib.sha256(args.release_index.read_bytes()).hexdigest()
    print(json.dumps(result))
