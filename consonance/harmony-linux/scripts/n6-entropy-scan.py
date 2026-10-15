#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Reject untrusted entropy instructions from executable ELF load segments."""

from __future__ import annotations

import struct
import subprocess
import sys
import tempfile
from pathlib import Path


ELF_HEADER = "<16sHHIQQQIHHHHHH"
PROGRAM_HEADER = "<IIQQQQQQ"
SECTION_HEADER = "<IIQQQQIIQQ"
PT_LOAD = 1
PF_X = 1
SHF_ALLOC = 2
EM_X86_64 = 62
EM_AARCH64 = 183
RNDR = 0xD53B2400
RNDRRS = 0xD53B2420


def executable_ranges(path: Path) -> tuple[int, list[tuple[str, int, bytes]]]:
    """Return executable segments and mapped sections as decoder starts."""
    data = path.read_bytes()
    header_size = struct.calcsize(ELF_HEADER)
    program_size = struct.calcsize(PROGRAM_HEADER)
    if len(data) < header_size:
        raise ValueError("truncated ELF header")
    (
        ident,
        _kind,
        machine,
        version,
        _entry,
        program_offset,
        section_offset,
        _flags,
        elf_header_size,
        program_entry_size,
        program_count,
        section_entry_size,
        section_count,
        string_index,
    ) = struct.unpack_from(ELF_HEADER, data)
    if ident[:4] != b"\x7fELF" or ident[4:7] != b"\x02\x01\x01":
        raise ValueError("require ELF64 little-endian version 1")
    if version != 1 or elf_header_size != header_size:
        raise ValueError("invalid ELF version/header size")
    if machine not in (EM_X86_64, EM_AARCH64):
        raise ValueError(f"unsupported ELF machine {machine}")
    if program_count == 0 or program_entry_size < program_size:
        raise ValueError("missing or short program table")
    table_bytes = program_count * program_entry_size
    if program_offset > len(data) or table_bytes > len(data) - program_offset:
        raise ValueError("program table outside file")
    if section_count == 0 or string_index >= section_count:
        raise ValueError("missing/extended section table unsupported")
    if section_entry_size < struct.calcsize(SECTION_HEADER):
        raise ValueError("short section entry")
    table_bytes = section_count * section_entry_size
    if section_offset > len(data) or table_bytes > len(data) - section_offset:
        raise ValueError("section table outside file")

    def section(index: int):
        return struct.unpack_from(
            SECTION_HEADER, data, section_offset + index * section_entry_size
        )

    strings = section(string_index)
    if strings[1] != 3 or strings[4] > len(data) or strings[5] > len(data) - strings[4]:
        raise ValueError("invalid section-name table")
    names = data[strings[4] : strings[4] + strings[5]]

    def name_at(offset: int) -> str:
        if offset >= len(names):
            raise ValueError("section-name offset outside table")
        end = names.find(b"\0", offset)
        if end < 0:
            raise ValueError("unterminated section name")
        return names[offset:end].decode("ascii", errors="strict")

    found: list[tuple[str, int, bytes]] = []
    for index in range(program_count):
        kind, flags, offset, address, _physical, size, memory_size, _align = (
            struct.unpack_from(
                PROGRAM_HEADER, data, program_offset + index * program_entry_size
            )
        )
        if kind != PT_LOAD or flags & PF_X == 0:
            continue
        if size == 0 or size > memory_size or size >= (1 << 64) - address:
            raise ValueError(f"invalid executable segment {index} size")
        if machine == EM_AARCH64 and address % 4 != 0:
            raise ValueError(f"unaligned AArch64 executable segment {index}")
        if offset > len(data) or size > len(data) - offset:
            raise ValueError(f"executable segment {index} outside file")
        segment = data[offset : offset + size]
        segment_end = address + size
        mapped_sections = 0
        mapped_ranges: list[tuple[int, int, str]] = []
        for section_index in range(section_count):
            name_offset, _kind, section_flags, section_address, _offset, section_size, *_ = (
                section(section_index)
            )
            if section_flags & SHF_ALLOC == 0 or section_size == 0:
                continue
            if section_size >= (1 << 64) - section_address:
                raise ValueError(f"mapped section {section_index} address overflow")
            section_end = section_address + section_size
            if section_address >= segment_end or section_end <= address:
                continue
            if section_address < address or section_end > segment_end:
                raise ValueError(f"partially mapped executable section {section_index}")
            relative = section_address - address
            mapped_sections += 1
            if machine == EM_X86_64:
                mapped_ranges.append(
                    (relative, relative + section_size, name_at(name_offset))
                )
        if mapped_sections == 0:
            raise ValueError(f"executable segment {index} without a mapped section")
        if machine == EM_AARCH64:
            found.append((f"<exec-segment {index}>", address, segment))
        else:
            mapped_ranges.sort()
            cursor = 0
            for begin, end, label in mapped_ranges:
                if begin > cursor:
                    found.append(
                        (f"<exec-segment {index}>", address + cursor, segment[cursor:begin])
                    )
                found.append((label, address + begin, segment[begin:end]))
                cursor = max(cursor, end)
            if cursor < size:
                found.append(
                    (f"<exec-segment {index}>", address + cursor, segment[cursor:])
                )
    if not found:
        raise ValueError("no executable load segments")
    return machine, found


