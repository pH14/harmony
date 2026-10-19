# SPDX-License-Identifier: AGPL-3.0-or-later
"""Execute assembled vertical arithmetic against unbounded signed arithmetic."""

import argparse
import hashlib
import json
from pathlib import Path
import re


def run(rom, debug):
    data, symbols, segments = rom.read_bytes(), {}, {}
    for line in debug.read_text().splitlines():
        fields = dict(re.findall(r'(\w+)=("[^"]*"|[^,\t]+)', line))
        if line.startswith("seg\t") and "ooffs" in fields:
            segments[int(fields["id"])] = fields
        if line.startswith("sym\t") and "val" in fields:
            symbols[fields["name"].strip('"')] = fields
    aliases = {"currplayer_y": "currplayer_rely", "currplayer_x": "currplayer_relx"}
    address = lambda name: int(symbols["_" + (name if "_" + name in symbols else aliases.get(name, name))]["val"], 0)
    memory = bytearray(65536)
    banked = {}
    for name in ("add_velocity_y_checked", "subtract_velocity_y_checked", "correct_y_from_a",
                 "checked_y_error", "enforce_checked_y_fault", "checked_y_error_banked", "enforce_checked_y_fault_banked"):
        if "_" + name not in symbols:
            continue
        symbol = symbols["_" + name]
        segment = segments[int(symbol["seg"])]
        start, size = address(name), int(symbol["size"])
        offset = int(segment["ooffs"], 0) + start - int(segment["start"], 0)
        memory[start:start + size] = data[offset:offset + size]
        if name.endswith("_banked"):
            banked[start] = int(segment["bank"], 0)

    dispatches = 0
    dispatcher = int(symbols["crossPRGBankJump"]["val"], 0) if banked else None

    def execute(name, a=0, carry=0):
        nonlocal dispatches
        pc, x, y, zero, negative = address(name), 0, 0, False, False
        for _ in range(100):
            if pc == dispatcher:
                target = a | x << 8
                assert banked.get(target) == y, "generated dispatcher must select the correct arithmetic kernel and bank"
                pc = target
                dispatches += 1
            op = memory[pc]
            pc += 1
            if op == 0x60:
                return
            if op in (0xad, 0x8d, 0x6d, 0x4d, 0x4c):
                arg = memory[pc] + 256 * memory[pc + 1]
                pc += 2
            elif op in (0xa5, 0xa9, 0xa2, 0xa0, 0x65, 0xe5, 0x85, 0x86, 0x45, 0x29,
                        0x09, 0xc9, 0xb1, 0x91, 0x10, 0x30, 0x90, 0xb0, 0xf0, 0xd0):
                arg = memory[pc]
                pc += 1
            elif op not in (0x18, 0x38, 0x6a, 0xe8):
                raise AssertionError(f"unmodeled opcode {op:02x} at {pc - 1:04x}")
            if op in (0xa5, 0xad, 0xa9, 0xb1):
                a = arg if op == 0xa9 else memory[(memory[arg] + 256 * memory[(arg + 1) & 255] + y) & 65535] if op == 0xb1 else memory[arg]
            elif op in (0xa2, 0xa0):
                if op == 0xa2:
                    x = arg
                else:
                    y = arg
                zero, negative = arg == 0, bool(arg & 128)
                continue
            elif op in (0x85, 0x8d, 0x86, 0x91):
                destination = (memory[arg] + 256 * memory[(arg + 1) & 255] + y) & 65535 if op == 0x91 else arg
                memory[destination] = x if op == 0x86 else a
                continue
            elif op in (0x65, 0x6d, 0xe5):
                result = a - memory[arg] - (1 - carry) if op == 0xe5 else a + memory[arg] + carry
                carry, a = int(result >= 0) if op == 0xe5 else int(result > 255), result & 255
            elif op == 0x6a:
                carry, a = a & 1, (a >> 1) | (carry << 7)
            elif op in (0x45, 0x4d):
                a ^= memory[arg]
            elif op == 0x29:
                a &= arg
            elif op == 0x09:
                a |= arg
            elif op == 0xc9:
                carry, zero, negative = int(a >= arg), a == arg, bool((a - arg) & 128)
                continue
            elif op in (0x18, 0x38):
                carry = int(op == 0x38)
                continue
            elif op == 0xe8:
                x = (x + 1) & 255
                zero, negative = x == 0, bool(x & 128)
                continue
            elif op == 0x4c:
                pc = arg
                continue
            elif op in (0x10, 0x30, 0x90, 0xb0, 0xf0, 0xd0):
                take = negative == (op == 0x30) if op in (0x10, 0x30) else carry == (op == 0xb0) if op in (0x90, 0xb0) else zero == (op == 0xf0)
                if take:
                    pc += arg if arg < 128 else arg - 256
                continue
            zero, negative = a == 0, bool(a & 128)
        raise AssertionError("arithmetic helper did not return")

    pos, vel, fault, cube = map(address, ("currplayer_y", "currplayer_vel_y", "checked_y_fault", "cube_data"))
    cases = 0

    def check(name, position, delta, player=0, exemption=None, carry=0):
        nonlocal cases
        memory[pos:pos + 2] = position.to_bytes(2, "little")
        memory[vel:vel + 2] = (delta & 65535).to_bytes(2, "little")
        memory[address("currplayer")] = player
        memory[address("currplayer_x") + 1] = 80
        for option in ("wrap_mode", "DEBUG_MODE", "invincible_counter"):
            memory[address(option)] = int(option == exemption)
        if exemption == "inactive":
            memory[address("currplayer_x") + 1] = 32
        memory[fault], memory[cube], memory[cube + 1] = 0, 0xa4, 0x52
        expected = position + (-delta if name.startswith("subtract") else delta * 256 if name == "correct_y_from_a" else delta)
        failed = not exemption and not 0 <= expected <= 65535
        result = expected & 65535
        if failed:
            result = (result & 255) | ((6 if expected < 0 else 224) << 8)
        execute(name, delta & 255, carry)
        actual = int.from_bytes(memory[pos:pos + 2], "little")
        assert (actual, memory[fault], memory[cube], memory[cube + 1]) == (result, int(failed), 0xa4 | int(failed and player == 0), 0x52 | int(failed and player == 1)), (name, position, delta, player, exemption, actual, result)
        cases += 1

    for name in ("add_velocity_y_checked", "subtract_velocity_y_checked"):
        for delta in (-32768, -1594, -257, -1, 0, 1, 257, 1594, 32767):
            for position in range(65536):
                check(name, position, delta, position & 1, carry=position & 1)
        for exemption in ("wrap_mode", "DEBUG_MODE", "invincible_counter", "inactive"):
            for delta in (-32768, -1594, -1, 0, 1, 1594, 32767):
                for position in (0, 1, 255, 256, 32768, 65280, 65534, 65535):
                    check(name, position, delta, position & 1, exemption)
    for delta in range(-128, 128):
        for high in range(256):
            check("correct_y_from_a", high * 256 + 173, delta, high & 1, carry=high & 1)
    for player in (0, 1):
        memory[address("currplayer")] = player
        for debug_value in (0, 1):
            for invincible in (0, 1):
                memory[address("DEBUG_MODE")] = debug_value
                memory[address("invincible_counter")] = invincible
                memory[fault], memory[cube], memory[cube + 1] = 1, 0xa4, 0x52
                execute("enforce_checked_y_fault")
                active = not (debug_value or invincible)
                assert (memory[fault], memory[cube], memory[cube + 1]) == (int(active), 0xa4 | int(active and player == 0), 0x52 | int(active and player == 1))
                cases += 1
    return {"status": "assembled-vertical-motion-matches-unbounded-oracle", "cases": cases,
            "bank_dispatches_checked": dispatches,
            "mapper_dispatch": "address and bank arguments checked; mapper implementation not interpreted" if banked else "not used",
            "rom_sha256": hashlib.sha256(data).hexdigest(), "debug_sha256": hashlib.sha256(debug.read_bytes()).hexdigest()}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("rom", type=Path)
    parser.add_argument("debug", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = run(args.rom, args.debug)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
