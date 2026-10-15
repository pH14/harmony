#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
import platform
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path


SPEC = importlib.util.spec_from_file_location(
    "entropy_scan", Path(__file__).with_name("n6-entropy-scan.py")
)
scan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scan)


def elf(machine, text, safe, text_flags):
    names = b"\0.text\0.safe\0.shstrtab\0"
    code = text + safe
    code_offset = 8192
    code_address = 0x1000
    names_offset = code_offset + 4096
    data = bytearray(names_offset + len(names))
    ident = b"\x7fELF\x02\x01\x01" + bytes(9)
    struct.pack_into(
        scan.ELF_HEADER,
        data,
        0,
        ident,
        2,
        machine,
        1,
        code_address,
        64,
        128,
        0,
        64,
        56,
        1,
        64,
        4,
        3,
    )
    struct.pack_into(
        scan.PROGRAM_HEADER,
        data,
        64,
        scan.PT_LOAD,
        scan.PF_X | 4,
        code_offset,
        code_address,
        code_address,
        len(code),
        len(code),
        1,
    )
    section = "<IIQQQQIIQQ"
    struct.pack_into(section, data, 192, 1, 1, text_flags, code_address, code_offset, len(text), 0, 0, 1, 0)
    struct.pack_into(
        section,
        data,
        256,
        7,
        1,
        6,
        code_address + len(text),
        code_offset + len(text),
        len(safe),
        0,
        0,
        1,
        0,
    )
    struct.pack_into(
        section,
        data,
        320,
        13,
        3,
        0,
        0,
        names_offset,
        len(names),
        0,
        0,
        1,
        0,
    )
    data[code_offset : code_offset + len(code)] = code
    data[names_offset : names_offset + len(names)] = names
    return data


def executable_tail_elf():
    data = elf(scan.EM_X86_64, b"\xe9\x00\x00\x00\x00", b"", 6)
    for offset, value in (
        (24, 0x401000),
        (80, 0x401000),
        (88, 0x401000),
        (112, 0x1000),
        (192 + 16, 0x401000),
    ):
        struct.pack_into("<Q", data, offset, value)
    data[8192 : 8192 + 20] = (
        b"\xe9\x00\x00\x00\x00"
        b"\x0f\xc7\xf0"
        b"\xbf\x2a\x00\x00\x00"
        b"\xb8\x3c\x00\x00\x00"
        b"\x0f\x05"
    )
    return data


def executable_head_elf():
    data = executable_tail_elf()
    for offset, value in (
        (24, 0x401005),
        (72, 8197),
        (80, 0x401005),
        (88, 0x401005),
        (96, 17),
        (104, 17),
        (192 + 16, 0x401005),
        (192 + 24, 8197),
        (192 + 32, 17),
    ):
        struct.pack_into("<Q", data, offset, value)
    data[8192 : 8192 + 22] = (
        b"\x0f\xc7\xf0\xeb\x05"
        b"\xe9\xf6\xff\xff\xff"
        b"\xbf\x2a\x00\x00\x00"
        b"\xb8\x3c\x00\x00\x00"
        b"\x0f\x05"
    )
    return data


def runnable_x86_elf(code):
    data = elf(scan.EM_X86_64, code, b"", 6)
    for offset in (24, 80, 88, 192 + 16):
        struct.pack_into("<Q", data, offset, 0x401000)
    return data


