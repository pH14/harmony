# SPDX-License-Identifier: AGPL-3.0-or-later
"""Observe bank-aware collision reads through Mesen's read-only opcode hook."""

import argparse
import ctypes as C
import gzip
import hashlib
import json
from pathlib import Path
import re

from mesen import Mesen, SymbolMesen


LENGTHS = {}
for length, codes in [
    (1, "00 08 18 28 38 40 48 58 60 68 78 88 8a 98 9a a8 aa b8 ba c8 ca d8 e8 ea f8 0a 2a 4a 6a"),
    (2, "01 05 06 09 10 11 15 16 21 24 25 26 29 30 31 35 36 41 45 46 49 50 51 55 56 61 65 66 69 70 71 75 76 81 84 85 86 90 91 94 95 96 a0 a1 a2 a4 a5 a6 a9 b0 b1 b4 b5 b6 c0 c1 c4 c5 c6 c9 d0 d1 d5 d6 e0 e1 e4 e5 e6 e9 f0 f1 f5 f6"),
    (3, "0d 0e 19 1d 1e 20 2c 2d 2e 39 3d 3e 4c 4d 4e 59 5d 5e 6c 6d 6e 79 7d 7e 8c 8d 8e 99 9d ac ad ae b9 bc bd be cc cd ce d9 dd de ec ed ee f9 fd fe"),
]:
    LENGTHS.update({int(value, 16): length for value in codes.split()})


def instructions(code):
    offset = 0
    while offset < len(code):
        opcode = code[offset]
        assert opcode in LENGTHS, ("unsupported opcode in collision function", offset, opcode)
        size = LENGTHS[opcode]
        assert offset + size <= len(code), "truncated collision instruction"
        yield offset, code[offset:offset + size]
        offset += size


def load_address(code, ram, x, y):
    opcode = code[0]
    if opcode in (0xAD, 0xBD, 0xB9):
        base = code[1] | code[2] << 8
        return (base + (x if opcode == 0xBD else y if opcode == 0xB9 else 0)) & 65535
    if opcode in (0xA1, 0xB1):
        pointer = (code[1] + (x if opcode == 0xA1 else 0)) & 255
        base = ram[pointer] | ram[(pointer + 1) & 255] << 8
        return (base + (y if opcode == 0xB1 else 0)) & 65535
    return None