def arm64_hits(data: bytes) -> list[tuple[int, str]]:
    """Find RNDR/RNDRRS MRS words, ignoring the destination register."""
    hits: list[tuple[int, str]] = []
    for offset in range(0, len(data) - len(data) % 4, 4):
        word = int.from_bytes(data[offset : offset + 4], "little") & ~0x1F
        if word == RNDR:
            hits.append((offset, "RNDR"))
        elif word == RNDRRS:
            hits.append((offset, "RNDRRS"))
    return hits


def x86_hits(segments: list[tuple[str, int, bytes]]) -> list[tuple[str, int, str]]:
    """Decode executable ranges and every directly reachable alternate entry."""
    hits: list[tuple[str, int, str]] = []
    seen: set[tuple[int, str]] = set()
    targets: set[int] = set()
    instructions: dict[int, tuple[int, str, int | None, int]] = {}
    parsed = 0
    ordered = sorted(segments, key=lambda segment: segment[1])
    for left, right in zip(ordered, ordered[1:]):
        if left[1] + len(left[2]) > right[1]:
            raise ValueError("overlapping executable mappings")
    with tempfile.TemporaryDirectory() as temporary:
        segment_path = Path(temporary) / "segment"

        def decode(data: bytes, base: int, label: str, first_only: bool = False):
            segment_path.write_bytes(data)
            try:
                output = subprocess.run(
                    [
                        "objdump", "-D", "-z", "-w", "-b", "binary", "-m", "i386:x86-64",
                        f"--adjust-vma=0x{base:x}", str(segment_path),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout
            except (OSError, subprocess.CalledProcessError) as error:
                raise ValueError(f"objdump failed: {error}") from error
            cursor = 0
            decoded_instructions = []
            for line in output.splitlines():
                if ":" not in line:
                    continue
                address_text, remainder = line.split(":", 1)
                try:
                    address = int(address_text.strip(), 16)
                except ValueError:
                    continue
                fields = remainder.split("\t")
                if len(fields) < 3:
                    raise ValueError(f"objdump omitted instruction bytes in {label}")
                hex_bytes = fields[1].split()
                if not hex_bytes or any(
                    len(part) != 2 or any(digit not in "0123456789abcdefABCDEF" for digit in part)
                    for part in hex_bytes
                ):
                    raise ValueError(f"objdump emitted malformed bytes in {label}")
                decoded = bytes(int(part, 16) for part in hex_bytes)
                if address != base + cursor or data[cursor : cursor + len(decoded)] != decoded:
                    raise ValueError(f"objdump bytes differ from executable {label}")
                cursor += len(decoded)
                mnemonic_fields = " ".join(fields[2:]).strip().split()
                if not mnemonic_fields:
                    raise ValueError(f"objdump omitted instruction in {label}")
                instruction = next(
                    (
                        index for index, field in enumerate(mnemonic_fields)
                        if field.lower() not in ("bnd", "notrack", "data16", "addr32")
                    ),
                    len(mnemonic_fields),
                )
                if instruction == len(mnemonic_fields):
                    raise ValueError(f"objdump emitted only instruction prefixes in {label}")
                opcode = mnemonic_fields[instruction].lower()
                decoded_instructions.append(
                    (address, len(decoded), opcode, mnemonic_fields[instruction + 1 :])
                )
                if first_only:
                    break
            if cursor != len(data) and not first_only:
                raise ValueError(f"objdump skipped executable bytes in {label}")
            return decoded_instructions

        def branch_target(opcode: str, operands: list[str], label: str) -> int | None:
            if opcode in ("ljmp", "lcall"):
                raise ValueError(f"unreviewable far branch in {label}")
            direct = (
                opcode.startswith("j")
                or opcode in (
                    "call", "callq", "loop", "loope", "loopne", "loopz", "loopnz",
                    "xbegin",
                )
            )
            if not direct:
                return None
            if not operands:
                raise ValueError(f"objdump omitted branch operand in {label}")
            if operands[0].startswith("0x"):
                try:
                    return int(operands[0].rstrip(","), 16)
                except ValueError as error:
                    raise ValueError(f"objdump emitted invalid branch target in {label}") from error
            if not operands[0].startswith("*"):
                raise ValueError(f"unresolved direct branch in {label}")
            return None

        def record_hit(
            label: str, base: int, address: int, opcode: str, operands: list[str]
        ) -> None:
            mnemonic = next(
                (
                    instruction
                    for field in (opcode, *operands)
                    for instruction in ("rdrand", "rdseed")
                    if field.lower() == instruction
                    or field.lower() in (
                        f"{instruction}w", f"{instruction}l", f"{instruction}q"
                    )
                ),
                None,
            )
            if mnemonic is not None:
                key = (address, mnemonic.upper())
                if key not in seen:
                    hits.append((label, address - base, mnemonic.upper()))
                    seen.add(key)

        def slice_at(address: int) -> int | None:
            return next(
                (
                    index for index, (_, base, data) in enumerate(ordered)
                    if base <= address < base + len(data)
                ),
                None,
            )

        def first_bytes(address: int, index: int) -> bytes:
            _, base, data = ordered[index]
            result = bytearray(data[address - base : address - base + 15])
            next_base = base + len(data)
            next_index = index + 1
            while len(result) < 15 and next_index < len(ordered):
                _, adjoining_base, adjoining = ordered[next_index]
                if adjoining_base != next_base:
                    break
                result.extend(adjoining[: 15 - len(result)])
                next_base += len(adjoining)
                next_index += 1
            return bytes(result)

        for index, (label, base, data) in enumerate(ordered):
            for address, size, opcode, operands in decode(data, base, label):
                parsed += 1
                target = branch_target(opcode, operands, label)
                if target is not None:
                    targets.add(target)
                instructions[address] = (address + size, opcode, target, index)
                record_hit(label, base, address, opcode, operands)

        pending = sorted(target for target in targets if slice_at(target) is not None)
        reached: set[int] = set()
        alternate_starts = 0
        while pending:
            address = pending.pop()
            if address in reached:
                continue
            reached.add(address)
            if address not in instructions:
                index = slice_at(address)
                if index is None:
                    continue
                alternate_starts += 1
                if alternate_starts > 4096:
                    raise ValueError("alternate decoder start budget exceeded")
                label, base, _ = ordered[index]
                decoded = decode(first_bytes(address, index), address, label, first_only=True)
                if not decoded:
                    raise ValueError(f"objdump omitted reachable instruction at 0x{address:x}")
                _, size, opcode, operands = decoded[0]
                target = branch_target(opcode, operands, label)
                record_hit(label, base, address, opcode, operands)
                instructions[address] = (address + size, opcode, target, index)
            end, opcode, target, index = instructions[address]
            label, base, data = ordered[index]
            remaining = base + len(data) - address
            if remaining < 15 and index + 1 < len(ordered):
                suffix = first_bytes(address, index)
                if len(suffix) > remaining:
                    extended = decode(suffix, address, label, first_only=True)
                    if not extended or extended[0][1] > remaining:
                        raise ValueError(
                            f"branch stream crosses executable slice at 0x{address:x}"
                        )
            if target is not None and slice_at(target) is not None:
                pending.append(target)
            if opcode in ("jmp", "jmpq", "jmpl", "ret", "retq", "retl", "retw",
                          "lret", "lretq", "iret", "iretq", "iretd"):
                continue
            if slice_at(end) is not None:
                pending.append(end)
    if parsed == 0:
        raise ValueError("objdump parsed zero executable instructions")
    return hits


def scan(path: Path) -> list[tuple[str, int, str]]:
    """Return entropy-opcode hits with segment-relative offsets."""
    machine, segments = executable_ranges(path)
    if machine == EM_X86_64:
        return x86_hits(segments)
    hits: list[tuple[str, int, str]] = []
    seen: set[tuple[int, str]] = set()
    for segment, address, data in segments:
        for offset, mnemonic in arm64_hits(data):
            key = (address + offset, mnemonic)
            if key not in seen:
                hits.append((segment, offset, mnemonic))
                seen.add(key)
    return hits


def self_test() -> None:
    """Pin both positive detections and neighboring non-entropy encodings."""
    arm = RNDR.to_bytes(4, "little") + RNDRRS.to_bytes(4, "little") + b"\xc0\x03\x5f\xd6"
    if arm64_hits(arm) != [(0, "RNDR"), (4, "RNDRRS")]:
        raise ValueError("AArch64 entropy decoder self-test failed")


def main() -> int:
    self_test()
    if len(sys.argv) < 2:
        print("usage: n6-entropy-scan.py <elf> [<elf> ...]", file=sys.stderr)
        return 2
    rejected = False
    for argument in sys.argv[1:]:
        path = Path(argument)
        try:
            hits = scan(path)
        except (OSError, UnicodeError, ValueError, struct.error) as error:
            print(f"N6_ENTROPY_SCAN_FAIL {path}: {error}", file=sys.stderr)
            return 2
        if hits:
            rejected = True
            print(f"N6_ENTROPY_REJECT {path} hits={len(hits)}")
            for section, offset, mnemonic in hits:
                print(f"  {section}+0x{offset:x} {mnemonic}")
        else:
            print(f"N6_ENTROPY_CLEAN {path} hits=0")
    return 1 if rejected else 0


if __name__ == "__main__":
    raise SystemExit(main())
