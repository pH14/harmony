#!/usr/bin/env -S uv run --script
# SPDX-License-Identifier: AGPL-3.0-or-later
# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow"]
# ///
"""Replay Famidash victory tapes in any libretro NES core and check the game's own completion state.

`replay` boots each tape's ROM, repeats the workload's level-start procedure,
feeds the tape one frame at a time, and reports the first frame where the game
state byte at $0498 reads 3 (level complete). `diverge` runs two cores in
lockstep on one tape and reports the first frame where their gameplay parts
and the RAM bytes that differ there.
"""

from __future__ import annotations

import argparse
import json
import multiprocessing
import re
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

from libretro_host import Core

A_BUTTON = 0x01
RIGHT_BUTTON = 0x80
MAX_HOLD_FRAMES = 120

GAME_STATE = 0x0498
LEVEL = 0x047B
CUBE_DATA = 0x7B
PLAYER_X = 0x043D
GAME_STATE_PLAYING = 2
GAME_STATE_COMPLETE = 3
DEATH_CHECK_SCREEN_X = 0x20
LEVEL_START_FRAMES = 40
MENU_SETTLE_FRAMES = 20
BANNER_FRAMES = 300

GAMEPLAY_FIELDS = {
    "game_state": (GAME_STATE, 1),
    "level": (LEVEL, 1),
    "cube_data": (CUBE_DATA, 2),
    "gamemode": (0x7A, 1),
    "player_x": (PLAYER_X, 2),
    "player_y": (0x0441, 2),
    "velocity_y": (0x0449, 2),
    "scroll_x": (0x04A2, 4),
    "scroll_y": (0x04A6, 2),
}

ROMS = {
    "main": "famidash.nes",
    "b": "famidash-b-sides.nes",
    "c": "famidash-c-sides.nes",
    "d": "famidash-d-sides.nes",
}

QUICKNES_OPTIONS = {
    b"quicknes_aspect_ratio_par": b"PAR",
    b"quicknes_use_overscan_h": b"enabled",
    b"quicknes_use_overscan_v": b"disabled",
    b"quicknes_palette": b"default",
    b"quicknes_no_sprite_limit": b"disabled",
    b"quicknes_audio_samplerate": b"48000",
    b"quicknes_audio_nonlinear": b"nonlinear",
    b"quicknes_audio_eq": b"default",
    b"quicknes_turbo_enable": b"none",
    b"quicknes_turbo_pulse_width": b"3",
    b"quicknes_up_down_allowed": b"disabled",
}

QUICKNES_POWER_ON_RAM = {0x08: 0xF7, 0x09: 0xEF, 0x0A: 0xDF, 0x0F: 0xBF}

GENESIS = re.compile(r"genesis=famidash-(main|b|c|d)-level-(\d+)-to-(\d+)-v1")


@dataclass(frozen=True)
class Stage:
    side: str
    level: int
    goal: int

    @property
    def community_menu(self) -> bool:
        return self.side != "main" or self.level >= 22


@dataclass
class Run:
    name: str
    stage: Stage
    rom: Path
    tape: list[tuple[int, int]]

    @property
    def tape_frames(self) -> int:
        return sum(hold for _, hold in self.tape)


def load_run(run_dir: Path, rom_dir: Path) -> Run:
    identity = json.loads((run_dir / "identity.json").read_text())
    match = GENESIS.search(identity["policies"]["emulator_backend"])
    if not match:
        raise ValueError(f"{run_dir.name}: no Famidash genesis in identity.json")
    side, level, goal = match.group(1), int(match.group(2)), int(match.group(3))
    if level != goal:
        raise ValueError(f"{run_dir.name}: multi-level genesis is not supported")
    tape = [
        (action["buttons"], min(max(action["hold_frames"], 1), MAX_HOLD_FRAMES))
        for action in json.loads((run_dir / "victory-input.json").read_text())["actions"]
    ]
    return Run(run_dir.name, Stage(side, level, goal), rom_dir / ROMS[side], tape)


def idle(frames: int) -> list[tuple[int, int]]:
    chords = []
    while frames > 0:
        hold = min(frames, MAX_HOLD_FRAMES)
        chords.append((0, hold))
        frames -= hold
    return chords


