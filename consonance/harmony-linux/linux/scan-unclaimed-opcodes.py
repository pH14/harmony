#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import re
import struct
import subprocess
import sys
import tempfile
from bisect import bisect_left
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
TRAILING_PREFIXES = frozenset(
    ("data16", "data32", "addr16", "addr32", "lock", "rep", "repz", "repnz",
     "cs", "ds", "es", "fs", "gs", "ss", "bnd", "notrack")
)
COUNTER_INSTRUCTIONS = frozenset(("rdtsc", "rdtscp", "rdrand", "rdseed"))
SYMBOL_RESTART_LIMIT = 4096


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


def check_complete_tail(decoded, end, name):
    last = decoded[-1] if decoded else None
    if last is None or last[0] + last[1] != end:
        raise ValueError(f"incomplete instruction at adjacent code boundary after {name}")
    for address, _length, instruction in decoded:
        if address >= end - (MAX_X86_INSTRUCTION_BYTES - 1) \
                and truncated_instruction(instruction):
            raise ValueError(f"incomplete instruction at adjacent code boundary after {name}")


def complete_section_tail(data, path, name, address, offset, size):
    with tempfile.TemporaryDirectory() as directory:
        raw_section = Path(directory) / "section.bin"
        raw_section.write_bytes(data[offset : offset + size])
        for mode in ("amd64", "intel64"):
            try:
                result = subprocess.run(
                    ["objdump", "-D", "-z", "-w", "-b", "binary", "-m", "i386:x86-64",
                     "-M", mode, str(raw_section)],
                    capture_output=True, text=True, check=False,
                )
            except OSError as error:
                raise ValueError(
                    f"cannot disassemble adjacent code section {name} in {mode}: {error}"
                ) from error
            if result.returncode:
                raise ValueError(
                    f"cannot disassemble adjacent code section {name} in {mode}: "
                    f"{result.stderr.strip()}"
                )
            check_complete_tail(list(disassembled_instructions(result.stdout)), size, name)
            symbolic = subprocess.run(
                ["objdump", "-d", "-z", "-w", "-M", mode, "-j", name, path],
                capture_output=True, text=True, check=False,
            )
            if symbolic.returncode:
                raise ValueError(f"cannot disassemble adjacent code section {name} in {mode}")
            check_complete_tail(
                list(disassembled_instructions(symbolic.stdout)), address + size, name
            )


def disassembled_instructions(output):
    for line in output.splitlines():
        fields = line.split("\t")
        if len(fields) < 3 or not re.fullmatch(r"[0-9a-fA-F]+:", fields[0].strip()):
            continue
        raw = fields[1].split()
        if not raw or any(re.fullmatch(r"[0-9a-fA-F]{2}", byte) is None for byte in raw):
            continue
        yield int(fields[0].strip()[:-1], 16), len(raw), fields[2].strip()


def instruction_mnemonic(instruction):
    for word in instruction.lower().split():
        if word in TRAILING_PREFIXES or word.startswith("rex"):
            continue
        return word
    return ""


def truncated_instruction(instruction):
    mnemonic = instruction_mnemonic(instruction)
    return mnemonic.startswith((".byte", "(bad)")) or not mnemonic


def scan_mixed_mode_raw_code(data, sections, tag):
    for name, kind, flags, _address, offset, size in sections:
        if kind != 1 or not flags & 4 or not size:
            continue
        code = data[offset : offset + size]
        for position in range(len(code) - 1):
            if code[position] != 0x0F:
                continue
            tail = code[position + 1 : position + 3]
            opcode = None
            if tail[:1] == b"\x31":
                opcode = "rdtsc"
            elif tail == b"\x01\xf9":
                opcode = "rdtscp"
            elif len(tail) == 2 and tail[0] == 0xC7 and (tail[1] >> 3) & 7 in (6, 7):
                opcode = "rdrand/rdseed"
            if opcode:
                raise ValueError(
                    f"mixed-mode raw counter instruction {opcode} in "
                    f"{tag}:{name} at file offset 0x{offset + position:x}"
                )


