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
FALLTHROUGH_FENCE = b"\xcc" * MAX_X86_INSTRUCTION_BYTES
TRAILING_PREFIXES = frozenset(
    ("data16", "data32", "addr16", "addr32", "lock", "rep", "repz", "repnz",
     "cs", "ds", "es", "fs", "gs", "ss", "bnd", "notrack")
)
COUNTER_INSTRUCTIONS = frozenset(("rdtsc", "rdtscp", "rdrand", "rdseed"))
SYMBOL_RESTART_LIMIT = 4096
MIXED_MODE_REACHABLE_LIMIT = 10000


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
        program_fields = (0, 1, 2, 3, 5, 6)
        names_index_offset = 62
    elif data[4] == 1:
        if unpack("<H", data, 18)[0] != 3:
            raise ValueError("expected an x86 ELF")
        program_offset, section_offset = unpack("<II", data, 28)
        program_size, program_count, section_size, section_count = unpack("<HHHH", data, 42)
        program_format = "<IIIIIIII"
        section_format = "<IIIIIIIIII"
        program_fields = (0, 6, 1, 2, 4, 5)
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
        program = (index, *(fields[position] for position in program_fields))
        if program[1] == 1 and program[6] < program[5]:
            raise ValueError(f"PT_LOAD {index} memory size is smaller than its file size")
        programs.append(program)
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
        raise ValueError(f"incomplete instruction at executable section boundary after {name}")
    for address, _length, instruction in decoded:
        if address >= end - (MAX_X86_INSTRUCTION_BYTES - 1) \
                and truncated_instruction(instruction):
            raise ValueError(f"incomplete instruction at executable section boundary after {name}")


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


def direct_branch_target(instruction):
    words = instruction.lower().split()
    while words and (words[0] in TRAILING_PREFIXES or words[0].startswith("rex")):
        words.pop(0)
    if len(words) < 2:
        return None
    mnemonic = words[0]
    if mnemonic.startswith(("ljmp", "lcall")) and not words[1].startswith("*"):
        raise ValueError("unresolved direct far branch")
    if not (mnemonic.startswith(("j", "call", "loop"))
            or mnemonic in ("lcall", "xbegin")):
        return None
    operand = words[1].rstrip(",")
    if not re.fullmatch(r"(?:0x)?[0-9a-f]+", operand):
        return None
    return int(operand, 16)


def scan_mixed_mode_raw_code(data, programs, tag):
    for index, kind, _flags, offset, _address, size, _memory_size in programs:
        if kind != 1 or not size:
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
                    f"{tag}:PT_LOAD {index} at file offset 0x{offset + position:x}"
                )


def require_complete_head(instructions, first, last, mode):
    cursor = first
    for site, length, instruction in instructions:
        if site != cursor:
            raise ValueError(f"incomplete {mode} decompressor head")
        if truncated_instruction(instruction):
            raise ValueError(f"incomplete {mode} decompressor head")
        cursor += length
    if cursor != last:
        raise ValueError(f"incomplete {mode} decompressor head")


def decompressor_head_32(data, sections, path):
    head = [
        (address, offset, size)
        for name, kind, flags, address, offset, size in sections
        if name == ".head.text" and kind == 1 and flags & 2 and flags & 4 and size
    ]
    if len(head) != 1:
        raise ValueError("missing/duplicate executable decompressor head")
    address, offset, size = head[0]
    result = subprocess.run(
        ["objdump", "-d", "-z", "-w", "-M", "i386", "-j", ".head.text", path],
        capture_output=True, text=True, check=False,
    )
    if result.returncode:
        raise ValueError("cannot disassemble 32-bit decompressor head")
    anchors = {}
    for line in result.stdout.splitlines():
        match = re.fullmatch(r"([0-9a-fA-F]+) <(startup_32|startup_64)>:", line)
        if match:
            if match[2] in anchors:
                raise ValueError("duplicate decompressor head mode anchor")
            anchors[match[2]] = int(match[1], 16)
    boundary = anchors.get("startup_64")
    if anchors.get("startup_32") != address or boundary is None \
            or not address < boundary < address + size:
        raise ValueError("decompressor head mode boundary is unavailable")
    decoded = [
        item for item in disassembled_instructions(result.stdout)
        if address <= item[0] < boundary
    ]
    head_bytes = data[offset : offset + boundary - address]
    if len(head_bytes) != boundary - address:
        raise ValueError("incomplete 32-bit decompressor head")
    code_end = address + len(head_bytes.rstrip(b"\0"))
    decoded = [item for item in decoded if item[0] < code_end]
    require_complete_head(decoded, address, code_end, "32-bit")
    if not decoded or instruction_mnemonic(decoded[-1][2]) != "lret":
        raise ValueError("incomplete 32-bit decompressor head")
    return boundary, decoded