class EntropyScanTests(unittest.TestCase):
    def check(self, data):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "guest"
            path.write_bytes(data)
            return scan.scan(path)

    @unittest.skipUnless(
        platform.system() == "Linux" and platform.machine() == "x86_64",
        "requires x86-64 Linux",
    )
    def test_x86_executable_file_page_tail_is_scanned(self):
        for memory_size in (5, 20):
            with self.subTest(memory_size=memory_size):
                with tempfile.TemporaryDirectory() as temporary:
                    path = Path(temporary) / "guest"
                    data = executable_tail_elf()
                    struct.pack_into("<Q", data, 104, memory_size)
                    path.write_bytes(data)
                    path.chmod(0o755)
                    self.assertEqual(subprocess.run([path], check=False).returncode, 42)
                    self.assertEqual(
                        scan.scan(path),
                        [("<exec-segment 0>", 0, "RDRAND")],
                    )

    @unittest.skipUnless(
        platform.system() == "Linux" and platform.machine() == "x86_64",
        "requires x86-64 Linux",
    )
    def test_x86_executable_file_page_head_is_scanned(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "guest"
            path.write_bytes(executable_head_elf())
            path.chmod(0o755)
            self.assertEqual(subprocess.run([path], check=False).returncode, 42)
            self.assertEqual(
                scan.scan(path),
                [("<exec-segment 0>", 0, "RDRAND")],
            )

    def test_x86_executable_load_page_positions_must_match(self):
        data = executable_tail_elf()
        struct.pack_into("<Q", data, 72, 8193)
        with self.assertRaisesRegex(ValueError, "page offset mismatch"):
            self.check(data)

    def test_x86_section_flag_cannot_hide_loaded_rdrand(self):
        for flags in (6, 2):
            with self.subTest(text_flags=flags):
                data = elf(scan.EM_X86_64, b"\x0f\xc7\xf0", b"\xc3", flags)
                self.assertEqual(
                    self.check(data), [(".text", 0, "RDRAND")]
                )

    def test_x86_displacement_with_entropy_opcode_is_rejected(self):
        lea = b"\x48\x8d\x3d\x0f\xc7\xf8\x00"
        self.assertEqual(
            self.check(elf(scan.EM_X86_64, lea, b"\xc3", 6)),
            [(".text", 3, "RDSEED")],
        )

    def test_x86_register_forms_and_neighboring_modrm(self):
        code = b"\x0f\xc7\xf0\x0f\xc7\xf7\x0f\xc7\xf8\x0f\xc7\xff\x0f\xc7\x30"
        self.assertEqual(
            self.check(elf(scan.EM_X86_64, code, b"", 6)),
            [
                (".text", 0, "RDRAND"),
                (".text", 3, "RDRAND"),
                (".text", 6, "RDSEED"),
                (".text", 9, "RDSEED"),
            ],
        )

    def test_x86_opcode_split_across_adjacent_sections(self):
        self.assertEqual(
            self.check(elf(scan.EM_X86_64, b"\x90\x0f", b"\xc7\xf0\xc3", 6)),
            [(".text", 1, "RDRAND")],
        )

    def test_x86_opcode_split_across_adjacent_load_slices(self):
        self.assertEqual(
            scan.x86_hits(
                [
                    ("<exec-segment 0>", 0x1000, b"\x90\x0f"),
                    ("<exec-segment 1>", 0x1002, b"\xc7\xf8\xc3"),
                ]
            ),
            [("<exec-segment 0>", 1, "RDSEED")],
        )

    def test_x86_opcode_bytes_across_unmapped_gap_do_not_form_hit(self):
        self.assertEqual(
            scan.x86_hits([("one", 0x1000, b"\x0f"), ("two", 0x1002, b"\xc7\xf0")]),
            [],
        )

    def test_x86_opcode_split_across_adjacent_executable_loads(self):
        first_page = b"\x90" * 4095 + b"\x0f"
        data = elf(scan.EM_X86_64, first_page, b"", 6)
        names = b"\0.text\0.safe\0.shstrtab\0"
        data.extend(bytes(4096))
        data[12288:12290] = b"\xc7\xf8"
        data[16384 : 16384 + len(names)] = names
        struct.pack_into("<Q", data, 32, 384)
        struct.pack_into("<H", data, 56, 2)
        data[384:440] = data[64:120]
        struct.pack_into(
            scan.PROGRAM_HEADER,
            data,
            440,
            scan.PT_LOAD,
            scan.PF_X | 4,
            12288,
            0x2000,
            0x2000,
            2,
            2,
            4096,
        )
        struct.pack_into("<Q", data, 256 + 16, 0x2000)
        struct.pack_into("<Q", data, 256 + 24, 12288)
        struct.pack_into("<Q", data, 256 + 32, 2)
        struct.pack_into("<Q", data, 320 + 24, 16384)
        self.assertEqual(self.check(data), [(".text", 4095, "RDSEED")])

    @unittest.skipUnless(
        platform.system() == "Linux" and platform.machine() == "x86_64",
        "requires x86-64 Linux",
    )
    def test_x86_runnable_direct_and_indirect_operand_entries(self):
        exit_42 = b"\xbf\x2a\x00\x00\x00\xb8\x3c\x00\x00\x00\x0f\x05"
        direct = b"\xeb\x01\xb8\x0f\xc7\xf0\x90" + exit_42
        indirect = (
            b"\x48\x8d\x05\x03\x00\x00\x00\xff\xe0\xb8\x0f\xc7\xf0\x90"
            + exit_42
        )
        for code, offset in ((direct, 3), (indirect, 10)):
            with self.subTest(code=code.hex()), tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / "guest"
                path.write_bytes(runnable_x86_elf(code))
                path.chmod(0o755)
                self.assertEqual(subprocess.run([path], check=False).returncode, 42)
                self.assertEqual(scan.scan(path), [(".text", offset, "RDRAND")])

    def test_x86_direct_transfer_cannot_enter_an_instruction_immediate(self):
        for code, offset in (
            (b"\xeb\x01\xb8\x0f\xc7\xf0\x90\xc3", 3),
            (b"\x75\x01\xb8\x0f\xc7\xf0\x90\xc3", 3),
            (b"\xe8\x01\x00\x00\x00\xb8\x0f\xc7\xf0\x90\xc3", 6),
        ):
            with self.subTest(code=code.hex()):
                self.assertEqual(
                    self.check(elf(scan.EM_X86_64, code, b"", 6)),
                    [(".text", offset, "RDRAND")],
                )

    def test_x86_direct_transfer_checks_neighboring_section(self):
        text = b"\xeb\x03\x90\xc3"
        safe = b"\xb8\x0f\xc7\xf0\x90\xc3"
        self.assertEqual(
            self.check(elf(scan.EM_X86_64, text, safe, 6)),
            [(".safe", 1, "RDRAND")],
        )

    def test_x86_alternate_entry_past_lock_prefix_is_valid(self):
        code = b"\x74\x01\xf0\x48\x0f\xb1\x0d\xa5\xf3\x09\x00\xc3"
        self.assertEqual(self.check(elf(scan.EM_X86_64, code, b"", 6)), [])

    def test_x86_branch_into_split_instruction_cannot_hide_entropy(self):
        text = b"\xeb\x00"
        safe_and_gap = b"\x3c\xb8\x90\x0f\xc7\xf0\xc3"
        data = elf(scan.EM_X86_64, text, safe_and_gap, 6)
        struct.pack_into("<Q", data, 256 + 32, 1)
        self.assertEqual(self.check(data), [("<exec-segment 0>", 2, "RDRAND")])

    def test_x86_branch_reaches_split_instruction_after_fallthrough(self):
        data = elf(
            scan.EM_X86_64,
            b"\xeb\x00",
            b"\x90\x90\x3c\xb8\x90\x0f\xc7\xf0\xc3",
            6,
        )
        struct.pack_into("<Q", data, 256 + 32, 3)
        self.assertEqual(self.check(data), [("<exec-segment 0>", 2, "RDRAND")])

    def test_x86_return_before_split_instruction_still_rejects_entropy(self):
        data = elf(scan.EM_X86_64, b"\xeb\x00", b"\xc3\x3c\xb8\x90\x0f\xc7\xf0\xc3", 6)
        struct.pack_into("<Q", data, 256 + 32, 2)
        self.assertEqual(self.check(data), [("<exec-segment 0>", 2, "RDRAND")])

    def test_x86_unused_entropy_shaped_immediate_is_rejected(self):
        self.assertEqual(
            self.check(elf(scan.EM_X86_64, b"\xb8\x0f\xc7\xf0\x90\xc3", b"", 6)),
            [(".text", 1, "RDRAND")],
        )

    def test_mapped_section_start_recovers_instruction_after_segment_prelude(self):
        data = elf(scan.EM_X86_64, b"\x0f\xc7\xf0", b"\xc3", 2)
        data[8191] = 0xe8
        for offset, value in (
            (24, 0x401000),
            (72, 8191),
            (80, 0x400fff),
            (88, 0x400fff),
            (96, 5),
            (104, 5),
            (192 + 16, 0x401000),
            (256 + 16, 0x401003),
        ):
            struct.pack_into("<Q", data, offset, value)
        self.assertEqual(
            scan.x86_hits([("segment", 0x400fff, data[8191:8196])]),
            [("segment", 1, "RDRAND")],
        )
        self.assertEqual(self.check(data), [(".text", 0, "RDRAND")])

    def test_segment_prelude_does_not_hide_entropy_inside_a_section(self):
        data = elf(scan.EM_X86_64, b"\xb8\x90\x0f\xc7\xf0\xc3", b"", 6)
        data[8191] = 0x3c
        for offset, value in (
            (24, 0x401000),
            (72, 8191),
            (80, 0x400fff),
            (88, 0x400fff),
            (96, 7),
            (104, 7),
            (192 + 16, 0x401000),
        ):
            struct.pack_into("<Q", data, offset, value)
        self.assertEqual(self.check(data), [(".text", 2, "RDRAND")])

    def test_arm_section_flag_cannot_hide_loaded_rndr(self):
        rndr = scan.RNDR.to_bytes(4, "little")
        ret = b"\xc0\x03\x5f\xd6"
        self.assertEqual(
            self.check(elf(scan.EM_AARCH64, rndr, ret, 2)),
            [("<exec-segment 0>", 0, "RNDR")],
        )

    def test_arm_odd_length_data_section_does_not_fail_the_segment_scan(self):
        rndr = scan.RNDR.to_bytes(4, "little")
        ret = b"\xc0\x03\x5f\xd6"
        data = elf(scan.EM_AARCH64, rndr, ret, 2)
        struct.pack_into("<Q", data, 192 + 32, 3)
        self.assertEqual(
            self.check(data), [("<exec-segment 0>", 0, "RNDR")]
        )

    def test_arm_executable_load_can_end_with_partial_data_word(self):
        rndr = scan.RNDR.to_bytes(4, "little")
        ret = b"\xc0\x03\x5f\xd6"
        self.assertEqual(
            self.check(elf(scan.EM_AARCH64, rndr, ret + b"\xff", 2)),
            [("<exec-segment 0>", 0, "RNDR")],
        )


if __name__ == "__main__":
    unittest.main()