def level_select_tape(stage: Stage) -> list[tuple[int, int]]:
    tape = idle(22) + [(A_BUTTON, 1)] + idle(936)
    if stage.community_menu:
        tape += [(RIGHT_BUTTON, 1)] + idle(MENU_SETTLE_FRAMES)
    return tape + [(A_BUTTON, 1)] + idle(90)


def start_level_tape() -> list[tuple[int, int]]:
    return [(A_BUTTON, 1)] + idle(LEVEL_START_FRAMES)


def frames_of(tape: list[tuple[int, int]]):
    for buttons, hold in tape:
        for _ in range(hold):
            yield buttons


def dead(ram: bytes) -> bool:
    return (
        ram[GAME_STATE] == GAME_STATE_PLAYING
        and ram[PLAYER_X + 1] > DEATH_CHECK_SCREEN_X
        and bool((ram[CUBE_DATA] | ram[CUBE_DATA + 1]) & 1)
    )


def core_options(core_path: Path) -> dict[bytes, bytes]:
    return QUICKNES_OPTIONS if "quicknes" in core_path.name.lower() else {}


def open_core(core_path: Path, rom: Path, video: bool, quicknes_ram: bool = False) -> Core:
    core = Core(core_path, rom, options=core_options(core_path), video=video)
    if quicknes_ram:
        for address in range(0x800):
            core.poke(address, QUICKNES_POWER_ON_RAM.get(address, 0xFF))
    return core


def setup(core: Core, stage: Stage, boot_frames: int) -> bytes:
    """Repeat the workload's level start: menu inputs, the level byte write, then the start press."""
    ram = core.ram()
    for buttons in frames_of(idle(boot_frames) + level_select_tape(stage)):
        ram = core.run_frame(buttons)
    if ram[LEVEL] != stage.level:
        core.poke(LEVEL, stage.level)
    for buttons in frames_of(start_level_tape()):
        ram = core.run_frame(buttons)
    if ram[GAME_STATE] != GAME_STATE_PLAYING or ram[LEVEL] != stage.level:
        raise RuntimeError(
            f"setup did not reach level {stage.level} gameplay: state={ram[GAME_STATE]} level={ram[LEVEL]}"
        )
    return ram


@dataclass
class ReplayResult:
    run: str
    side: str
    level: int
    recorded_frame: int
    complete_frame: int | None
    death_frame: int | None
    matches_recorded_frame: bool
    screenshot: str | None
    error: str | None = None


def replay_one(
    core_path: Path, boot_frames: int, quicknes_ram: bool, run: Run, screenshot_dir: Path | None
) -> ReplayResult:
    try:
        core = open_core(core_path, run.rom, video=screenshot_dir is not None, quicknes_ram=quicknes_ram)
        setup(core, run.stage, boot_frames)
        complete_frame = death_frame = None
        frame = 0
        for buttons in frames_of(run.tape + idle(600)):
            ram = core.run_frame(buttons)
            frame += 1
            if death_frame is None and dead(ram):
                death_frame = frame
            if ram[GAME_STATE] == GAME_STATE_COMPLETE:
                complete_frame = frame
                break
        screenshot = None
        if screenshot_dir is not None and complete_frame is not None and core.frame is not None:
            path = screenshot_dir / f"{run.name}.png"
            core.frame.save_png(path)
            screenshot = str(path)
            for _ in range(BANNER_FRAMES):
                core.run_frame(0)
            core.frame.save_png(screenshot_dir / f"{run.name}-banner.png")
        matches_recorded_frame = complete_frame == run.tape_frames and (
            death_frame is None or death_frame > complete_frame
        )
        return ReplayResult(
            run.name,
            run.stage.side,
            run.stage.level,
            run.tape_frames,
            complete_frame,
            death_frame,
            matches_recorded_frame,
            screenshot,
        )
    except Exception as error:
        return ReplayResult(
            run.name, run.stage.side, run.stage.level, run.tape_frames, None, None, False, None, str(error)
        )


def _replay_task(task):
    return replay_one(*task)


def run_dirs(paths: list[Path]) -> list[Path]:
    found = []
    for path in paths:
        if (path / "victory-input.json").is_file():
            found.append(path)
        else:
            found.extend(sorted(p.parent for p in path.glob("*/victory-input.json")))
    return found