def scan_decompressor_32_reachable(data, programs, sections, path, head_32):
    code = [
        (name, address, offset, size)
        for name, kind, flags, address, offset, size in sections
        if kind == 1 and flags & 2 and flags & 4 and size
    ]
    loads = [
        (address, address + memory_size)
        for _index, kind, _flags, _offset, address, _size, memory_size in programs
        if kind == 1 and memory_size
    ]
    head_start = head_32[0][0]
    head_end = head_32[-1][0] + head_32[-1][1]
    head_starts = {site for site, _length, _instruction in head_32}
    pending = []
    for site, _length, instruction in head_32:
        target = direct_branch_target(instruction)
        if target is not None and not head_start <= target < head_end:
            pending.append((target, site, "direct branch"))
    seen = set()
    with tempfile.TemporaryDirectory() as directory:
        raw_files = {}
        for index, (_name, _address, offset, size) in enumerate(code):
            raw = Path(directory) / f"code-{index}.bin"
            raw.write_bytes(data[offset : offset + size])
            raw_files[index] = raw
        while pending:
            site, source, edge = pending.pop()
            if head_start <= site < head_end:
                if site not in head_starts:
                    raise ValueError(
                        f"noncanonical 32-bit decompressor head target from 0x{source:x} to 0x{site:x}"
                    )
                continue
            if site in seen:
                continue
            match = next(
                (
                    (index, row)
                    for index, row in enumerate(code)
                    if row[1] <= site < row[1] + row[3]
                ),
                None,
            )
            if match is None:
                if any(first <= site < last for first, last in loads):
                    raise ValueError(
                        f"32-bit {edge} into loaded non-code from 0x{source:x} to 0x{site:x}"
                    )
                raise ValueError(
                    f"32-bit {edge} outside loaded code from 0x{source:x} to 0x{site:x}"
                )
            if len(seen) >= MIXED_MODE_REACHABLE_LIMIT:
                raise ValueError("32-bit decompressor reachability limit exceeded")
            index, (_name, address, _offset, size) = match
            result = subprocess.run(
                [
                    "objdump", "-D", "-z", "-w", "-b", "binary", "-m", "i386",
                    "-M", "i386", f"--adjust-vma={address}",
                    f"--start-address={site}",
                    f"--stop-address={min(address + size, site + MAX_X86_INSTRUCTION_BYTES)}",
                    str(raw_files[index]),
                ],
                capture_output=True, text=True, check=False,
            )
            decoded = list(disassembled_instructions(result.stdout))
            if result.returncode or not decoded or decoded[0][0] != site:
                raise ValueError(f"cannot decode reachable 32-bit decompressor code at 0x{site:x}")
            _site, length, instruction = decoded[0]
            if length > MAX_X86_INSTRUCTION_BYTES or site + length > address + size \
                    or truncated_instruction(instruction):
                raise ValueError(f"incomplete reachable 32-bit decompressor code at 0x{site:x}")
            seen.add(site)
            mnemonic = instruction_mnemonic(instruction)
            try:
                target = direct_branch_target(instruction)
            except ValueError as error:
                raise ValueError(
                    f"unresolved reachable 32-bit decompressor branch at 0x{site:x}: {error}"
                ) from error
            if target is not None:
                pending.append((target, site, "direct branch"))
            elif mnemonic.startswith(("j", "call", "loop")) \
                    or mnemonic in ("lcall", "ljmp", "xbegin"):
                raise ValueError(
                    f"unresolved reachable 32-bit decompressor branch at 0x{site:x}"
                )
            if mnemonic.startswith(("ret", "lret", "iret", "jmp", "ljmp")):
                continue
            if mnemonic in ("int", "int3", "ud2", "syscall", "sysenter", "sysret"):
                raise ValueError(
                    f"unresolved reachable 32-bit decompressor transfer at 0x{site:x}"
                )
            pending.append((site + length, site, "fallthrough"))


