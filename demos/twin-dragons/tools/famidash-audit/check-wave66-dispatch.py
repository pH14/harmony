# SPDX-License-Identifier: AGPL-3.0-or-later
"""Exhaustively execute the assembled collision-history query decision."""

import argparse
import hashlib
import itertools
import json
from pathlib import Path
import re


def check_direct_call_bank(rom, symbols, segments, caller_name, callee_name):
    def location(name):
        symbol = symbols[name]
        segment = segments[int(symbol["seg"])]
        address = int(symbol["val"], 0)
        offset = int(segment["ooffs"], 0) + address - int(segment["start"], 0)
        return address, offset, int(symbol.get("size", "0"), 0)

    caller, caller_offset, size = location(caller_name)
    callee, callee_offset, _ = location(callee_name)
    code = rom[caller_offset:caller_offset + size]
    target = callee.to_bytes(2, "little")
    calls = [caller_offset + n for n in range(len(code) - 2)
             if code[n] in (0x20, 0x4C) and code[n + 1:n + 3] == target]
    assert calls, f"no direct call from {caller_name} to {callee_name}"
    caller_bank, callee_bank = (caller_offset - 16) // 8192, (callee_offset - 16) // 8192
    if 0xA000 <= callee < 0xE000:
        assert caller // 8192 == callee // 8192 and caller_bank == callee_bank, (
            f"bank mismatch: {caller_name} in PRG bank {caller_bank} directly calls "
            f"{callee_name} in PRG bank {callee_bank} at CPU ${callee:04X}")
    else:
        assert 0x8000 <= callee < 0xA000 or callee >= 0xE000, "callee is outside fixed code windows"
    return {"caller": caller_name, "callee": callee_name, "caller_prg_bank": caller_bank,
            "callee_prg_bank": callee_bank, "direct_call_rom_offsets": calls}


