#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-4(c) level-2: the executable-page exclusive-opcode scan. This is the primitive a W^X
# rescan-on-exec would run to enforce "no LL/SC in the guest": scan the raw instruction words of
# an aarch64 image's executable sections for the load/store-EXCLUSIVE family and reject any hit.
#
# The LL/SC exclusive family (monitor-based): LDXR/LDXRB/LDXRH/LDAXR.., STXR/STLXR..,
# LDXP/STXP/LDAXP/STLXP. These and LSE CASP share the broad encoding class
# `(insn & 0x3f800000) == 0x08000000`; o1/size distinguish monitor-based pairs from CASP:
# o1=0 is always LL/SC; o1=1 is LL/SC only for word/dword element sizes (size>=2). This deliberately
# EXCLUDES acquire/release non-exclusives LDAR/STLR and LSE CAS/CASP: they carry no reservation
# monitor and are deterministic, so an LSE-only build is clean by construction.
#
# The scan operates on RAW bytes (what a page rescan sees). It then SELF-VALIDATES against
# `objdump -d` wherever objdump renders an instruction: the ELF word, decoder verdict, and
# mnemonic must agree. Mapping-symbol data remains covered by the authoritative byte walk even
# though the disassembler intentionally does not render it as an instruction.
import re
import struct
import subprocess
import sys
from pathlib import Path

EXCL_MASK = 0x3F800000
EXCL_MATCH = 0x08000000


def is_llsc_exclusive(word: int) -> bool:
    if (word & EXCL_MASK) != EXCL_MATCH:
        return False
    o1 = (word >> 21) & 1
    size = (word >> 30) & 0b11
    return o1 == 0 or size >= 0b10


def self_test_mask() -> None:
    # Real words from the retained AA-4 payload/kernel investigations. CASP shares the broad
    # class prefix with LDXP/STXP, so pinning both sides prevents the exact false-positive that
    # would make an actually LSE-only kernel impossible to publish.
    cases = {
        0x885F7C20: True,   # ldxr w0, [x1]
        0x88027C20: True,   # stxr w2, w0, [x1]
        0x085F7C61: True,   # ldxrb w1, [x3]
        0xC87F7C20: True,   # ldxp x0, xzr, [x1]
        0x48207C82: False,  # casp x0, x1, x2, x3, [x4]
        0x4860FC82: False,  # caspal x0, x1, x2, x3, [x4]
        0xC8DFFC00: False,  # ldar x0, [x0]
    }
    wrong = [
        f"{word:#010x}"
        for word, expected in cases.items()
        if is_llsc_exclusive(word) != expected
    ]
    if wrong:
        raise SystemExit(f"SCANNER MASK SELF-TEST FAILED for words: {', '.join(wrong)}")


