#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import struct
import sys
from pathlib import Path


def unpack(fmt, data, offset):
    if offset < 0 or offset + struct.calcsize(fmt) > len(data):
        raise ValueError("ELF table extends beyond the file")
    return struct.unpack_from(fmt, data, offset)


def headers(data):
    if data[:4] != b"\x7fELF" or len(data) < 52 or data[5:7] != b"\x01\x01":
        raise ValueError("expected a little-endian ELF")
    if data[4] == 2:
        if unpack("<H", data, 18)[0] != 62:
            raise ValueError("expected an x86-64 ELF")
        program_offset, section_offset = unpack("<QQ", data, 32)
        program_size, program_count, section_size, section_count = unpack("<HHHH", data, 54)
        program_format = "<IIQQQQQQ"
        section_format = "<IIQQQQIIQQ"
        program_fields = (0, 1, 2, 3, 5)
    elif data[4] == 1:
        if unpack("<H", data, 18)[0] != 3:
            raise ValueError("expected an x86 ELF")
        program_offset, section_offset = unpack("<II", data, 28)
        program_size, program_count, section_size, section_count = unpack("<HHHH", data, 42)
        program_format = "<IIIIIIII"
        section_format = "<IIIIIIIIII"
        program_fields = (0, 6, 1, 2, 4)
    else:
        raise ValueError("unsupported ELF class")
    if not program_count or not section_count:
        raise ValueError("missing ELF program or section headers")
    if program_size < struct.calcsize(program_format) or section_size < struct.calcsize(section_format):
        raise ValueError("short ELF table entries")
    if program_offset + program_count * program_size > len(data):
        raise ValueError("program headers extend beyond the file")
    if section_offset + section_count * section_size > len(data):
        raise ValueError("section headers extend beyond the file")

    programs = []
    for index in range(program_count):
        fields = unpack(program_format, data, program_offset + index * program_size)
        programs.append((index, *(fields[position] for position in program_fields)))
    sections = []
    for index in range(section_count):
        fields = unpack(section_format, data, section_offset + index * section_size)
        sections.append((fields[1], fields[2], fields[3], fields[4], fields[5]))
    return programs, sections


def unclaimed_opcode(data, programs, sections):
    saw_load = False
    for index, kind, _flags, offset, address, size in programs:
        if kind != 1:
            continue
        saw_load = True
        if not size:
            continue
        if offset + size > len(data):
            raise ValueError(f"PT_LOAD {index} extends beyond the file")
        claimed = bytearray(size)
        for section_kind, section_flags, section_address, section_offset, section_size in sections:
            if not section_flags & 2 or section_kind == 8 or not section_size:
                continue
            if section_offset + section_size > len(data):
                raise ValueError("allocated section extends beyond the file")
            if section_offset >= offset + size or section_offset + section_size <= offset:
                continue
            if section_offset < offset or section_offset + section_size > offset + size:
                raise ValueError(f"allocated section crosses PT_LOAD {index} boundary")
            if section_address - address != section_offset - offset:
                raise ValueError(f"allocated section does not map to PT_LOAD {index} address")
            begin = section_offset - offset
            claimed[begin : begin + section_size] = b"\x01" * section_size

        segment = data[offset : offset + size]
        for position in range(size - 1):
            if segment[position] != 0x0F:
                continue
            opcode = None
            length = 0
            if segment[position + 1] == 0x31:
                opcode, length = "RDTSC", 2
            elif position + 2 < size and segment[position + 1 : position + 3] == b"\x01\xf9":
                opcode, length = "RDTSCP", 3
            elif position + 2 < size and segment[position + 1] == 0xC7:
                reg = (segment[position + 2] >> 3) & 7
                if reg in (6, 7):
                    opcode, length = ("RDRAND" if reg == 6 else "RDSEED"), 3
            if opcode and not all(claimed[position : position + length]):
                return opcode, offset + position, index
    if not saw_load:
        raise ValueError("no PT_LOAD segments to scan")
    return None


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: scan-unclaimed-opcodes.py tag=path [tag=path ...]")
    for artifact in sys.argv[1:]:
        tag, separator, path = artifact.partition("=")
        if not separator or not tag or not path:
            raise SystemExit(f"invalid artifact argument: {artifact}")
        try:
            data = Path(path).read_bytes()
            hit = unclaimed_opcode(data, *headers(data))
        except (OSError, ValueError, struct.error) as error:
            raise SystemExit(f"FAIL: {tag} ({path}) ELF coverage error: {error}") from error
        if hit:
            opcode, offset, segment = hit
            raise SystemExit(
                f"FAIL: {tag} ({path}) has unclaimed {opcode} bytes at file offset "
                f"0x{offset:x} in PT_LOAD {segment}"
            )


if __name__ == "__main__":
    main()
