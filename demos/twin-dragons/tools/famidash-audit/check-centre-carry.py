# SPDX-License-Identifier: AGPL-3.0-or-later
"""Execute assembled collision-coordinate arithmetic against a linear oracle."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re


def run(rom, debug):
    spec = importlib.util.spec_from_file_location("geometry", Path(__file__).with_name("verify-geometry.py"))
    geometry = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(geometry)
    data, symbols = rom.read_bytes(), {}
    for line in debug.read_text().splitlines():
        if line.startswith("sym\t"):
            name = re.search(r'name="([^"]+)"', line)
            value = re.search(r"(?:^|,)val=0x([\da-f]+)", line, re.I)
            if name and value:
                symbols[name[1]] = int(value[1], 16)
    entry = geometry.debug_symbols(debug)["_bg_coll_death"]
    code, ram = data[entry:entry + 256], bytearray(2048)
    scroll, generic = symbols["_scroll_y"], symbols["_Generic"]
    temp_y, temp_room = symbols["_temp_y"], symbols["_temp_room"]
    ram[generic], ram[generic + 2] = 80, 15

    def execute():
        a, x, carry, zero, pc = 0, 195, 1, False, 0
        for _ in range(80):
            op = code[pc]
            pc += 1
            if op in (0xae, 0xad, 0x6d, 0x8d, 0x8e, 0xed):
                arg = code[pc] + 256 * code[pc + 1]
                pc += 2
            elif op in (0xa6, 0xa5, 0xa9, 0x65, 0x69, 0x90, 0xb0, 0xf0, 0xd0, 0xc9, 0x85, 0x86, 0xe5, 0xe9):
                arg = code[pc]
                pc += 1
            elif op not in (0x18, 0x38, 0xe8, 0x4a):
                raise AssertionError(f"unmodeled opcode {op:02x} at ROM offset {entry + pc - 1:x}")
            if op in (0xae, 0xa6):
                x = ram[arg]
                zero = x == 0
            elif op in (0xad, 0xa5, 0xa9):
                a = arg if op == 0xa9 else ram[arg]
                zero = a == 0
            elif op == 0x18:
                carry = 0
            elif op == 0x38:
                carry = 1
            elif op in (0x65, 0x6d, 0x69):
                value = a + (arg if op == 0x69 else ram[arg]) + carry
                carry, a = int(value > 255), value & 255
                zero = a == 0
            elif op in (0xe5, 0xed, 0xe9):
                value = a - (arg if op == 0xe9 else ram[arg]) - (1 - carry)
                carry, a = int(value >= 0), value & 255
                zero = a == 0
            elif op == 0xc9:
                carry, zero = int(a >= arg), a == arg
            elif op in (0x90, 0xb0, 0xf0, 0xd0):
                take = carry == (op == 0xb0) if op in (0x90, 0xb0) else zero == (op == 0xf0)
                if take:
                    pc += arg if arg < 128 else arg - 256
            elif op == 0xe8:
                x = (x + 1) & 255
                zero = x == 0
            elif op == 0x4a:
                carry, a = a & 1, a >> 1
                zero = a == 0
            elif op in (0x85, 0x8d):
                ram[arg] = a
            elif op in (0x86, 0x8e):
                ram[arg] = x
                if arg == temp_room:
                    return ram[temp_y] + 256 * ram[temp_room]
        raise AssertionError("coordinate stores not reached")

    cases = 0
    for camera in range(960):
        ram[scroll], ram[scroll + 1] = camera % 240, camera // 240
        for relative in range(256):
            ram[generic + 1] = relative
            for height in (7, 8, 15):
                ram[generic + 3] = height
                for mini in (0, 1):
                    ram[symbols["_currplayer_mini"]] = mini
                    offset = height // 2 + ((16 - height) // 2 if mini else 0)
                    value = camera + relative + offset
                    expected = value // 240 * 256 + value % 240
                    actual = execute()
                    assert actual == expected, (camera, relative, height, mini, actual, expected)
                    cases += 1
    return {"status": "assembled-centre-conversion-matches-linear-oracle", "cases": cases,
            "heights": [7, 8, 15], "rom_sha256": hashlib.sha256(data).hexdigest(),
            "debug_sha256": hashlib.sha256(debug.read_bytes()).hexdigest(), "entry_rom_offset": entry}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("rom", type=Path)
    parser.add_argument("debug", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = run(args.rom, args.debug)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