def run(args):
    case_bytes = args.witness.read_bytes()
    witness = json.loads(gzip.decompress(case_bytes) if args.witness.suffix == ".gz" else case_bytes)
    rom = args.rom.read_bytes()
    assert hashlib.sha256(rom).hexdigest() == witness["rom_sha256"]
    assert not rom[6] & 4, "trainer layout is unsupported"
    segments, symbols = {}, {}
    for line in args.debug.read_text().splitlines():
        fields = dict(re.findall(r'(\w+)=("[^"]*"|[^,\t]+)', line))
        if line.startswith("seg\t") and "ooffs" in fields:
            segments[int(fields["id"])] = fields
        if line.startswith("sym\t") and "val" in fields:
            symbols[fields["name"].strip('"')] = fields

    def address(name):
        return int(symbols["_" + name]["val"], 0)

    def function(name):
        s = symbols["_" + name]
        segment = segments[int(s["seg"])]
        pc, size = int(s["val"], 0), int(s["size"], 0)
        offset = int(segment["ooffs"], 0) + pc - int(segment["start"], 0)
        assert size and 0x8000 <= pc < pc + size <= 65536
        return pc, offset, size

    sub, sub_rom, sub_size = function("bg_collision_sub")
    death, death_rom, death_size = function("bg_coll_death")
    sites = {sub_rom + offset: code for offset, code in instructions(rom[sub_rom:sub_rom + sub_size])
             if code[0] in (0xAD, 0xBD, 0xB9, 0xA1, 0xB1)}
    assert sites, "no supported load instructions in collision function"
    linear = "target_layout" in witness
    layout = None
    if linear:
        layout = {"kind": "symbol-based linear coordinates", "symbols": {
            name: address(name) for name in witness["target_layout"]["symbols"]}}
    core = SymbolMesen(args.core, args.rom, layout) if linear else Mesen(args.core, args.rom)
    core.startup_frames = witness["options"].get("fceumm_startup_released_frames", 180)
    core.boot(witness["options"]["side"], witness["boot_level"], witness["options"].get("menu_delay", 0))
    ram = core.ram
    locations = {name: address(name) for name in [
        "temp_x", "temp_y", "Generic", "currplayer", "currplayer_mini", "gamemode", "wrap_mode",
        "dblocked", "scroll_x", "scroll_y", "scroll_y_subpx"]}
    locations["relative_y"] = address("currplayer_rely" if linear else "currplayer_y")
    if not linear:
        locations["temp_room"] = address("temp_room")
    assert all(0 <= value < 2044 for value in locations.values())
    frame, calls, pending, samples, states, differences, errors = 0, {}, {}, [], [], [], []
    callback_type = C.CFUNCTYPE(None, C.c_uint16, C.c_int32, *([C.c_uint8] * 6))

    def snapshot(pc, offset, stack, memory_address):
        returned = ram[0x100 + ((stack + 1) & 255)] | ram[0x100 + ((stack + 2) & 255)] << 8
        caller = calls.get((stack, returned))
        assert caller is not None, ("unresolved mapped caller", frame, pc, stack, returned)
        y = ram[locations["temp_y"]]
        y += (ram[locations["temp_y"] + 1] << 8) if linear else ram[locations["temp_room"]] * 240
        generic = locations["Generic"]
        scroll_x = int.from_bytes(bytes(ram[locations["scroll_x"]:locations["scroll_x"] + 4]), "little", signed=True)
        base_x = scroll_x + ram[generic]
        probe_offset = ((ram[locations["temp_x"]] - (base_x & 255) + 128) % 256) - 128
        return {"frame": frame, "pc": pc, "return_address": returned,
                "centre_probe": death_rom <= caller < death_rom + death_size,
                "pc_rom_offset": offset, "caller_rom_offset": caller, "address": memory_address,
                "sample_x_low": ram[locations["temp_x"]], "sample_world_x": base_x + probe_offset,
                "scroll_x_at_probe": scroll_x, "relative_probe_offset": probe_offset,
                "sample_world_y": y, "generic": list(ram[generic:generic + 4]),
                "player": ram[locations["currplayer"]], "mini": ram[locations["currplayer_mini"]],
                "mode": ram[locations["gamemode"]], "wrap_mode": ram[locations["wrap_mode"]],
                "dblocked": ram[locations["dblocked"] + ram[locations["currplayer"]]],
                "camera_at_probe": ram[locations["scroll_y"]] + ram[locations["scroll_y"] + 1] * (256 if linear else 240),
                "camera_subpixel": ram[locations["scroll_y_subpx"]],
                "relative_y_fixed": ram[locations["relative_y"]] + 256 * ram[locations["relative_y"] + 1],
                "fractional_camera_carry": (ram[locations["relative_y"]] + ram[locations["scroll_y_subpx"]]) >> 8}

    @callback_type
    def on_opcode(pc, prg, opcode, accumulator, x, y, flags, stack):
        if errors:
            return
        try:
            offset = prg + 16 if prg >= 0 else None
            if offset is not None:
                assert 16 <= offset < len(rom) and rom[offset] == opcode, ("mapped opcode mismatch", pc, offset)
            key = (pc, offset, stack)
            if key in pending:
                sample = pending.pop(key)
                sample["metatile"] = accumulator
                samples.append(sample)
            if offset is not None and opcode == 0x20:
                calls[((stack - 2) & 255, (pc + 2) & 65535)] = offset + 2
            if args.start <= frame <= args.end and offset in sites:
                code = sites[offset]
                memory_address = load_address(code, ram, x, y)
                if memory_address is not None and 0x6000 <= memory_address < 0x6400:
                    key = ((pc + len(code)) & 65535, offset + len(code), stack)
                    assert key not in pending, "overlapping unresolved collision reads"
                    pending[key] = snapshot(pc, offset, stack, memory_address)
        except Exception as error:
            errors.append(repr(error))

    core.lib.retro_famidash_set_opcode_callback.argtypes = [C.c_void_p]
    core.lib.retro_famidash_set_opcode_callback.restype = None
    assert all(a["buttons"] in (0, 1, 128) and isinstance(a["frames"], int)
               and 0 < a["frames"] <= 200000 for a in witness["actions"])
    tape = [a["buttons"] for a in witness["actions"] for _ in range(a["frames"])]
    assert args.end <= len(witness["trace"]) <= len(tape)
    try:
        for frame, button in enumerate(tape, 1):
            if frame == max(1, args.start - 1):
                core.lib.retro_famidash_set_opcode_callback(C.cast(on_opcode, C.c_void_p))
            core.step(button)
            assert not errors, errors[:3]
            observed = core.observe()
            if observed != witness["trace"][frame - 1]:
                assert frame <= args.intro_frames, f"instrumented replay differs at frame {frame}"
                differences.append(frame)
            if args.start <= frame <= args.end:
                states.append({"frame": frame, **observed})
            if frame >= args.end and not pending:
                break
        assert not pending, "collision read did not reach its following instruction"
    finally:
        core.lib.retro_famidash_set_opcode_callback(None)
    assert samples and any(s["centre_probe"] for s in samples)
    result = {"schema": 2, "centre_probe_classification": "mapped PRG ROM offset of caller",
              "measurement": "Actual LDA result before the following opcode; probe RAM captured before the load.",
              "witness_sha256": hashlib.sha256(case_bytes).hexdigest(), "rom_sha256": witness["rom_sha256"],
              "core_sha256": hashlib.sha256(args.core.read_bytes()).hexdigest(),
              "debug_sha256": hashlib.sha256(args.debug.read_bytes()).hexdigest(),
              "collision_sub": {"pc": sub, "size": sub_size, "rom_offset": sub_rom},
              "death_probe": {"pc": death, "size": death_size, "rom_offset": death_rom},
              "comparison_start_frame": args.intro_frames + 1, "intro_difference_frames": differences,
              "samples": sorted(samples, key=lambda s: s["frame"]), "states": states}
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"samples": len(samples), "centre_samples": sum(s["centre_probe"] for s in samples),
                      "output": str(args.output)}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("witness", "core", "rom", "debug", "output"):
        parser.add_argument(name, type=Path)
    parser.add_argument("--start", type=int, required=True)
    parser.add_argument("--end", type=int, required=True)
    parser.add_argument("--intro-frames", type=int, choices=(0, 40), default=0)
    args = parser.parse_args()
    assert 0 < args.start <= args.end
    run(args)