def command_replay(args) -> int:
    runs = [load_run(path, args.rom_dir) for path in run_dirs(args.runs)]
    screenshot_dir = args.screenshots
    if screenshot_dir is not None:
        screenshot_dir.mkdir(parents=True, exist_ok=True)
    tasks = [(args.core, args.boot_frames, args.quicknes_power_on_ram, run, screenshot_dir) for run in runs]
    context = multiprocessing.get_context("spawn")
    with context.Pool(args.jobs, maxtasksperchild=1) as pool:
        results = pool.map(_replay_task, tasks, chunksize=1)
    out = open(args.output, "w") if args.output else None
    for result in results:
        line = json.dumps(asdict(result))
        if out:
            out.write(line + "\n")
        if not args.quiet or not result.matches_recorded_frame:
            print(line)
    if out:
        out.close()
    complete = sum(r.complete_frame is not None for r in results)
    same = sum(r.matches_recorded_frame for r in results)
    print(
        f"{args.core.name}: {len(results)} tapes, {complete} reach $0498=3, "
        f"{same} at the tape's recorded victory frame",
        file=sys.stderr,
    )
    return 0 if same == len(results) else 1


STACK = range(0x0100, 0x0200)
LASTING_FRAMES = 8


def differing(a: bytes, b: bytes) -> list[int]:
    return [x for x in range(len(a)) if a[x] != b[x] and x not in STACK]


def hex_addresses(addresses) -> list[str]:
    return [f"${x:04X}" for x in addresses]


def field_values(ram: bytes) -> dict[str, int]:
    return {
        name: int.from_bytes(ram[address : address + size], "little")
        for name, (address, size) in GAMEPLAY_FIELDS.items()
    }


def scroll_x(ram: bytes) -> int:
    return field_values(ram)["scroll_x"]


def frame_offset(a: list[bytes], b: list[bytes], start: int) -> int | None:
    """Return s when core B's horizontal scroll at frame i equals core A's at frame i + s from start on.

    Famidash scrolls at a fixed speed per gameplay frame, so a constant offset
    means one core ran a gameplay frame that the other skipped as a lag frame.
    """
    window = range(start, min(start + LASTING_FRAMES, len(a) - 2, len(b) - 2))
    for shift in (1, -1, 2, -2):
        if all(0 <= i + shift < len(a) and scroll_x(b[i]) == scroll_x(a[i + shift]) for i in window):
            return shift
    return None


def gameplay_divergence(a: list[bytes], b: list[bytes], end: dict) -> int | None:
    """Return the first frame of a gameplay difference that persists.

    A frame starts a divergence when its gameplay fields differ and they differ
    on at least LASTING_FRAMES of the next 2 * LASTING_FRAMES frames. Brief
    one-pixel scroll differences that resolve are ignored. When the cores end
    at different frames or in different ways and no such frame exists, the
    first differing frame before the earlier end is used.
    """
    flags = [field_values(x) != field_values(y) for x, y in zip(a, b)]
    for frame in range(1, len(flags)):
        if flags[frame] and sum(flags[frame : frame + 2 * LASTING_FRAMES]) >= LASTING_FRAMES:
            return frame
    if end.get("a") == end.get("b"):
        return None
    first_end = min(frame for _, frame in end.values()) if end else len(flags) - 1
    for frame in range(max(1, first_end - 2 * LASTING_FRAMES), first_end + 1):
        if flags[frame]:
            return frame
    return first_end


