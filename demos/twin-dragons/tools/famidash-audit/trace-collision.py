# SPDX-License-Identifier: AGPL-3.0-or-later
"""Observe original-ROM collision-map reads using an export-only FCEUmm build."""

import argparse
import ctypes as C
import gzip
import hashlib
import json
import re
from pathlib import Path

from fceumm import Fceumm, SymbolFceumm


class CPU(C.Structure):
    _fields_ = [("tcount", C.c_int32), ("PC", C.c_uint16), ("A", C.c_uint8),
                ("X", C.c_uint8), ("Y", C.c_uint8), ("S", C.c_uint8), ("P", C.c_uint8),
                ("mooPI", C.c_uint8), ("jammed", C.c_uint8), ("count", C.c_int32),
                ("IRQlow", C.c_uint32), ("DB", C.c_uint8)]


def mapped_rom_offset(pages, prg_base, prg_size, address):
    page = pages[address >> 11]
    offset = page + address - prg_base if page else -1
    return 16 + offset if 0 <= offset < prg_size else None


def run(args):
    data = args.witness.read_bytes()
    witness = json.loads(gzip.decompress(data) if args.witness.suffix == ".gz" else data)
    assert hashlib.sha256(args.rom.read_bytes()).hexdigest() == witness["rom_sha256"]
    symbols, segments, symbol_segments = {}, {}, {}
    debug_lines = args.debug.read_text().splitlines()
    for line in debug_lines:
        if line.startswith("seg\t"):
            fields = dict(re.findall(r'(\w+)=("[^\"]*"|[^,\t]+)', line))
            if "ooffs" in fields:
                segments[int(fields["id"])] = (int(fields["start"], 0), int(fields["ooffs"], 0))
    for line in debug_lines:
        if not line.startswith("sym\t"):
            continue
        name, value = re.search(r'name="([^"]+)"', line), re.search(r"(?:^|,)val=0x([\da-f]+)", line, re.I)
        if name and value:
            size = re.search(r"(?:^|,)size=(\d+)", line)
            symbols[name[1]] = (int(value[1], 16), int(size[1]) if size else None)
            segment = re.search(r"(?:^|,)seg=(\d+)", line)
            if segment:
                symbol_segments[name[1]] = int(segment[1])
    def symbol_rom_offset(name):
        start, offset = segments[symbol_segments[name]]
        return offset + symbols[name][0] - start

    sub_rom = symbol_rom_offset("_bg_collision_sub")
    death_rom = symbol_rom_offset("_bg_coll_death")
    sub, size = symbols["_bg_collision_sub"]
    death, death_size = symbols["_bg_coll_death"]
    assert size and death_size and 0x8000 <= sub < sub + size <= 0x10000
    locations = {name: symbols[f"_{name}"][0] for name in
                 ["temp_x", "temp_y", "Generic", "currplayer", "currplayer_mini", "gamemode"]}
    locations.update({name: symbols[f"_{name}"][0] for name in
                      ["wrap_mode", "dblocked", "scroll_x", "scroll_y", "scroll_y_subpx"]})
    locations["relative_y"] = symbols["_currplayer_rely" if "target_layout" in witness else "_currplayer_y"][0]
    assert all(0 <= value < 2044 for value in locations.values())
    scratch = {}
    for name, width in [("eject_U", 1), ("eject_D", 1), ("currplayer_rely", 2),
                        ("currplayer_y", 2), ("currplayer_vel_y", 2), ("slope_type", 2),
                        ("slope_frames", 2), ("tmp4", 1), ("tmp7", 1), ("tmp8", 1)]:
        if "_" + name in symbols:
            address = symbols["_" + name][0]
            assert 0 <= address <= 2048 - width
            scratch[name] = address, width
    core = SymbolFceumm(args.core, args.rom, witness["target_layout"]) if "target_layout" in witness else Fceumm(args.core, args.rom)
    core.startup_frames = witness["options"].get("fceumm_startup_released_frames", 180)
    core.boot(witness["options"]["side"], witness["boot_level"], witness["options"].get("menu_delay", 0))
    lib, ram = core.lib, core.ram
    cpu = CPU.in_dll(lib, "X")
    pages = (C.c_void_p * 32).in_dll(lib, "Page")
    prg = (C.c_void_p * 32).in_dll(lib, "PRGptr")
    prg_sizes = (C.c_uint32 * 32).in_dll(lib, "PRGsize")
    rom = args.rom.read_bytes()
    assert not rom[6] & 4, "trainer layout is unsupported"
    assert prg[0] and prg_sizes[0] == rom[4] * 16384
    assert C.string_at(prg[0], 16) == rom[16:32]

    lib.GetReadHandler.argtypes, lib.GetReadHandler.restype = [C.c_int32], C.c_void_p
    handlers = {lib.GetReadHandler(address) for address in range(0x6000, 0x6400)}
    assert len(handlers) == 1, "collision SRAM requires one unchanged read handler"
    read_type = C.CFUNCTYPE(C.c_uint8, C.c_uint32)
    original = read_type(handlers.pop())
    frame, samples, states, intro_differences = 0, [], [], []

    @read_type
    def hook(address):
        value = original(address)
        if args.start <= frame <= args.end and sub <= cpu.PC < sub + size:
            pc_rom = mapped_rom_offset(pages, prg[0], prg_sizes[0], cpu.PC)
            if pc_rom is None or not sub_rom <= pc_rom < sub_rom + size:
                return value
            returned = ram[0x100 + ((cpu.S + 1) & 255)] | (ram[0x100 + ((cpu.S + 2) & 255)] << 8)
            caller_rom = mapped_rom_offset(pages, prg[0], prg_sizes[0], returned)
            y = ram[locations["temp_y"]]
            if "target_layout" in witness:
                y |= ram[locations["temp_y"] + 1] << 8
            else:
                y += ram[symbols["_temp_room"][0]] * 240
            generic = locations["Generic"]
            scroll_x = int.from_bytes(bytes(ram[locations["scroll_x"]:locations["scroll_x"] + 4]), "little", signed=True)
            base_x = scroll_x + ram[generic]
            probe_offset = ((ram[locations["temp_x"]] - (base_x & 255) + 128) % 256) - 128
            samples.append({"frame": frame, "pc": cpu.PC, "return_address": returned,
                            "centre_probe": caller_rom is not None and death_rom <= caller_rom < death_rom + death_size,
                            "pc_rom_offset": pc_rom, "caller_rom_offset": caller_rom,
                            "address": address, "metatile": value, "sample_x_low": ram[locations["temp_x"]],
                            "sample_world_x": base_x + probe_offset, "scroll_x_at_probe": scroll_x,
                            "relative_probe_offset": probe_offset,
                            "sample_world_y": y, "generic": list(ram[generic:generic + 4]),
                            "player": ram[locations["currplayer"]], "mini": ram[locations["currplayer_mini"]],
                            "mode": ram[locations["gamemode"]], "wrap_mode": ram[locations["wrap_mode"]],
                            "dblocked": ram[locations["dblocked"] + ram[locations["currplayer"]]],
                            "camera_at_probe": ram[locations["scroll_y"]] + ram[locations["scroll_y"] + 1] * (256 if "target_layout" in witness else 240),
                            "camera_subpixel": ram[locations["scroll_y_subpx"]],
                            "relative_y_fixed": ram[locations["relative_y"]] + 256 * ram[locations["relative_y"] + 1],
                            "fractional_camera_carry": (ram[locations["relative_y"]] + ram[locations["scroll_y_subpx"]]) >> 8})
        return value

    lib.SetReadHandler.argtypes = [C.c_int32, C.c_int32, read_type]
    lib.SetReadHandler(0x6000, 0x63ff, hook)
    for action in witness["actions"]:
        for _ in range(action["frames"]):
            frame += 1
            core.step(action["buttons"])
            observed = core.observe()
            if observed != witness["trace"][frame - 1]:
                assert frame <= args.intro_frames, f"instrumented replay differs at frame {frame}"
                intro_differences.append(frame)
            if args.start <= frame <= args.end:
                values = {name: int.from_bytes(bytes(ram[address:address + width]), "little")
                          for name, (address, width) in scratch.items()}
                states.append({"frame": frame, **observed, "scratch": values})
            if frame >= args.end:
                break
        if frame >= args.end:
            break
    assert samples and any(sample["centre_probe"] for sample in samples)
    result = {"schema": 2, "centre_probe_classification": "mapped PRG ROM offset of caller", "witness_sha256": hashlib.sha256(data).hexdigest(),
              "rom_sha256": witness["rom_sha256"], "core_sha256": hashlib.sha256(args.core.read_bytes()).hexdigest(),
              "debug_sha256": hashlib.sha256(args.debug.read_bytes()).hexdigest(),
              "collision_sub": {"pc": sub, "size": size, "rom_offset": sub_rom}, "death_probe": {"pc": death, "size": death_size, "rom_offset": death_rom},
              "comparison_start_frame": args.intro_frames + 1, "intro_difference_frames": intro_differences,
              "samples": samples, "states": states}
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"samples": len(samples), "centre_samples": sum(s["centre_probe"] for s in samples), "output": str(args.output)}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["witness", "core", "rom", "debug", "output"]:
        parser.add_argument(name, type=Path)
    parser.add_argument("--start", type=int, required=True)
    parser.add_argument("--end", type=int, required=True)
    parser.add_argument("--intro-frames", type=int, choices=(0, 40), default=0)
    args = parser.parse_args()
    assert 0 < args.start <= args.end
    run(args)