def executable_words(path: str):
    data = Path(path).read_bytes()
    elf_header = "<16sHHIQQQIHHHHHH"
    section_header = "<IIQQQQIIQQ"
    if len(data) < struct.calcsize(elf_header):
        raise SystemExit(f"SCANNER ELF ERROR for {path}: truncated ELF header")

    (
        ident,
        _elf_type,
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
        string_section_index,
    ) = struct.unpack_from(elf_header, data)
    if ident[:4] != b"\x7fELF" or ident[4] != 2 or ident[5] != 1 or ident[6] != 1:
        raise SystemExit(f"SCANNER ELF ERROR for {path}: require ELF64 little-endian")
    if machine != 183 or version != 1 or elf_header_size != struct.calcsize(elf_header):
        raise SystemExit(f"SCANNER ELF ERROR for {path}: require aarch64 ELF version 1")
    if section_count == 0 or string_section_index >= section_count:
        raise SystemExit(f"SCANNER ELF ERROR for {path}: unsupported extended/missing sections")
    if section_entry_size < struct.calcsize(section_header):
        raise SystemExit(f"SCANNER ELF ERROR for {path}: short section-header entries")
    section_table_size = section_count * section_entry_size
    if section_offset > len(data) or section_table_size > len(data) - section_offset:
        raise SystemExit(f"SCANNER ELF ERROR for {path}: section table outside file")

    def read_section(index: int):
        offset = section_offset + index * section_entry_size
        return struct.unpack_from(section_header, data, offset)

    string_section = read_section(string_section_index)
    string_type = string_section[1]
    string_offset = string_section[4]
    string_size = string_section[5]
    if string_type != 3 or string_offset > len(data) or string_size > len(data) - string_offset:
        raise SystemExit(f"SCANNER ELF ERROR for {path}: invalid section-name table")
    names = data[string_offset : string_offset + string_size]

    def section_name(name_offset: int, index: int) -> str:
        if name_offset >= len(names):
            raise SystemExit(f"SCANNER ELF ERROR for {path}: bad name for section {index}")
        end = names.find(b"\0", name_offset)
        if end < 0:
            raise SystemExit(f"SCANNER ELF ERROR for {path}: unterminated section name")
        return names[name_offset:end].decode("ascii", errors="replace")

    words = []
    addresses = set()
    for index in range(section_count):
        name_offset, section_type, flags, address, offset, size, *_rest = read_section(index)
        if flags & 0x4 == 0 or size == 0:  # SHF_EXECINSTR
            continue
        name = section_name(name_offset, index)
        if section_type == 8:  # SHT_NOBITS
            raise SystemExit(f"SCANNER ELF ERROR for {path}: executable NOBITS section {name}")
        if address % 4 != 0 or size % 4 != 0:
            raise SystemExit(f"SCANNER ELF ERROR for {path}: unaligned executable section {name}")
        if size > (1 << 64) - address:
            raise SystemExit(f"SCANNER ELF ERROR for {path}: executable section {name} wraps")
        if offset > len(data) or size > len(data) - offset:
            raise SystemExit(f"SCANNER ELF ERROR for {path}: executable section {name} outside file")
        section = data[offset : offset + size]
        for word_offset in range(0, size, 4):
            word_address = address + word_offset
            if word_address in addresses:
                raise SystemExit(f"SCANNER ELF ERROR for {path}: overlapping executable sections")
            addresses.add(word_address)
            word = int.from_bytes(section[word_offset : word_offset + 4], "little")
            words.append((word_address, word, name))

    # F3-SCAN-SEG: also walk executable PT_LOAD SEGMENTS, not just SHF_EXECINSTR sections.
    # The stage-2 execute guard scans whatever the guest makes executable at PAGE/segment
    # granularity; a word in an executable segment that lies in a non-exec-flagged section
    # (or in no section at all) is still executable in the guest and must be scanned. Union
    # the segment words with the section words (dedup by address); a segment word already
    # covered by a section keeps the section's label.
    program_header = "<IIQQQQQQ"  # p_type, p_flags, p_offset, p_vaddr, p_paddr, p_filesz, p_memsz, p_align
    if program_count and program_entry_size >= struct.calcsize(program_header):
        table_size = program_count * program_entry_size
        if program_offset > len(data) or table_size > len(data) - program_offset:
            raise SystemExit(f"SCANNER ELF ERROR for {path}: program table outside file")
        for index in range(program_count):
            off = program_offset + index * program_entry_size
            p_type, p_flags, p_offset, p_vaddr, _paddr, p_filesz, _memsz, _align = struct.unpack_from(
                program_header, data, off
            )
            if p_type != 1 or p_flags & 0x1 == 0:  # PT_LOAD with PF_X
                continue
            if p_vaddr % 4 != 0 or p_filesz % 4 != 0:
                raise SystemExit(f"SCANNER ELF ERROR for {path}: unaligned executable segment {index}")
            if p_offset > len(data) or p_filesz > len(data) - p_offset:
                raise SystemExit(f"SCANNER ELF ERROR for {path}: executable segment {index} outside file")
            if p_filesz > (1 << 64) - p_vaddr:
                raise SystemExit(f"SCANNER ELF ERROR for {path}: executable segment {index} wraps")
            segment = data[p_offset : p_offset + p_filesz]
            label = f"<exec-segment {index}>"
            for word_offset in range(0, p_filesz, 4):
                word_address = p_vaddr + word_offset
                if word_address in addresses:
                    continue  # already scanned as part of an executable section
                addresses.add(word_address)
                word = int.from_bytes(segment[word_offset : word_offset + 4], "little")
                words.append((word_address, word, label))

    if not words:
        raise SystemExit(f"SCANNER ELF ERROR for {path}: no executable words")
    words.sort(key=lambda w: w[0])
    return words


