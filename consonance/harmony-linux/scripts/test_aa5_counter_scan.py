#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "counter_scan", Path(__file__).with_name("aa5-counter-scan.py")
)
scan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scan)
EXCLUSIVE_SPEC = importlib.util.spec_from_file_location(
    "exclusive_scan", Path(__file__).with_name("aa4-exclusive-scan.py")
)
exclusive = importlib.util.module_from_spec(EXCLUSIVE_SPEC)
EXCLUSIVE_SPEC.loader.exec_module(exclusive)


def elf_sections(sections, entry):
    names = bytearray(b"\0")
    name_offsets = {}
    for name, _flags, _body in sections:
        if name not in name_offsets:
            name_offsets[name] = len(names)
            names.extend(name.encode() + b"\0")
    name_offsets[".shstrtab"] = len(names)
    names.extend(b".shstrtab\0")
    code = b"".join(body for _name, _flags, body in sections)
    code_offset = 1024
    section_table_offset = 256
    data = bytearray(code_offset + len(code) + len(names))
    ident = b"\x7fELF\x02\x01\x01" + bytes(9)
    entry_address = 0x1000 + sum(len(body) for name, _flags, body in sections[:entry])
    struct.pack_into(
        "<16sHHIQQQIHHHHHH", data, 0,
        ident, 2, 183, 1, entry_address, 64, section_table_offset, 0, 64, 56, 1, 64,
        len(sections) + 2, len(sections) + 1,
    )
    struct.pack_into(
        "<IIQQQQQQ", data, 64,
        1, 5, code_offset, 0x1000, 0x1000, len(code), len(code), 4,
    )
    position = 0
    for index, (name, flags, body) in enumerate(sections, start=1):
        struct.pack_into(
            "<IIQQQQIIQQ", data, section_table_offset + index * 64,
            name_offsets[name], 1, flags, 0x1000 + position,
            code_offset + position, len(body), 0, 0, 4, 0,
        )
        position += len(body)
    struct.pack_into(
        "<IIQQQQIIQQ", data, section_table_offset + (len(sections) + 1) * 64,
        name_offsets[".shstrtab"], 3, 0, 0, code_offset + len(code),
        len(names), 0, 0, 1, 0,
    )
    data[code_offset : code_offset + len(code)] = code
    data[code_offset + len(code) :] = names
    return data