def scan_symbol_restarts(data, programs, sections, tag, path):
    if tag == "setup":
        scan_mixed_mode_raw_code(data, programs, tag)
        return
    loads = [
        (index, address, address + memory_size)
        for index, kind, _flags, _offset, address, _size, memory_size in programs
        if kind == 1 and memory_size
    ]
    code_ranges = [
        (address, address + size)
        for _name, kind, flags, address, _offset, size in sections
        if kind == 1 and flags & 2 and flags & 4 and size
    ]
    names = {
        name: (address, offset, size)
        for name, kind, flags, address, offset, size in sections
        if flags & 4 and kind == 1 and size
        and (tag == "vmlinux" or name in (".text", ".head.text"))
    }
    if not names and tag != "decompressor":
        return
    head_boundary, head_32 = (
        decompressor_head_32(data, sections, path)
        if tag == "decompressor" and any(name == ".head.text" for name, *_rest in sections)
        else (None, [])
    )
    with tempfile.TemporaryDirectory() as directory:
        for mode in ("amd64", "intel64"):
            result = subprocess.run(
                ["objdump", "-d", "-z", "-w", "-M", mode, path],
                capture_output=True, text=True, check=False,
            )
            if result.returncode:
                raise ValueError(f"cannot disassemble {tag} symbol boundaries in {mode}")
            section = None
            decoded = {name: list(head_32) if name == ".head.text" else [] for name in names}
            symbols = {name: set() for name in names}
            starts = {name: bytearray(size) for name, (_address, _offset, size) in names.items()}
            if head_boundary is not None:
                address, _offset, _size = names[".head.text"]
                for site, _length, _instruction in head_32:
                    starts[".head.text"][site - address] = 1
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
                for site, length, instruction in disassembled_instructions(line):
                    address, _offset, size = names[section]
                    if head_boundary is not None and section == ".head.text" \
                            and site < head_boundary:
                        continue
                    if address <= site < address + size:
                        starts[section][site - address] = 1
                    decoded[section].append((site, length, instruction))

            if head_boundary is not None:
                address, _offset, size = names[".head.text"]
                require_complete_head(
                    [item for item in decoded[".head.text"] if item[0] >= head_boundary],
                    head_boundary, address + size, mode,
                )

            for source, instructions in decoded.items():
                for site, _length, instruction in instructions:
                    target = direct_branch_target(instruction)
                    if target is None:
                        continue
                    for name, (address, _offset, size) in names.items():
                        if address <= target < address + size:
                            if not starts[name][target - address]:
                                raise ValueError(
                                    f"noncanonical direct branch target in {tag}:{source}:{mode} "
                                    f"from 0x{site:x} to {name}:0x{target:x}"
                                )
                            break
                    else:
                        if any(first <= target < last for _index, first, last in loads) \
                                and not any(first <= target < last for first, last in code_ranges):
                            raise ValueError(
                                f"direct branch into loaded non-code in {tag}:{source}:{mode} "
                                f"from 0x{site:x} to 0x{target:x}"
                            )

            for name, (address, offset, size) in names.items():
                if tag == "decompressor" and name == ".head.text":
                    continue
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
    if tag == "decompressor" and head_32:
        scan_decompressor_32_reachable(data, programs, sections, path, head_32)


