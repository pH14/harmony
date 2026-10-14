#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import platform
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path


SCAN = Path(__file__).with_name("scan-counter-opcodes.sh")


def run(*args):
    subprocess.run(args, check=True, capture_output=True, text=True)


def hide_text_flag(path):
    data = bytearray(path.read_bytes())
    section_offset = struct.unpack_from("<Q", data, 40)[0]
    section_size = struct.unpack_from("<H", data, 58)[0]
    section_count = struct.unpack_from("<H", data, 60)[0]
    names_index = struct.unpack_from("<H", data, 62)[0]
    names_header = section_offset + names_index * section_size
    names_offset, names_size = struct.unpack_from("<QQ", data, names_header + 24)
    names = data[names_offset : names_offset + names_size]
    for index in range(section_count):
        header = section_offset + index * section_size
        name_offset = struct.unpack_from("<I", data, header)[0]
        name = names[name_offset:].split(b"\0", 1)[0]
        if name == b".text":
            flags = struct.unpack_from("<Q", data, header + 8)[0]
            assert flags & 4
            text_offset, text_size = struct.unpack_from("<QQ", data, header + 24)
            program_offset = struct.unpack_from("<Q", data, 32)[0]
            program_size, program_count = struct.unpack_from("<HH", data, 54)
            executable_load = False
            for program in range(program_count):
                entry = program_offset + program * program_size
                segment_type, segment_flags = struct.unpack_from("<II", data, entry)
                segment_offset = struct.unpack_from("<Q", data, entry + 8)[0]
                segment_size = struct.unpack_from("<Q", data, entry + 32)[0]
                if segment_type == 1 and segment_flags & 1:
                    if (
                        segment_offset <= text_offset
                        and text_offset + text_size <= segment_offset + segment_size
                    ):
                        executable_load = True
            assert executable_load
            struct.pack_into("<Q", data, header + 8, flags & ~4)
            path.write_bytes(data)
            return
    raise AssertionError("fixture has no .text section")


class CounterScanCoverage(unittest.TestCase):
    @unittest.skipUnless(
        platform.system() == "Linux" and platform.machine() == "x86_64",
        "native x86 Linux binutils required",
    )
    def test_executable_segment_with_hidden_text_flag_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            hazard = root / "hazard.s"
            hazard.write_text(
                '.section .text,"ax"\n.global _start\n_start:\nrdseed %eax\nret\n'
                '.section .safe,"ax"\n.global safe\nsafe:\nret\n'
            )
            run("as", "--64", "-o", str(root / "hazard.o"), str(hazard))
            kernel = root / "vmlinux"
            run("ld", "-nostdlib", "-e", "_start", "-o", str(kernel), str(root / "hazard.o"))
            hide_text_flag(kernel)

            safe = root / "safe.s"
            safe.write_text('.section .text,"ax"\n.global _start\n_start:\nret\n')
            run("as", "--64", "-o", str(root / "safe.o"), str(safe))
            run(
                "ld", "-nostdlib", "-e", "_start", "-o",
                str(root / "safe.elf"), str(root / "safe.o"),
            )
            setup = root / "arch/x86/boot/setup.elf"
            decompressor = root / "arch/x86/boot/compressed/vmlinux"
            setup.parent.mkdir(parents=True)
            decompressor.parent.mkdir(parents=True)
            hazard_bytes = kernel.read_bytes()
            safe_bytes = (root / "safe.elf").read_bytes()
            for target in (kernel, setup, decompressor):
                kernel.write_bytes(safe_bytes)
                setup.write_bytes(safe_bytes)
                decompressor.write_bytes(safe_bytes)
                target.write_bytes(hazard_bytes)
                with self.subTest(target=target):
                    result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn(".text", result.stderr)


if __name__ == "__main__":
    unittest.main()
