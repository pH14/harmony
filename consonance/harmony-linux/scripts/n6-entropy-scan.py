#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Reject untrusted entropy instructions from executable ELF sections."""

from __future__ import annotations

import struct
import subprocess
import sys
from pathlib import Path


ELF_HEADER = "<16sHHIQQQIHHHHHH"
SECTION_HEADER = "<IIQQQQIIQQ"
SHF_EXECINSTR = 0x4
SHT_NOBITS = 8
EM_X86_64 = 62
EM_AARCH64 = 183
RNDR = 0xD53B2400
RNDRRS = 0xD53B2420


def executable_sections(path: Path) -> tuple[int, list[tuple[str, int, bytes]]]:
    """Return every executable section after strict ELF bounds checks."""
    data = path.read_bytes()
    header_size = struct.calcsize(ELF_HEADER)
    section_size = struct.calcsize(SECTION_HEADER)
    if len(data) < header_size:
        raise ValueError("truncated ELF header")
    (
        ident,
        _kind,
        machine,
        version,
        _entry,
        _program_offset,
        section_offset,
        _flags,
        elf_header_size,
        _program_entry_size,
        _program_count,
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
    if section_count == 0 or string_index >= section_count:
        raise ValueError("missing/extended section table unsupported")
    if section_entry_size < section_size:
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
    for index in range(section_count):
        name_offset, kind, flags, address, offset, size, *_rest = section(index)
        if flags & SHF_EXECINSTR == 0 or size == 0:
            continue
        name = name_at(name_offset)
        if kind == SHT_NOBITS:
            raise ValueError(f"executable NOBITS section {name}")
        if offset > len(data) or size > len(data) - offset:
            raise ValueError(f"executable section {name} outside file")
        found.append((name, address, data[offset : offset + size]))
    if not found:
        raise ValueError("no executable sections")
    return machine, found


def arm64_hits(data: bytes) -> list[tuple[int, str]]:
    """Find RNDR/RNDRRS MRS words, ignoring the destination register."""
    if len(data) % 4 != 0:
        raise ValueError("unaligned AArch64 executable section")
    hits: list[tuple[int, str]] = []
    for offset in range(0, len(data), 4):
        word = int.from_bytes(data[offset : offset + 4], "little") & ~0x1F
        if word == RNDR:
            hits.append((offset, "RNDR"))
        elif word == RNDRRS:
            hits.append((offset, "RNDRRS"))
    return hits


def x86_hits(path: Path, sections: list[tuple[str, int, bytes]]) -> list[tuple[str, int, str]]:
    """Find decoded RDRAND/RDSEED instructions at real x86 boundaries."""
    bases = {name: address for name, address, _data in sections}
    try:
        output = subprocess.run(
            ["objdump", "-d", str(path)],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as error:
        raise ValueError(f"objdump failed: {error}") from error

    current: str | None = None
    hits: list[tuple[str, int, str]] = []
    parsed = 0
    for line in output.splitlines():
        if line.startswith("Disassembly of section ") and line.endswith(":"):
            current = line.removeprefix("Disassembly of section ")[:-1]
            continue
        if current not in bases or ":" not in line:
            continue
        address_text, remainder = line.split(":", 1)
        try:
            address = int(address_text.strip(), 16)
        except ValueError:
            continue
        fields = remainder.split("\t")
        if len(fields) < 2:
            continue
        mnemonic_fields = " ".join(fields[1:]).strip().split()
        if not mnemonic_fields:
            continue
        parsed += 1
        mnemonic = next(
            (
                base
                for field in mnemonic_fields
                for base in ("rdrand", "rdseed")
                if field.lower() == base
                or field.lower() in (f"{base}w", f"{base}l", f"{base}q")
            ),
            None,
        )
        if mnemonic is not None:
            offset = address - bases[current]
            if offset < 0:
                raise ValueError(f"objdump address before section base for {current}")
            hits.append((current, offset, mnemonic.upper()))
    if parsed == 0:
        raise ValueError("objdump parsed zero executable instructions")
    return hits


def scan(path: Path) -> list[tuple[str, int, str]]:
    """Return entropy-opcode hits with section-relative offsets."""
    machine, sections = executable_sections(path)
    if machine == EM_X86_64:
        return x86_hits(path, sections)
    return [
        (section, offset, mnemonic)
        for section, _address, data in sections
        for offset, mnemonic in arm64_hits(data)
    ]


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