def counter_encoding(segment, position):
    if position + 1 >= len(segment) or segment[position] != 0x0F:
        return None
    if segment[position + 1] == 0x31:
        return "RDTSC", 2
    if position + 2 >= len(segment):
        return None
    if segment[position + 1 : position + 3] == b"\x01\xf9":
        return "RDTSCP", 3
    if segment[position + 1] == 0xC7:
        reg = (segment[position + 2] >> 3) & 7
        if reg in (6, 7):
            return ("RDRAND" if reg == 6 else "RDSEED"), 3
    return None


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
    loads = sorted(
        (address, address + memory_size, index)
        for index, kind, _flags, _offset, address, _size, memory_size in programs
        if kind == 1 and memory_size
    )
    for left, right in zip(loads, loads[1:]):
        if right[0] < left[1]:
            raise ValueError(f"overlapping PT_LOAD memory ranges {left[2]} and {right[2]}")
    seen_data = set()
    saw_load = False
    missing_fences = []
    reviewed_data_hit = None
    load_edges = []
    for index, kind, _flags, offset, address, size, _memory_size in programs:
        if kind != 1:
            continue
        saw_load = True
        if not size:
            continue
        if offset + size > len(data):
            raise ValueError(f"PT_LOAD {index} extends beyond the file")
        claimed = bytearray(size)
        executable = bytearray(size)
        reviewed_data = bytearray(size)
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
            elif tag == "decompressor" and name != ".rodata..compressed":
                reviewed_data[begin : begin + section_size] = b"\x01" * section_size

        adjacent_code = {
            boundary
            for boundary in boundaries
            if 0 < boundary < size and executable[boundary - 1] and executable[boundary]
        }
        nearby_code = adjacent_code | {
            section_offset + section_size - offset
            for _name, _address, section_offset, section_size in executable_sections
            if section_offset + section_size - offset < size
            and any(executable[
                section_offset + section_size - offset :
                min(size, section_offset + section_size - offset + MAX_X86_INSTRUCTION_BYTES - 1)
            ])
        }

        segment = data[offset : offset + size]
        load_edges.append({
            "start": address,
            "end": address + size,
            "offset": offset,
            "index": index,
            "head": (segment[:2], claimed[:2], executable[:2], reviewed_data[:2]),
            "tail": (segment[-2:], claimed[-2:], executable[-2:], reviewed_data[-2:]),
        })
        for position in range(size - 1):
            hit = counter_encoding(segment, position)
            if hit is None:
                continue
            opcode, length = hit
            if not all(claimed[position : position + length]):
                return opcode, offset + position, index, "unclaimed"
            if reviewed_data_hit is None and any(reviewed_data[position : position + length]):
                reviewed_data_hit = opcode, offset + position, index, "reviewed-data"
            if executable[position] and any(
                position < boundary < position + length for boundary in boundaries
            ):
                return opcode, offset + position, index, "cross-section"
            if all(executable[position : position + length]) and any(
                boundary <= position < boundary + MAX_X86_INSTRUCTION_BYTES
                for boundary in nearby_code
            ):
                return opcode, offset + position, index, "boundary-desync"
        for boundary in nearby_code:
            if sum(section[2] + section[3] == offset + boundary
                   for section in executable_sections) != 1:
                raise ValueError(f"ambiguous nearby executable section boundary in PT_LOAD {index}")
        for section in executable_sections:
            boundary = section[2] + section[3] - offset
            if boundary in nearby_code:
                complete_section_tail(data, path, *section)
        for name, section_address, section_offset, section_size in executable_sections:
            boundary = section_offset + section_size - offset
            if boundary < size and executable[boundary]:
                continue
            if boundary == size and not any(
                other_index != index and other_kind == 1 and other_memory_size
                and other_address + other_memory_size > section_address + section_size
                for other_index, other_kind, _other_flags, _other_offset, other_address, _other_size, other_memory_size in programs
            ):
                continue
            if boundary < MAX_X86_INSTRUCTION_BYTES or segment[
                boundary - MAX_X86_INSTRUCTION_BYTES : boundary
            ] != FALLTHROUGH_FENCE:
                missing_fences.append((name, index))
    if not saw_load:
        raise ValueError("no PT_LOAD segments to scan")
    if missing_fences:
        name, index = missing_fences[0]
        raise ValueError(
            f"executable section {name} in PT_LOAD {index} can fall through into loaded bytes"
        )
    ordered_edges = sorted(load_edges, key=lambda edge: edge["start"])
    for left, right in zip(ordered_edges, ordered_edges[1:]):
        if left["end"] != right["start"]:
            continue
        combined = tuple(a + b for a, b in zip(left["tail"], right["head"]))
        code, claimed, executable, reviewed_data = combined
        boundary = len(left["tail"][0])
        for position in range(max(0, boundary - 2), boundary):
            hit = counter_encoding(code, position)
            if hit is None:
                continue
            opcode, length = hit
            if position + length <= boundary:
                continue
            file_offset = left["offset"] + left["end"] - left["start"] - boundary + position
            if not all(claimed[position : position + length]):
                return opcode, file_offset, left["index"], "unclaimed"
            if reviewed_data_hit is None and any(reviewed_data[position : position + length]):
                reviewed_data_hit = opcode, file_offset, left["index"], "reviewed-data"
            if any(executable[position : position + length]):
                return opcode, file_offset, left["index"], "cross-load"
    scan_symbol_restarts(data, programs, sections, tag, path)
    if reviewed_data_hit is not None:
        return reviewed_data_hit
    if tag == "decompressor":
        heads = [
            (address, offset, size) for name, kind, flags, address, offset, size in sections
            if name == ".head.text" and kind == 1 and flags & 2 and flags & 4 and size
        ]
        if len(heads) != 1:
            raise ValueError("missing/duplicate executable decompressor head")
        head_address, head_offset, head_size = heads[0]
        if sum(
            kind == 1 and offset <= head_offset
            and head_offset + head_size <= offset + size
            and head_address - address == head_offset - offset
            for _index, kind, _flags, offset, address, size, _memory_size in programs
        ) != 1:
            raise ValueError("decompressor head is not in one loaded segment")
        if head_address != min(
            address for _name, kind, flags, address, _offset, size in sections
            if flags & 2 and kind != 8 and size
        ):
            raise ValueError("decompressor head is not the first loaded section")
    return reviewed_data_hit


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