def command_diverge(args) -> int:
    """Run two cores in lockstep and report where their RAM and gameplay first part.

    Interrupts land on different instructions in different emulators, so the
    stack page is excluded from every RAM comparison.
    """
    run = load_run(args.run, args.rom_dir)
    left = open_core(args.core_a, run.rom, video=False, quicknes_ram=args.quicknes_power_on_ram)
    right = open_core(args.core_b, run.rom, video=False, quicknes_ram=args.quicknes_power_on_ram)
    for _ in range(args.boot_frames_b):
        right.run_frame(0)
    report: dict = {"run": run.name, "recorded_frame": run.tape_frames}
    setup_frame = 0
    first_setup_difference = None

    def lockstep(tape: list[tuple[int, int]]) -> None:
        nonlocal setup_frame, first_setup_difference
        for buttons in frames_of(tape):
            setup_frame += 1
            a, b = left.run_frame(buttons), right.run_frame(buttons)
            if first_setup_difference is None and differing(a, b):
                first_setup_difference = {"frame": setup_frame, "addresses": hex_addresses(differing(a, b)[:16])}

    lockstep(level_select_tape(run.stage))
    for core in (left, right):
        if core.ram()[LEVEL] != run.stage.level:
            core.poke(LEVEL, run.stage.level)
    lockstep(start_level_tape())
    report["first_setup_difference"] = first_setup_difference
    genesis_a, genesis_b = left.ram(), right.ram()
    genesis_noise = set(differing(genesis_a, genesis_b))
    report["genesis_differing_addresses"] = hex_addresses(sorted(genesis_noise))
    history_a, history_b = [genesis_a], [genesis_b]
    end = {}
    for frame, buttons in enumerate(frames_of(run.tape + idle(600)), 1):
        a, b = left.run_frame(buttons), right.run_frame(buttons)
        history_a.append(a)
        history_b.append(b)
        for name, ram in (("a", a), ("b", b)):
            if name not in end and (ram[GAME_STATE] == GAME_STATE_COMPLETE or dead(ram)):
                end[name] = ("complete" if ram[GAME_STATE] == GAME_STATE_COMPLETE else "death", frame)
        if len(end) == 2:
            break
    divergence = gameplay_divergence(history_a, history_b, end)
    report["end_a"] = end.get("a")
    report["end_b"] = end.get("b")
    if divergence is not None:
        a, b = history_a[divergence], history_b[divergence]
        fa, fb = field_values(a), field_values(b)
        report["gameplay_divergence"] = {
            "frame": divergence,
            "fields": {name: [fa[name], fb[name]] for name in fa if fa[name] != fb[name]},
            "new_differing_addresses": hex_addresses([x for x in differing(a, b) if x not in genesis_noise][:16]),
            "core_b_frames_ahead": frame_offset(history_a, history_b, divergence),
        }
    print(json.dumps(report))
    return 0


BOOT_FRAMES_HELP = (
    "idle frames to run before the level-start inputs, so the core's first "
    "frame lines up with QuickNES's first frame (fceumm needs 1, Mesen 0)"
)

POWER_ON_RAM_HELP = (
    "overwrite system RAM after load with QuickNES's power-on contents, so cores "
    "with other power-on fills start from the same RAM"
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    replay = sub.add_parser("replay", help="replay victory tapes in one core")
    replay.add_argument("--core", type=Path, required=True, help="libretro core library")
    replay.add_argument("--rom-dir", type=Path, required=True, help="directory holding the four Famidash 1.2.8 ROMs")
    replay.add_argument("--boot-frames", type=int, default=0, help=BOOT_FRAMES_HELP)
    replay.add_argument("--quicknes-power-on-ram", action="store_true", help=POWER_ON_RAM_HELP)
    replay.add_argument(
        "--screenshots",
        type=Path,
        help="write a PNG of each completion frame here, plus one of the level-complete banner 300 frames later",
    )
    replay.add_argument("--output", type=Path, help="write one JSON line per tape here")
    replay.add_argument("--jobs", type=int, default=multiprocessing.cpu_count())
    replay.add_argument("--quiet", action="store_true", help="print only mismatches")
    replay.add_argument("runs", type=Path, nargs="+", help="run directories, or directories of run directories")
    replay.set_defaults(handler=command_replay)
    diverge = sub.add_parser("diverge", help="run two cores in lockstep on one tape and compare RAM")
    diverge.add_argument("--core-a", type=Path, required=True)
    diverge.add_argument("--core-b", type=Path, required=True)
    diverge.add_argument("--boot-frames-b", type=int, default=0, help=BOOT_FRAMES_HELP)
    diverge.add_argument("--quicknes-power-on-ram", action="store_true", help=POWER_ON_RAM_HELP)
    diverge.add_argument("--rom-dir", type=Path, required=True)
    diverge.add_argument("run", type=Path, help="one run directory")
    diverge.set_defaults(handler=command_diverge)
    args = parser.parse_args()
    return args.handler(args)


if __name__ == "__main__":
    sys.exit(main())
