#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Create an evaluator prefix using SMB's built-in A+Start continuation path."""

import argparse
import json
from pathlib import Path


CONTINUE_WORLD = 0x07FD


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("world", type=int, choices=range(1, 9))
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    prefix = {
        "actions": [
            {"buttons": 0, "hold_frames": 120},
            {"buttons": 9, "hold_frames": 1},
            {"buttons": 0, "hold_frames": 120},
            {"buttons": 0, "hold_frames": 120},
        ],
        "pokes": [{"address": CONTINUE_WORLD, "value": args.world - 1}],
        "poke_after_frame": 120,
        "capture_after_frame": 360,
    }
    args.output.write_text(json.dumps(prefix, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"world": args.world, "prefix": prefix}, indent=2))


if __name__ == "__main__":
    main()
