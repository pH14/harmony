#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Create an evaluator prefix that lets SMB initialize a selected main level."""

import argparse
import json
from pathlib import Path


WORLD_NUMBER = 0x075F
LEVEL_NUMBER = 0x075C
AREA_NUMBER = 0x0760


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("world", type=int, choices=range(1, 9))
    parser.add_argument("level", type=int, choices=range(1, 5))
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    world = args.world - 1
    level = args.level - 1
    prefix = {
        "actions": [
            {"buttons": 0, "hold_frames": 120},
            {"buttons": 8, "hold_frames": 1},
            {"buttons": 0, "hold_frames": 120},
            {"buttons": 0, "hold_frames": 120},
        ],
        "pokes": [
            {"address": WORLD_NUMBER, "value": world},
            {"address": LEVEL_NUMBER, "value": level},
            {"address": AREA_NUMBER, "value": level},
        ],
        "poke_after_frame": 120,
        "capture_after_frame": 360,
    }
    args.output.write_text(json.dumps(prefix, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"world": args.world, "level": args.level, "prefix": prefix}, indent=2))


if __name__ == "__main__":
    main()
