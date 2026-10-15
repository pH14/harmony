#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
import platform
import struct
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
    code_offset = 384
    data = bytearray(code_offset + len(code) + len(names))
    ident = b"\x7fELF\x02\x01\x01" + bytes(9)
    struct.pack_into(
        scan.ELF_HEADER,
        data,
        0,
        ident,
        2,
        machine,
        1,
        0x1000,
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
        0x1000,
        0x1000,
        len(code),
        len(code),
        1,
    )
    section = "<IIQQQQIIQQ"
    struct.pack_into(section, data, 192, 1, 1, text_flags, 0x1000, code_offset, len(text), 0, 0, 1, 0)
    struct.pack_into(
        section,
        data,
        256,
        7,
        1,
        6,
        0x1000 + len(text),
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
        code_offset + len(code),
        len(names),
        0,
        0,
        1,
        0,
    )
    data[code_offset : code_offset + len(code)] = code
    data[code_offset + len(code) :] = names
    return data


class EntropyScanTests(unittest.TestCase):
    def check(self, data):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "guest"
            path.write_bytes(data)
            return scan.scan(path)

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_x86_section_flag_cannot_hide_loaded_rdrand(self):
        for flags in (6, 2):
            with self.subTest(text_flags=flags):
                data = elf(scan.EM_X86_64, b"\x0f\xc7\xf0", b"\xc3", flags)
                self.assertEqual(
                    self.check(data), [(".text", 0, "RDRAND")]
                )

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_x86_displacement_is_not_an_entropy_instruction(self):
        lea = b"\x48\x8d\x3d\x0f\xc7\xf8\x00"
        self.assertEqual(self.check(elf(scan.EM_X86_64, lea, b"\xc3", 6)), [])

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_x86_direct_transfer_cannot_enter_an_instruction_immediate(self):
        for code in (
            b"\xeb\x01\xb8\x0f\xc7\xf0\x90\xc3",
            b"\x75\x01\xb8\x0f\xc7\xf0\x90\xc3",
            b"\xe8\x01\x00\x00\x00\xb8\x0f\xc7\xf0\x90\xc3",
        ):
            with self.subTest(code=code.hex()):
                with self.assertRaisesRegex(ValueError, "noncanonical direct branch target"):
                    self.check(elf(scan.EM_X86_64, code, b"", 6))

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_x86_direct_transfer_checks_neighboring_section(self):
        text = b"\xeb\x03\x90\xc3"
        safe = b"\xb8\x0f\xc7\xf0\x90\xc3"
        with self.assertRaisesRegex(ValueError, "noncanonical direct branch target"):
            self.check(elf(scan.EM_X86_64, text, safe, 6))

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_x86_branch_into_split_instruction_cannot_hide_entropy(self):
        text = b"\xeb\x00"
        safe_and_gap = b"\x3c\xb8\x90\x0f\xc7\xf0\xc3"
        data = elf(scan.EM_X86_64, text, safe_and_gap, 6)
        struct.pack_into("<Q", data, 256 + 32, 1)
        with self.assertRaisesRegex(ValueError, "branch stream crosses executable slice"):
            self.check(data)

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_x86_branch_reaches_split_instruction_after_fallthrough(self):
        data = elf(
            scan.EM_X86_64,
            b"\xeb\x00",
            b"\x90\x90\x3c\xb8\x90\x0f\xc7\xf0\xc3",
            6,
        )
        struct.pack_into("<Q", data, 256 + 32, 3)
        with self.assertRaisesRegex(ValueError, "branch stream crosses executable slice"):
            self.check(data)

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_x86_return_before_split_instruction_does_not_invent_entropy(self):
        data = elf(scan.EM_X86_64, b"\xeb\x00", b"\xc3\x3c\xb8\x90\x0f\xc7\xf0\xc3", 6)
        struct.pack_into("<Q", data, 256 + 32, 2)
        self.assertEqual(self.check(data), [])

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_x86_unused_entropy_shaped_immediate_stays_clean(self):
        self.assertEqual(
            self.check(elf(scan.EM_X86_64, b"\xb8\x0f\xc7\xf0\x90\xc3", b"", 6)),
            [],
        )

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_mapped_section_start_recovers_instruction_after_segment_prelude(self):
        data = elf(scan.EM_X86_64, b"\x0f\xc7\xf0", b"\xc3", 2)
        data[383] = 0xe8
        for offset, value in (
            (24, 0x400180),
            (72, 383),
            (80, 0x40017f),
            (88, 0x40017f),
            (96, 5),
            (104, 5),
            (192 + 16, 0x400180),
            (256 + 16, 0x400183),
        ):
            struct.pack_into("<Q", data, offset, value)
        self.assertEqual(scan.x86_hits([("segment", 0x40017f, data[383:388])]), [])
        self.assertEqual(self.check(data), [(".text", 0, "RDRAND")])

    @unittest.skipUnless(platform.system() == "Linux", "requires GNU objdump")
    def test_segment_prelude_does_not_invent_entropy_inside_a_section(self):
        data = elf(scan.EM_X86_64, b"\xb8\x90\x0f\xc7\xf0\xc3", b"", 6)
        data[383] = 0x3c
        for offset, value in (
            (24, 0x400180),
            (72, 383),
            (80, 0x40017f),
            (88, 0x40017f),
            (96, 7),
            (104, 7),
            (192 + 16, 0x400180),
        ):
            struct.pack_into("<Q", data, offset, value)
        self.assertEqual(self.check(data), [])

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
