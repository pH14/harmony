#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Convert one FCEUX FM2 controller log to a Dissonance SMB prefix."""

import argparse
import base64
import gzip
import hashlib
import json
from pathlib import Path


BUTTON_BITS = (0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01)


def read_movie(path: Path) -> tuple[str, list[str]]:
    opener = gzip.open if path.read_bytes()[:2] == b"\x1f\x8b" else open
    checksum = ""
    frames: list[str] = []
    with opener(path, "rt", encoding="utf-8") as movie:
        for line in movie:
            if line.startswith("romChecksum base64:"):
                checksum = line.split(":", 1)[1].strip()
            elif line.startswith("|"):
                fields = line.rstrip("\n").split("|")
                if len(fields) < 4 or len(fields[2]) != 8:
                    raise ValueError("FM2 controller row is malformed")
                frames.append(fields[2])
    if not checksum or not frames:
        raise ValueError("FM2 lacks a ROM checksum or controller frames")
    return checksum, frames


def mask_of(controller: str) -> int:
    return sum(bit for bit, marker in zip(BUTTON_BITS, controller, strict=True) if marker != ".")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("movie", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--skip-frames", type=int, required=True)
    parser.add_argument("--prepend-idle-frames", type=int, default=0)
    parser.add_argument("--max-frames", type=int)
    args = parser.parse_args()

    checksum, frames = read_movie(args.movie)
    rom = args.rom.read_bytes()
    if len(rom) < 16 or rom[:4] != b"NES\x1a":
        raise ValueError("ROM is not an iNES image")
    actual = base64.b64encode(hashlib.md5(rom[16:]).digest()).decode("ascii")
    if actual != checksum:
        raise ValueError(f"FM2 ROM checksum {checksum} does not match {actual}")
    if not 0 <= args.skip_frames < len(frames):
        raise ValueError("skip frame count is outside the movie")
    if args.prepend_idle_frames < 0:
        raise ValueError("prepend idle frame count must be non-negative")
    if args.max_frames is not None and args.max_frames <= 0:
        raise ValueError("max frame count must be positive")

    actions: list[dict[str, int]] = []
    movie_frames = frames[args.skip_frames :]
    if args.max_frames is not None:
        movie_frames = movie_frames[: args.max_frames]
    controllers = ["........"] * args.prepend_idle_frames + movie_frames
    for controller in controllers:
        buttons = mask_of(controller)
        if actions and actions[-1]["buttons"] == buttons and actions[-1]["hold_frames"] < 120:
            actions[-1]["hold_frames"] += 1
        else:
            actions.append({"buttons": buttons, "hold_frames": 1})
    args.output.write_text(json.dumps({"actions": actions}, indent=2) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "movie_sha256": hashlib.sha256(args.movie.read_bytes()).hexdigest(),
                "rom_md5_base64": actual,
                "source_frames": len(frames),
                "skip_frames": args.skip_frames,
                "prepend_idle_frames": args.prepend_idle_frames,
                "max_frames": args.max_frames,
                "output_frames": len(controllers),
                "actions": len(actions),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
