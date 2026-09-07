#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Convert a canonical SMB BizHawk BK2 input log to a Dissonance prefix."""

import argparse
import hashlib
import json
import zipfile
from pathlib import Path


EXPECTED_GAME_SHA1 = "EA343F4E445A9050D4B4FBAC2C77D0693B1D0922"
EXPECTED_CARTRIDGE_MD5 = "8e3630186e35d477231bf8fd50e54cdd"
BUTTON_BITS = (0x10, 0x20, 0x40, 0x80, 0x08, 0x04, 0x02, 0x01)


def read_movie(path: Path) -> tuple[str, list[str]]:
    with zipfile.ZipFile(path) as movie:
        header = movie.read("Header.txt").decode("utf-8-sig")
        input_log = movie.read("Input Log.txt").decode("utf-8-sig")
    sha1 = ""
    for line in header.splitlines():
        if line.startswith("SHA1 "):
            sha1 = line.split(maxsplit=1)[1]
    lines = [line for line in input_log.splitlines() if line]
    if not lines or lines[0] != (
        "LogKey:#Power|Reset|#P1 Up|P1 Down|P1 Left|P1 Right|P1 Start|"
        "P1 Select|P1 B|P1 A|#P2 Up|P2 Down|P2 Left|P2 Right|P2 Start|"
        "P2 Select|P2 B|P2 A|"
    ):
        raise ValueError("BK2 input schema is not the expected NES controller log")
    controllers = []
    for line in lines[1:]:
        fields = line.split("|")
        if len(fields) != 5 or len(fields[2]) != 8:
            raise ValueError("BK2 controller row is malformed")
        controllers.append(fields[2])
    if not sha1 or not controllers:
        raise ValueError("BK2 lacks a ROM SHA1 or controller frames")
    return sha1, controllers


def mask_of(controller: str) -> int:
    return sum(bit for bit, marker in zip(BUTTON_BITS, controller, strict=True) if marker != ".")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("movie", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--skip-frames", type=int, default=0)
    parser.add_argument("--prepend-idle-frames", type=int, default=0)
    parser.add_argument("--max-frames", type=int)
    args = parser.parse_args()

    game_sha1, frames = read_movie(args.movie)
    if game_sha1 != EXPECTED_GAME_SHA1:
        raise ValueError(f"BK2 game SHA1 is {game_sha1}, expected {EXPECTED_GAME_SHA1}")
    rom = args.rom.read_bytes()
    if len(rom) < 16 or rom[:4] != b"NES\x1a":
        raise ValueError("ROM is not an iNES image")
    cartridge_md5 = hashlib.md5(rom[16:]).hexdigest()
    if cartridge_md5 != EXPECTED_CARTRIDGE_MD5:
        raise ValueError(
            f"ROM cartridge MD5 is {cartridge_md5}, expected {EXPECTED_CARTRIDGE_MD5}"
        )
    if not 0 <= args.skip_frames < len(frames):
        raise ValueError("skip frame count is outside the movie")
    if args.prepend_idle_frames < 0:
        raise ValueError("prepend idle frame count must be non-negative")
    if args.max_frames is not None and args.max_frames <= 0:
        raise ValueError("max frame count must be positive")

    movie_frames = frames[args.skip_frames :]
    if args.max_frames is not None:
        movie_frames = movie_frames[: args.max_frames]
    controllers = ["........"] * args.prepend_idle_frames + movie_frames
    actions: list[dict[str, int]] = []
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
                "game_sha1": game_sha1,
                "rom_cartridge_md5": cartridge_md5,
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
