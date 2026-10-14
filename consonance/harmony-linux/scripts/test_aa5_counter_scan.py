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
    code_offset = 4096
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


def executable_page_alias():
    branch = (0x14000001).to_bytes(4, "little")
    counter = (0xD53BE040).to_bytes(4, "little")
    exit_syscall = (0xD2800BA8).to_bytes(4, "little") + (0xD4000001).to_bytes(4, "little")
    data = elf_sections(((".text", 6, branch),
                         (".data", 3, counter + exit_syscall)), 0)
    struct.pack_into("<Q", data, 24, 0x401000)
    struct.pack_into("<H", data, 56, 2)
    struct.pack_into("<IIQQQQQQ", data, 64,
                     1, 5, 0, 0x400000, 0x400000, 4100, 4100, 4096)
    struct.pack_into("<IIQQQQQQ", data, 120,
                     1, 6, 4100, 0x402004, 0x402004, 12, 12, 4096)
    struct.pack_into("<Q", data, 256 + 64 + 16, 0x401000)
    struct.pack_into("<Q", data, 256 + 2 * 64 + 16, 0x402004)
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
        struct.pack_into("<Q", data, 256 + 64 + 24, 4100)
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

    def test_kernel_nx_data_does_not_trigger_exclusive_false_positive(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        ldxr_shaped_data = (0x885F7C20).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".rodata", 2, ldxr_shaped_data)), 0)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "reviewed-data"
            path.write_bytes(data)
            with patch.object(exclusive, "kernel_runtime_exec_ranges", return_value=((0x1000, 0x1004),)):
                words = exclusive.executable_words(str(path), kernel_image=True)
        self.assertIn((0x1000, 0xD65F03C0, ".text"), words)
        self.assertNotIn(0x885F7C20, [word for _address, word, _name in words])

    def test_user_executable_rodata_cannot_hide_exclusive(self):
        branch = (0x14000001).to_bytes(4, "little")
        ldxr = (0x885F7C20).to_bytes(4, "little")
        data = elf_sections(((".text", 6, branch), (".rodata", 2, ldxr)), 0)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "branched-rodata"
            path.write_bytes(data)
            words = exclusive.executable_words(str(path))
        self.assertIn((0x1004, 0x885F7C20, "<exec-segment 0>"), words)

    def test_user_go_runtime_metadata_in_executable_segment_is_scanned(self):
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
        self.assertIn((0x1000, 0xD65F03C0, ".text"), exclusive_words)
        self.assertEqual(
            [word for _address, word, _name in exclusive_words if word == 0x885F7C20],
            [0x885F7C20] * 3,
        )
        self.assertEqual(
            [word for _address, word, _name in counter_words if word == 0x885F7C20],
            [0x885F7C20] * 3,
        )

    def test_pinned_kernel_module_version_metadata_is_reviewed_data(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        ldxr_shaped_data = (0x885F7C20).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), ("__modver", 2, ldxr_shaped_data)), 0)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "kernel-modver"
            path.write_bytes(data)
            with patch.object(exclusive, "kernel_runtime_exec_ranges", return_value=((0x1000, 0x1004),)):
                words = exclusive.executable_words(str(path), kernel_image=True)
        self.assertNotIn(0x885F7C20, [word for _address, word, _name in words])

    def test_kernel_data_label_inside_executable_runtime_range_is_scanned(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        ldxr = (0x885F7C20).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".rodata", 2, ldxr)), 0)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "kernel-executable-data"
            path.write_bytes(data)
            with patch.object(exclusive, "kernel_runtime_exec_ranges", return_value=((0x1000, 0x1008),)):
                words = exclusive.executable_words(str(path), kernel_image=True)
        self.assertIn((0x1004, 0x885F7C20, "<exec-segment 0>"), words)

    def test_kernel_linker_ranges_require_aligned_known_layout(self):
        names = (
            "_text", "_stext", "_etext", "__start_rodata",
            "__rodata_text_begin", "__rodata_text_end", "__inittext_begin",
            "__inittext_end", "__initdata_begin", "__initdata_end", "_data", "_end",
        )
        values = (0x1000, 0x2000, 0x3000, 0x3000, 0x4000, 0x5000,
                  0x6000, 0x7000, 0x7000, 0x8000, 0x8000, 0x9000)

        def read_ranges(addresses, entry=0x1000):
            listing = "".join(f"{value:016x} T {name}\n" for name, value in zip(names, addresses))
            result = subprocess.CompletedProcess([], 0, stdout=listing)
            with patch.object(exclusive.subprocess, "run", return_value=result):
                return exclusive.kernel_runtime_exec_ranges("vmlinux", entry)

        self.assertEqual(read_ranges(values),
                         ((0x2000, 0x3000), (0x4000, 0x5000), (0x6000, 0x7000)))
        with self.assertRaisesRegex(SystemExit, "unexpected entrypoint"):
            read_ranges(values, entry=0x2000)
        with self.assertRaisesRegex(SystemExit, "unaligned linker boundary"):
            read_ranges(values[:4] + (0x4004,) + values[5:])
        with self.assertRaisesRegex(SystemExit, "unexpected linker layout"):
            read_ranges(values[:3] + (0x4000,) + values[4:])
        with self.assertRaisesRegex(SystemExit, "unexpected linker layout"):
            read_ranges(values[:5] + (0x6000,) + values[6:])
        listing = "".join(
            f"{value:016x} T {name}\n"
            for name, value in zip(names, values)
            if name != "__rodata_text_end"
        )
        result = subprocess.CompletedProcess([], 0, stdout=listing)
        with patch.object(exclusive.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(SystemExit, "missing __rodata_text_end"):
                exclusive.kernel_runtime_exec_ranges("vmlinux", values[0])

    def test_page_rounded_executable_mapping_rejects_counter_in_other_segment(self):
        data = executable_page_alias()
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "page-alias"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "executable page alias"):
                exclusive.executable_words(str(path))
            with self.assertRaisesRegex(SystemExit, "executable page alias"):
                scan.executable_words(str(path))

    def test_overlapping_executable_and_data_virtual_pages_are_rejected(self):
        data = executable_page_alias()
        counter = bytes(data[4100:4112])
        data[4100:4112] = bytes(12)
        data.extend(bytes(8208 - len(data)))
        data[8196:8208] = counter
        struct.pack_into("<Q", data, 120 + 8, 8196)
        struct.pack_into("<Q", data, 120 + 16, 0x401004)
        struct.pack_into("<Q", data, 120 + 24, 0x401004)
        struct.pack_into("<Q", data, 256 + 2 * 64 + 16, 0x401004)
        struct.pack_into("<Q", data, 256 + 2 * 64 + 24, 8196)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "virtual-page-alias"
            path.write_bytes(data)
            with self.assertRaisesRegex(SystemExit, "executable page alias"):
                exclusive.executable_words(str(path))

    def test_page_tail_words_outside_executable_filesz_are_scanned(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        for word, reader in ((0x885F7C20, exclusive.executable_words),
                             (0xD53BE040, scan.executable_words)):
            with self.subTest(word=hex(word)):
                data = elf_sections(((".text", 6, ret),
                                     (".padding", 0, word.to_bytes(4, "little"))), 0)
                struct.pack_into("<Q", data, 64 + 32, 4)
                struct.pack_into("<Q", data, 64 + 40, 4)
                with tempfile.TemporaryDirectory() as temporary:
                    path = Path(temporary) / "executable-page-tail"
                    path.write_bytes(data)
                    words = reader(str(path))
                self.assertIn((0x1004, word, "<exec-segment 0>"), words)

    def test_overlapping_executable_segments_are_rejected(self):
        ret = (0xD65F03C0).to_bytes(4, "little")
        data = elf_sections(((".text", 6, ret), (".gap", 0, ret),
                             (".safe", 6, ret)), 0)
        struct.pack_into("<H", data, 56, 2)
        struct.pack_into("<IIQQQQQQ", data, 120,
                         1, 5, 4100, 0x1004, 0x1004, 4, 4, 4)
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
        struct.pack_into("<Q", data, 256 + 2 * 64 + 24, 4096)
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
