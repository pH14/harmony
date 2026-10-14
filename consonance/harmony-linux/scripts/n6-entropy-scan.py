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
PT_LOAD = 1
PF_X = 1
EM_X86_64 = 62
EM_AARCH64 = 183
RNDR = 0xD53B2400
RNDRRS = 0xD53B2420


def executable_segments(path: Path) -> tuple[int, list[tuple[str, int, bytes]]]:
    """Return the file-backed bytes mapped executable by the ELF loader."""
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
        _section_offset,
        _flags,
        elf_header_size,
        program_entry_size,
        program_count,
        _section_entry_size,
        _section_count,
        _string_index,
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
        found.append((f"<exec-segment {index}>", address, data[offset : offset + size]))
    if not found:
        raise ValueError("no executable load segments")
    return machine, found


def arm64_hits(data: bytes) -> list[tuple[int, str]]:
    """Find RNDR/RNDRRS MRS words, ignoring the destination register."""
    if len(data) % 4 != 0:
        raise ValueError("unaligned AArch64 executable segment")
    hits: list[tuple[int, str]] = []
    for offset in range(0, len(data), 4):
        word = int.from_bytes(data[offset : offset + 4], "little") & ~0x1F
        if word == RNDR:
            hits.append((offset, "RNDR"))
        elif word == RNDRRS:
            hits.append((offset, "RNDRRS"))
    return hits


def x86_hits(segments: list[tuple[str, int, bytes]]) -> list[tuple[str, int, str]]:
    """Decode each executable load segment without consulting section headers."""
    hits: list[tuple[str, int, str]] = []
    parsed = 0
    with tempfile.TemporaryDirectory() as temporary:
        segment_path = Path(temporary) / "segment"
        for label, base, data in segments:
            segment_path.write_bytes(data)
            try:
                output = subprocess.run(
                    [
                        "objdump", "-D", "-b", "binary", "-m", "i386:x86-64",
                        f"--adjust-vma=0x{base:x}", str(segment_path),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout
            except (OSError, subprocess.CalledProcessError) as error:
                raise ValueError(f"objdump failed: {error}") from error
            for line in output.splitlines():
                if ":" not in line:
                    continue
                address_text, remainder = line.split(":", 1)
                try:
                    address = int(address_text.strip(), 16)
                except ValueError:
                    continue
                if not base <= address < base + len(data):
                    raise ValueError(f"objdump address outside {label}")
                fields = remainder.split("\t")
                if len(fields) < 2:
                    continue
                mnemonic_fields = " ".join(fields[1:]).strip().split()
                if not mnemonic_fields:
                    continue
                parsed += 1
                mnemonic = next(
                    (
                        instruction
                        for field in mnemonic_fields
                        for instruction in ("rdrand", "rdseed")
                        if field.lower() == instruction
                        or field.lower() in (
                            f"{instruction}w", f"{instruction}l", f"{instruction}q"
                        )
                    ),
                    None,
                )
                if mnemonic is not None:
                    hits.append((label, address - base, mnemonic.upper()))
    if parsed == 0:
        raise ValueError("objdump parsed zero executable instructions")
    return hits


def scan(path: Path) -> list[tuple[str, int, str]]:
    """Return entropy-opcode hits with segment-relative offsets."""
    machine, segments = executable_segments(path)
    if machine == EM_X86_64:
        return x86_hits(segments)
    return [
        (segment, offset, mnemonic)
        for segment, _address, data in segments
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