def check(source, rom_path, debug_path):
    symbols, segments = {}, {}
    debug = debug_path.read_bytes()
    for line in debug.decode().splitlines():
        fields = dict(re.findall(r'(\w+)=("[^"]*"|[^,\t]+)', line))
        if line.startswith("seg\t") and "ooffs" in fields:
            segments[int(fields["id"])] = fields
        if line.startswith("sym\t") and "val" in fields:
            symbols[fields["name"].strip('"')] = fields
    address = lambda name: int(symbols[name]["val"], 0)
    rom = rom_path.read_bytes()
    direct_calls = [check_direct_call_bank(rom, symbols, segments, caller, callee)
                    for caller, callee in [("_check_spike_motion", "_ss_test_tile"),
                                           ("_ss_test_tile", "_ss_test_spike_tile")]]
    memory = bytearray(65536)
    for name in ("_check_pit_crossing", "_ss_shapes", "_ss_wave66"):
        segment = segments[int(symbols[name]["seg"])]
        start, size = int(segment["start"], 0), int(segment["size"], 0)
        offset = int(segment["ooffs"], 0)
        memory[start:start + size] = rom[offset:offset + size]
    definitions = (source / "METATILES/metatiles.h").read_text()
    steep = {int(value, 0) for name, value in re.findall(
        r'^#define\s+(COL_SLOPE_\w+66_(?:TOP|BOT))\s+(0x[\da-fA-F]+|\d+)',
        definitions, re.M)}
    assert len(steep) == 8
    mini45 = set()
    if (source / "SAUCE/functions/mini_wave45.h").exists():
        mini45 = {int(value, 0) for name, value in re.findall(
            r'^#define\s+(COL_SLOPE_\w+45)\s+(0x[\da-fA-F]+|\d+)', definitions, re.M)}
        assert len(mini45) == 4
    mini22 = set()
    if (source / "SAUCE/functions/mini_wave22.h").exists():
        mini22 = {int(value, 0) for name, value in re.findall(
            r'^#define\s+(COL_SLOPE_\w+22_(?:LEFT|RIGHT))\s+(0x[\da-fA-F]+|\d+)', definitions, re.M)}
        assert len(mini22) == 8
    wave = int(re.search(r'^#define\s+GAMEMODE_WAVE\s+(0x[\da-fA-F]+|\d+)',
                         (source / "SAUCE/famidash.h").read_text(), re.M)[1], 0)
    table = address("_ss_wave66")
    assert {n for n in range(32) if memory[table + n]} == steep | mini45 | mini22
    if mini45:
        assert all(memory[table + n] == (1 if n in steep else 2 if n in mini45 else 4 if n in mini22 else 0)
                   for n in range(32))
    shapes = memory[address("_ss_shapes"):address("_ss_shapes") + 64]
    callees = {address(name) for name in ("crossPRGBankJump", "_check_spike_motion")
               if name in symbols}
    assert callees

    def execute():
        pc, a, x, y, carry, zero = address("_check_pit_crossing"), 0, 0, 0, False, False
        for steps in range(160):
            opcode = memory[pc]
            pc += 1
            if opcode == 0x60:
                return False, steps + 1
            if opcode == 0x20:
                target = memory[pc] | memory[pc + 1] << 8
                assert target in callees, (hex(pc - 1), hex(target))
                return True, steps + 1
            if opcode in (0xF0, 0xD0, 0xB0, 0x90):
                delta = memory[pc]
                pc += 1
                take = {0xF0: zero, 0xD0: not zero, 0xB0: carry, 0x90: not carry}[opcode]
                if take:
                    pc += delta if delta < 128 else delta - 256
                continue
            if opcode == 0x4C:
                pc = memory[pc] | memory[pc + 1] << 8
                continue
            if opcode == 0x4A:
                carry = bool(a & 1)
                a >>= 1
                zero = a == 0
                continue
            if opcode in (0xA9, 0xA2, 0xA0, 0x29, 0xC9, 0xC0):
                value = memory[pc]
                pc += 1
            elif opcode in (0xA5, 0xA6, 0xA4, 0x85, 0x05, 0x45):
                operand = memory[pc]
                pc += 1
                value = memory[operand]
            elif opcode in (0xAD, 0xAE, 0xAC, 0xBD, 0xBC, 0xB9, 0x8D, 0x9D, 0x1D, 0x5D, 0x0D, 0x4D):
                operand = memory[pc] | memory[pc + 1] << 8
                pc += 2
                if opcode in (0xBD, 0xBC, 0x9D, 0x1D, 0x5D):
                    operand += x
                if opcode == 0xB9:
                    operand += y
                value = memory[operand]
            else:
                raise AssertionError((hex(pc - 1), hex(opcode)))
            if opcode in (0xA9, 0xA5, 0xAD, 0xBD, 0xB9):
                a, zero = value, value == 0
            elif opcode in (0xA2, 0xA6, 0xAE):
                x, zero = value, value == 0
            elif opcode in (0xA0, 0xA4, 0xAC, 0xBC):
                y, zero = value, value == 0
            elif opcode in (0x85, 0x8D, 0x9D):
                memory[operand] = a
            elif opcode in (0x05, 0x0D, 0x1D):
                a |= value
                zero = a == 0
            elif opcode in (0x45, 0x4D, 0x5D):
                a ^= value
                zero = a == 0
            elif opcode == 0x29:
                a &= value
                zero = a == 0
            elif opcode in (0xC9, 0xC0):
                register = a if opcode == 0xC9 else y
                carry, zero = register >= value, register == value
        raise AssertionError("query decision did not terminate")

    cases, maximum_steps = 0, 0
    for old, new, mode, dx, dy, player, mini in itertools.product(
            range(64), range(64), (0, wave), (0, 1), (0, 1), (0, 1), (0, 1)):
        values = {"_currplayer": player, "_gamemode": mode, "_currplayer_mini": mini,
                  "_collision": new, "_temp_x": 15 + dx, "_temp_y": 15 + dy,
                  "_collision_metatile": 111}
        for name, value in values.items():
            memory[address(name)] = value
        for name, value in (("_pit_previous_collision", old), ("_pit_previous_x", 15),
                            ("_pit_previous_y", 15)):
            memory[address(name) + player] = value
        expected = bool(shapes[old] or shapes[new] or (dx and dy) or
                        (mode == wave and (dx or dy) and
                         (old in steep or new in steep or
                          (mini and (old in mini45 | mini22 or new in mini45 | mini22)))))
        actual, steps = execute()
        assert actual == expected, (old, new, mode, dx, dy, player, mini, actual, expected)
        maximum_steps = max(maximum_steps, steps)
        cases += 1
    return {"status": "verified", "cases": cases, "maximum_instructions_to_decision": maximum_steps,
            "mini45_classes": sorted(mini45), "mini22_classes": sorted(mini22),
            "direct_call_banks": direct_calls,
            "rom_sha256": hashlib.sha256(rom).hexdigest(),
            "debug_sha256": hashlib.sha256(debug).hexdigest(),
            "source_sha256": {name: hashlib.sha256((source / "SAUCE/functions" / name).read_bytes()).hexdigest()
                              for name in ("pit_crossing.h", "spike_sweep.h")},
            "limitation": "Executes the assembled query decision, not the dispatched motion kernel, mapper or frame budget."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source", "rom", "debug", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    result = check(args.source, args.rom, args.debug)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
