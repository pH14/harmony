#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import struct
import sys
from pathlib import Path


KNOWN_DATA = {
    "vmlinux": frozenset(
        (
            ".rodata",
            ".pci_fixup",
            "__param",
            "__modver",
            "__ex_table",
            ".notes",
            ".data",
            "__bug_table",
            ".orc_header",
            ".orc_unwind_ip",
            ".orc_unwind",
            ".init.data",
            ".x86_cpu_dev.init",
            ".retpoline_sites",
            ".return_sites",
            ".call_sites",
            ".altinstructions",
            ".apicdrivers",
            ".data..percpu",
            "runtime_shift_d_hash_shift",
            "runtime_ptr_dentry_hashtable",
            "runtime_ptr_USER_PTR_MAX",
            ".smp_locks",
        )
    ),
    "setup": frozenset((".header", ".initdata", ".rodata", ".videocards", ".data", ".signature")),
    "decompressor": frozenset((".rodata..compressed", ".rodata", ".data")),
}
MAX_X86_INSTRUCTION_BYTES = 15


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
        names_index_offset = 62
    elif data[4] == 1:
        if unpack("<H", data, 18)[0] != 3:
            raise ValueError("expected an x86 ELF")
        program_offset, section_offset = unpack("<II", data, 28)
        program_size, program_count, section_size, section_count = unpack("<HHHH", data, 42)
        program_format = "<IIIIIIII"
        section_format = "<IIIIIIIIII"
        program_fields = (0, 6, 1, 2, 4)
        names_index_offset = 50
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
    names_index = unpack("<H", data, names_index_offset)[0]
    if names_index >= section_count:
        raise ValueError("section-name table is missing")
    names_header = unpack(section_format, data, section_offset + names_index * section_size)
    if names_header[1] != 3 or names_header[4] + names_header[5] > len(data):
        raise ValueError("invalid section-name table")
    names = data[names_header[4] : names_header[4] + names_header[5]]
    sections = []
    for index in range(section_count):
        fields = unpack(section_format, data, section_offset + index * section_size)
        if fields[0] >= len(names):
            raise ValueError("section name is outside the name table")
        end = names.find(b"\0", fields[0])
        if end < 0:
            raise ValueError("unterminated section name")
        name = names[fields[0] : end].decode("ascii")
        sections.append((name, fields[1], fields[2], fields[3], fields[4], fields[5]))
    return programs, sections


def unclaimed_opcode(data, programs, sections, tag):
    allowed_data = KNOWN_DATA.get(tag)
    if allowed_data is None:
        raise ValueError(f"unknown x86 kernel artifact tag: {tag}")
    seen_data = set()
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
        executable = bytearray(size)
        boundaries = set()
        for name, section_kind, section_flags, section_address, section_offset, section_size in sections:
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
            if not section_flags & 4:
                if name not in allowed_data:
                    raise ValueError(f"unreviewed allocated data section {name!r} in PT_LOAD {index}")
                if name in seen_data:
                    raise ValueError(f"duplicate allocated data section {name!r}")
                seen_data.add(name)
            begin = section_offset - offset
            claimed[begin : begin + section_size] = b"\x01" * section_size
            if section_flags & 4:
                executable[begin : begin + section_size] = b"\x01" * section_size
                boundaries.update((begin, begin + section_size))

        adjacent_code = {
            boundary
            for boundary in boundaries
            if 0 < boundary < size and executable[boundary - 1] and executable[boundary]
        }

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
            if opcode:
                if not all(claimed[position : position + length]):
                    return opcode, offset + position, index, "unclaimed"
                if executable[position] and any(
                    position < boundary < position + length for boundary in boundaries
                ):
                    return opcode, offset + position, index, "cross-section"
                if all(executable[position : position + length]) and any(
                    boundary <= position < boundary + MAX_X86_INSTRUCTION_BYTES
                    for boundary in adjacent_code
                ):
                    return opcode, offset + position, index, "boundary-desync"
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
            hit = unclaimed_opcode(data, *headers(data), tag)
        except (OSError, ValueError, struct.error) as error:
            raise SystemExit(f"FAIL: {tag} ({path}) ELF coverage error: {error}") from error
        if hit:
            opcode, offset, segment, category = hit
            raise SystemExit(
                f"FAIL: {tag} ({path}) has {category} {opcode} bytes at file offset "
                f"0x{offset:x} in PT_LOAD {segment}"
            )


if __name__ == "__main__":
    main()