class CounterScanTests(unittest.TestCase):
    def test_timer_value_reads_with_every_destination(self):
        for base, name in (
            (0xD53BE200, "CNTP_TVAL_EL0"),
            (0xD53BE220, "CNTP_CTL_EL0"),
            (0xD53BE300, "CNTV_TVAL_EL0"),
            (0xD53BE320, "CNTV_CTL_EL0"),
        ):
            for register in range(32):
                with self.subTest(name=name, register=register):
                    self.assertEqual(scan.decode_counter_read(base | register), name)

    def test_adjacent_registers_and_writes_are_not_counter_reads(self):
        for word in (0xD53BE240, 0xD53BE340,
                     0xD538E200, 0xD53AE200, 0xD51BE200, 0xD51BE300):
            with self.subTest(word=hex(word)):
                self.assertIsNone(scan.decode_counter_read(word))
        self.assertEqual(scan.decode_timer_program(0xD51BE200), "CNTP_TVAL_EL0")
        self.assertEqual(scan.decode_timer_program(0xD51BE220), "CNTP_CTL_EL0")
        self.assertEqual(scan.decode_timer_program(0xD51BE300), "CNTV_TVAL_EL0")
        self.assertEqual(scan.decode_timer_program(0xD51BE320), "CNTV_CTL_EL0")

    def check_scan(self, disassembly):
        words = [(0x1000, 0xD53BE201, ".text"), (0x1004, 0xD53BE31F, ".text")]
        output = subprocess.CompletedProcess([], 0, stdout=disassembly)
        with patch.object(scan, "executable_words", return_value=words), \
             patch.object(scan.subprocess, "run", return_value=output):
            reads, writes = scan.scan("fixture")
        self.assertEqual([read[3] for read in reads], ["CNTP_TVAL_EL0", "CNTV_TVAL_EL0"])
        self.assertEqual(writes, [])

    def test_real_mnemonics_agree_with_raw_decoder(self):
        self.check_scan("1000: d53be201 mrs x1, cntp_tval_el0\n"
                        "1004: d53be31f mrs xzr, cntv_tval_el0\n")

    def test_mapping_symbol_data_remains_rejected(self):
        self.check_scan("1000: d53be201 .word 0xd53be201\n"
                        "1004: d53be31f .inst 0xd53be31f\n")

    def test_word_omitted_from_disassembly_remains_rejected(self):
        self.check_scan("1000: d53be201 mrs x1, cntp_tval_el0\n")

    def test_nonexecutable_section_flag_cannot_hide_loaded_counter(self):
        counter = (0xD53BE040).to_bytes(4, "little")
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 2, counter), (".safe", 6, ret)), 0)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "hidden-counter"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "non-executable .text"):
                exclusive.executable_words(str(path))

    def test_nontext_data_flag_cannot_hide_loaded_counter(self):
        counter = (0xD53BE040).to_bytes(4, "little")
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".rodata", 2, counter)), 0)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "hidden-counter"
            path.write_bytes(data)
            words = scan.executable_words(str(path))
        self.assertIn((0x1004, 0xD53BE040, "<exec-segment 0>"), words)

    def test_section_file_offset_lie_cannot_hide_loaded_counter(self):
        counter = (0xD53BE040).to_bytes(4, "little")
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, counter), (".safe", 6, ret)), 0)
        struct.pack_into("<Q", data, 256 + 64 + 24, 1028)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "misdirected-text"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "section.*segment mapping"):
                exclusive.executable_words(str(path))

    def test_section_size_lie_cannot_hide_loaded_counter(self):
        counter = (0xD53BE040).to_bytes(4, "little")
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".rodata", 2, counter)), 0)
        struct.pack_into("<Q", data, 256 + 2 * 64 + 32, 8)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "inflated-data"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "section.*segment mapping"):
                exclusive.executable_words(str(path))

    def test_unknown_data_section_cannot_hide_loaded_exclusive(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        ldxr = (0x885F7C20).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".hidden", 2, ldxr)), 1)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "hidden-exclusive"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "unreviewed.*data section"):
                exclusive.executable_words(str(path))

    def test_duplicate_reviewed_data_sections_are_rejected(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".rodata", 2, ret),
                             (".rodata", 2, ret)), 0)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "duplicate-data"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "duplicate loaded section"):
                exclusive.executable_words(str(path))

    def test_entrypoint_in_reviewed_data_is_rejected(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".rodata", 2, ret)), 1)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "data-entrypoint"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "entrypoint in data section"):
                exclusive.executable_words(str(path))

    def test_reviewed_data_does_not_trigger_exclusive_false_positive(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        ldxr_shaped_data = (0x885F7C20).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".rodata", 2, ldxr_shaped_data)), 0)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "reviewed-data"
            path.write_bytes(data)
            words = exclusive.executable_words(str(path))
        self.assertEqual(words, [(0x1000, 0xD65F03C0, ".text")])

    def test_pinned_go_runtime_metadata_in_executable_segment_is_reviewed_data(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        ldxr_shaped_data = (0x885F7C20).to_bytes(4, "little")
        sections = ((".text", 6, ret),) + tuple(
            (name, 2, ldxr_shaped_data)
            for name in (".typelink", ".itablink", ".gopclntab")
        )
        data = elf_sections(sections, 0)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "go-runtime-metadata"
            path.write_bytes(data)
            exclusive_words = exclusive.executable_words(str(path))
            counter_words = scan.executable_words(str(path))
        self.assertEqual(exclusive_words, [(0x1000, 0xD65F03C0, ".text")])
        self.assertEqual(len(counter_words), 4)

    def test_overlapping_executable_segments_are_rejected(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".gap", 0, ret),
                             (".safe", 6, ret)), 0)
        struct.pack_into("<H", data, 56, 2)
        struct.pack_into("<IIQQQQQQ", data, 120,
                         1, 5, 1028, 0x1004, 0x1004, 4, 4, 4)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "overlapping-segments"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "overlapping executable segments"):
                exclusive.executable_words(str(path))

    def test_overlapping_loaded_data_and_code_are_rejected(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".rodata", 2, ret),
                             (".safe", 6, ret)), 2)
        struct.pack_into("<Q", data, 256 + 2 * 64 + 16, 0x1000)
        struct.pack_into("<Q", data, 256 + 2 * 64 + 24, 1024)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "overlapping-loaded-sections"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "overlapping loaded sections"):
                exclusive.executable_words(str(path))

    def test_entrypoint_in_segment_gap_is_rejected(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".gap", 0, ret),
                             (".safe", 6, ret)), 1)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "gap-entrypoint"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "entrypoint outside executable sections"):
                exclusive.executable_words(str(path))

    def test_executable_section_outside_executable_segment_is_rejected(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".safe", 6, ret)), 0)
        struct.pack_into("<Q", data, 256 + 2 * 64 + 16, 0x2000)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "outside-executable-segment"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "executable section.*outside executable load"):
                exclusive.executable_words(str(path))


if __name__ == "__main__":
    unittest.main()