def scan_symbol_restarts(data, sections, tag, path):
    if tag == "setup":
        scan_mixed_mode_raw_code(data, sections, tag)
        return
    names = {
        name: (address, offset, size)
        for name, kind, flags, address, offset, size in sections
        if flags & 4 and kind == 1 and size and (tag == "vmlinux" or name == ".text")
    }
    if not names:
        return
    with tempfile.TemporaryDirectory() as directory:
        for mode in ("amd64", "intel64"):
            result = subprocess.run(
                ["objdump", "-d", "-z", "-w", "-M", mode, path],
                capture_output=True, text=True, check=False,
            )
            if result.returncode:
                raise ValueError(f"cannot disassemble {tag} symbol boundaries in {mode}")
            section = None
            decoded = {name: [] for name in names}
            symbols = {name: set() for name in names}
            for line in result.stdout.splitlines():
                match = re.fullmatch(r"Disassembly of section (.+):", line)
                if match:
                    section = match.group(1)
                    continue
                if section not in names:
                    continue
                match = re.fullmatch(r"([0-9a-fA-F]+) <.*>:", line)
                if match:
                    symbols[section].add(int(match.group(1), 16))
                    continue
                decoded[section].extend(disassembled_instructions(line))

            for name, (address, offset, size) in names.items():
                canonical = {site: (length, instruction) for site, length, instruction in decoded[name]}
                markers = sorted(
                    site for site, _length, instruction in decoded[name]
                    if truncated_instruction(instruction)
                )
                starts = set()
                for symbol in symbols[name]:
                    first = bisect_left(markers, symbol - (MAX_X86_INSTRUCTION_BYTES - 1))
                    last = bisect_left(markers, symbol)
                    starts.update(markers[first:last])
                if not starts:
                    continue
                raw_section = Path(directory) / f"section-{mode}-{offset:x}.bin"
                raw_section.write_bytes(data[offset : offset + size])
                for start in sorted(starts):
                    relative = start - address
                    if relative < 0 or relative >= size:
                        raise ValueError(f"symbol restart outside {tag} section {name}")
                    stop = min(size, relative + SYMBOL_RESTART_LIMIT)
                    raw = subprocess.run(
                        ["objdump", "-D", "-z", "-w", "-b", "binary", "-m", "i386:x86-64",
                         "-M", mode, f"--start-address={relative}", f"--stop-address={stop}",
                         str(raw_section)],
                        capture_output=True, text=True, check=False,
                    )
                    if raw.returncode:
                        raise ValueError(f"cannot decode {tag} symbol restart in {name} ({mode})")
                    rejoined = False
                    for raw_address, length, instruction in disassembled_instructions(raw.stdout):
                        site = address + raw_address
                        mnemonic = instruction_mnemonic(instruction)
                        if mnemonic in COUNTER_INSTRUCTIONS:
                            raise ValueError(
                                f"symbol-restart counter instruction {mnemonic} in "
                                f"{tag}:{name}:{mode} at 0x{site:x}"
                            )
                        ordinary = canonical.get(site)
                        if site > start and ordinary is not None and ordinary[0] == length \
                                and not truncated_instruction(ordinary[1]):
                            rejoined = True
                            break
                        if mnemonic.startswith(("j", "loop", "call", "lcall", "ljmp")) \
                                or mnemonic == "xbegin":
                            raise ValueError(
                                f"unresolved branch in {tag}:{name}:{mode} symbol restart at 0x{site:x}"
                            )
                        if mnemonic.startswith("ret") or mnemonic in ("iret", "iretq"):
                            rejoined = True
                            break
                    if not rejoined:
                        raise ValueError(
                            f"unresolved {tag}:{name}:{mode} symbol restart at 0x{start:x}"
                        )


def unclaimed_opcode(data, programs, sections, tag, path):
    allowed_data = KNOWN_DATA.get(tag)
    if allowed_data is None:
        raise ValueError(f"unknown x86 kernel artifact tag: {tag}")
    code_names = [
        name for name, kind, flags, _address, _offset, size in sections
        if kind == 1 and flags & 4 and size
    ]
    if len(code_names) != len(set(code_names)):
        raise ValueError("duplicate executable section name")
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
        executable_sections = []
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
                executable_sections.append((name, section_address, section_offset, section_size))

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
        for boundary in adjacent_code:
            left = [
                section for section in executable_sections
                if section[2] + section[3] == offset + boundary
            ]
            if len(left) != 1:
                raise ValueError(f"ambiguous adjacent executable section boundary in PT_LOAD {index}")
            complete_section_tail(data, path, *left[0])
    if not saw_load:
        raise ValueError("no PT_LOAD segments to scan")
    scan_symbol_restarts(data, sections, tag, path)
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
            hit = unclaimed_opcode(data, *headers(data), tag, path)
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
