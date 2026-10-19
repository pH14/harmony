# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check compiled spider metadata lookup and the source's bounded vertical base test."""

import argparse
import ctypes
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile


def check(args):
    source = (args.source / "SAUCE/functions/spider_bases.h").read_text()
    start = source.index("unsigned char spider_base_crossed(")
    opening = source.index("{", start)
    end, depth = opening + 1, 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    cases = 0
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory)
        (path / "kernel.c").write_text(source[start:end])
        subprocess.run(["cc", "-shared", "-fPIC", "-O2", str(path / "kernel.c"), "-o", str(path / "kernel.so")], check=True)
        kernel = ctypes.CDLL(str(path / "kernel.so")).spider_base_crossed
        kernel.argtypes = [ctypes.c_ubyte] * 5
        kernel.restype = ctypes.c_ubyte
        for down in (0, 1):
            for magnitude in range(16):
                delta = magnitude if down else -magnitude
                for phase in range(16):
                    target = phase + delta
                    origin = target // 16 * 16
                    for old_flags in range(4):
                        for new_flags in range(4):
                            if not origin and old_flags != new_flags:
                                continue
                            expected = False
                            for tile_origin, flags in {(0, old_flags), (origin, new_flags)}:
                                for bit, offset in [(1, 0), (2, 15)]:
                                    plane = tile_origin + offset
                                    if flags & bit and min(phase, target) <= plane and max(phase, target) >= plane + 1:
                                        expected = True
                            actual = kernel(old_flags, phase, new_flags, target & 15, down)
                            assert bool(actual) == expected, (down, magnitude, phase, old_flags, new_flags)
                            cases += 1

    symbols, segments = {}, {}
    for line in args.debug.read_text().splitlines():
        fields = dict(re.findall(r'(\w+)=("[^"]*"|[^,\t]+)', line))
        if line.startswith("seg\t") and "ooffs" in fields:
            segments[int(fields["id"])] = fields
        if line.startswith("sym\t") and "val" in fields:
            symbols[fields["name"].strip('"')] = fields
    rom = args.rom.read_bytes()
    memory = bytearray(65536)
    for name in ("_spider_base_flags", "metatiles_bases"):
        symbol = symbols[name]
        segment = segments[int(symbol["seg"])]
        size = int(segment["size"], 0)
        address = int(segment["start"], 0)
        offset = int(segment["ooffs"], 0)
        memory[address:address + size] = rom[offset:offset + size]
    address = lambda name: int(symbols[name]["val"], 0)
    index = json.loads(args.index.read_bytes())
    names = {tile["name"]: n for n, tile in enumerate(index["metatiles"])}
    masks = [0] * len(names)
    definitions = args.source / "METATILES/spike_bases.inc"
    for name, flags in re.findall(r'SpikeBase\s+"([^"]+)",\s*(\d+)', definitions.read_text()):
        masks[names[name]] = int(flags)
        if int(flags) & 3:
            assert index["metatiles"][names[name]]["collision"] != "COL_NONE", "empty-cell fast path must not skip a base"
    table = address("metatiles_bases")
    for metatile, mask in enumerate(masks):
        packed = memory[table + metatile // 2]
        assert (packed >> (4 if metatile & 1 else 0)) & 15 == mask
        memory[address("_spider_base_meta")] = metatile
        pc, a, x, y, carry = address("_spider_base_flags"), 0, 0, 0, 0
        for _ in range(24):
            opcode = memory[pc]
            pc += 1
            if opcode == 0x60:
                break
            if opcode in (0xAE, 0xB9):
                operand = memory[pc] | memory[pc + 1] << 8
                pc += 2
                if opcode == 0xAE:
                    x = memory[operand]
                else:
                    a = memory[operand + y]
            elif opcode == 0x8A:
                a = x
            elif opcode == 0xA8:
                y = a
            elif opcode == 0x4A:
                carry, a = a & 1, a >> 1
            elif opcode == 0x90:
                offset = memory[pc]
                pc += 1
                if not carry:
                    pc += offset if offset < 128 else offset - 256
            elif opcode == 0x29:
                a &= memory[pc]
                pc += 1
            else:
                raise AssertionError(f"unmodeled lookup opcode {opcode:02x}")
        else:
            raise AssertionError("lookup did not return")
        assert a == mask & 3, metatile
    return {"status": "verified", "geometry_cases": cases, "assembled_lookup_cases": len(masks),
            "source_sha256": hashlib.sha256(source.encode()).hexdigest(),
            "metadata_sha256": hashlib.sha256(definitions.read_bytes()).hexdigest(),
            "rom_sha256": hashlib.sha256(rom).hexdigest(),
            "debug_sha256": hashlib.sha256(args.debug.read_bytes()).hexdigest(),
            "index_sha256": hashlib.sha256(args.index.read_bytes()).hexdigest(),
            "limitation": "Geometry uses the host-compiled source body; lookup executes assembled instructions. This does not emulate NES banking, frame budget, or other collision routines."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source", "rom", "debug", "index", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    result = check(args)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