def disassembly_cross_check(path: str):
    # Mapping-symbol data in an executable section is intentionally absent from this parse. The
    # independent ELF-byte walk above is authoritative; objdump checks the decoder wherever it
    # actually renders an instruction mnemonic.
    dz = subprocess.run(
        ["objdump", "-d", path], capture_output=True, text=True, check=True
    ).stdout

    decoded = {}
    disagreements = []
    line_re = re.compile(r"^\s*([0-9a-fA-F]+):\s+([0-9a-fA-F]{8})\s+(.*)$")
    for line in dz.splitlines():
        m = line_re.match(line)
        if not m:
            continue
        addr, raw, asm = m.group(1), m.group(2), m.group(3).strip()
        # objdump prints the raw word in target-endian text (already big-endian display of the
        # 32-bit value), so int(raw,16) is the instruction word directly.
        word = int(raw, 16)
        mnem = asm.split()[0].lower() if asm else ""
        if mnem in (".word", ".inst"):
            # Mapping-symbol data that this objdump renders word-wise (binutils 2.42) rather
            # than byte-wise. No mnemonic exists to check; the ELF-byte walk stays the
            # authority that rejects an exclusive hidden as data.
            decoded[int(addr, 16)] = (word, asm)
            continue
        # An exclusive-monitor mnemonic per objdump: starts with ldxr/stxr/ldaxr/stlxr/ldxp/stxp..
        mnem_is_excl = bool(re.match(r"(ld(a)?xr[bh]?|st(l)?xr[bh]?|ld(a)?xp|st(l)?xp)$", mnem))
        if is_llsc_exclusive(word) != mnem_is_excl:
            disagreements.append(addr)
        decoded[int(addr, 16)] = (word, asm)
    if not decoded:
        raise SystemExit(f"SCANNER SELF-CHECK FAILED for {path}: objdump parsed zero instructions")
    if disagreements:
        raise SystemExit(
            f"SCANNER SELF-CHECK FAILED for {path}: decoder disagrees at {disagreements}"
        )
    return decoded


def scan(path: str):
    words = executable_words(path)
    decoded = disassembly_cross_check(path)
    raw_by_address = {address: word for address, word, _section in words}
    raw_mismatches = [
        f"{address:x}"
        for address, (word, _asm) in decoded.items()
        if raw_by_address.get(address) != word
    ]
    if raw_mismatches:
        raise SystemExit(
            f"SCANNER SELF-CHECK FAILED for {path}: ELF/objdump mismatch at {raw_mismatches}"
        )
    hits = []
    for address, word, section in words:
        if is_llsc_exclusive(word):
            decoded_word = decoded.get(address)
            asm = decoded_word[1] if decoded_word else f"<raw executable word in {section}>"
            hits.append((f"{address:x}", f"{word:08x}", asm))
    return hits


def main():
    self_test_mask()
    if len(sys.argv) < 2:
        raise SystemExit("usage: aa4-exclusive-scan.py <aarch64-elf> [<elf> ...]")
    any_hits = False
    for path in sys.argv[1:]:
        hits = scan(path)
        name = path.split("/")[-1]
        if hits:
            any_hits = True
            print(f"[BANNED] {name}: {len(hits)} LL/SC exclusive instruction(s)")
            for addr, raw, asm in hits:
                print(f"    {addr}: {raw}  {asm}")
        else:
            print(f"[CLEAN]  {name}: no LL/SC exclusive instructions (mask self-check passed)")
    # Exit 1 if any scanned image carries an exclusive -- a rescan-on-exec would reject it.
    sys.exit(1 if any_hits else 0)


if __name__ == "__main__":
    main()
