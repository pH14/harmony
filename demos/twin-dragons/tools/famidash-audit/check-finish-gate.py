# SPDX-License-Identifier: AGPL-3.0-or-later
"""Execute the assembled offscreen gate against its finish/animated-coin contract."""

import argparse
import hashlib
import json
from pathlib import Path
import re


def check(rom, debug):
    symbols, segments = [], {}
    for line in debug.decode().splitlines():
        fields = dict(re.findall(r'(\w+)=("[^"]*"|[^,\t]+)', line))
        if line.startswith("seg\t") and "ooffs" in fields:
            segments[int(fields["id"])] = {key: int(fields[key], 0) for key in ("start", "size", "ooffs")}
        if line.startswith("sym\t") and "val" in fields:
            symbols.append(fields)
    gate, = [symbol for symbol in symbols if symbol["name"] == '"sprite_offscreen"']
    local = {symbol["name"].strip('"'): int(symbol["val"], 0)
             for symbol in symbols if symbol.get("scope") == gate["scope"]}
    addresses = {symbol["name"].strip('"'): int(symbol["val"], 0) for symbol in symbols}
    segment = segments[int(gate["seg"])]
    memory = bytearray(65536)
    start, size, offset = (segment[key] for key in ("start", "size", "ooffs"))
    memory[start:start + size] = rom[offset:offset + size]
    cases = 0
    for slot in range(16):
        for kind in range(256):
            memory[addresses["_activesprites_type"] + slot] = kind
            for animating in (0, 1, 255):
                memory[addresses["_animating"]] = animating
                pc, a, y, zero = local["sprite_offscreen"], 0, 0, False
                for _ in range(32):
                    if pc in (local["finish"], local["sprite_offscreen2"]):
                        actual = pc == local["finish"]
                        break
                    opcode = memory[pc]
                    pc += 1
                    if opcode in (0xAD, 0xBD, 0xBC):
                        address = memory[pc] | memory[pc + 1] << 8
                        pc += 2
                        value = memory[address + (slot if opcode != 0xAD else 0)]
                        if opcode == 0xBC:
                            y = value
                        else:
                            a = value
                        zero = value == 0
                    elif opcode in (0xC0, 0xC9):
                        zero = (y if opcode == 0xC0 else a) == memory[pc]
                        pc += 1
                    elif opcode == 0x98:
                        a = y
                        zero = a == 0
                    elif opcode == 0x49:
                        a ^= memory[pc]
                        pc += 1
                        zero = a == 0
                    elif opcode == 0x4A:
                        a >>= 1
                        zero = a == 0
                    elif opcode == 0xF0:
                        displacement = memory[pc]
                        pc += 1
                        if zero:
                            pc += displacement if displacement < 128 else displacement - 256
                    else:
                        raise AssertionError(f"unsupported gate opcode {opcode:02x} at {pc - 1:04x}")
                else:
                    raise AssertionError("gate did not reach an activation decision")
                expected = kind == 15 or (bool(animating) and kind in (7, 26, 27))
                assert actual == expected, (slot, kind, animating, actual, expected)
                cases += 1
    return {"status": "assembled-gate-matches-finish-and-animated-coin-contract", "cases": cases,
            "rom_sha256": hashlib.sha256(rom).hexdigest(), "debug_sha256": hashlib.sha256(debug).hexdigest()}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("rom", type=Path)
    parser.add_argument("debug", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = check(args.rom.read_bytes(), args.debug.read_bytes())
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
