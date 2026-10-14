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


def hide_text_bytes(path, mode):
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
            if mode == "flags":
                struct.pack_into("<Q", data, header + 8, flags & ~4)
            elif mode == "size":
                assert text_size >= 3
                struct.pack_into("<Q", data, header + 32, 1)
            else:
                raise AssertionError(mode)
            path.write_bytes(data)
            return
    raise AssertionError("fixture has no .text section")


def make_allocated_code_load_executable(path):
    data = bytearray(path.read_bytes())
    section_offset = struct.unpack_from("<Q", data, 40)[0]
    section_size, section_count, names_index = struct.unpack_from("<HHH", data, 58)
    names_header = section_offset + names_index * section_size
    names_offset, names_size = struct.unpack_from("<QQ", data, names_header + 24)
    names = data[names_offset : names_offset + names_size]
    hidden = None
    for index in range(section_count):
        header = section_offset + index * section_size
        name_offset = struct.unpack_from("<I", data, header)[0]
        name = names[name_offset:].split(b"\0", 1)[0]
        if name == b".hidden":
            flags = struct.unpack_from("<Q", data, header + 8)[0]
            assert flags & 2 and not flags & 4
            hidden = struct.unpack_from("<QQ", data, header + 24)
            break
    assert hidden is not None
    program_offset = struct.unpack_from("<Q", data, 32)[0]
    program_size, program_count = struct.unpack_from("<HH", data, 54)
    for index in range(program_count):
        header = program_offset + index * program_size
        kind, flags = struct.unpack_from("<II", data, header)
        offset = struct.unpack_from("<Q", data, header + 8)[0]
        size = struct.unpack_from("<Q", data, header + 32)[0]
        if kind == 1 and offset <= hidden[0] and hidden[0] + hidden[1] <= offset + size:
            struct.pack_into("<I", data, header + 4, flags | 1)
            path.write_bytes(data)
            return
    raise AssertionError("hidden code has no load segment")


def rename_hidden_to_rodata(path):
    data = bytearray(path.read_bytes())
    section_offset = struct.unpack_from("<Q", data, 40)[0]
    section_size, section_count, names_index = struct.unpack_from("<HHH", data, 58)
    names_header = section_offset + names_index * section_size
    names_offset, names_size = struct.unpack_from("<QQ", data, names_header + 24)
    names = data[names_offset : names_offset + names_size]
    hidden_header = None
    rodata_name = None
    for index in range(section_count):
        header = section_offset + index * section_size
        name_offset = struct.unpack_from("<I", data, header)[0]
        name = names[name_offset:].split(b"\0", 1)[0]
        if name == b".hidden":
            hidden_header = header
        elif name == b".rodata":
            rodata_name = name_offset
    assert hidden_header is not None and rodata_name is not None
    struct.pack_into("<I", data, hidden_header, rodata_name)
    path.write_bytes(data)


class CounterScanCoverage(unittest.TestCase):
    @unittest.skipUnless(
        platform.system() == "Linux" and platform.machine() == "x86_64",
        "native x86 Linux binutils required",
    )
    def test_executable_segment_with_hidden_code_is_rejected(self):
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
            hazard_bytes = kernel.read_bytes()

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
            safe_bytes = (root / "safe.elf").read_bytes()
            for target in (kernel, setup, decompressor):
                target.write_bytes(safe_bytes)
            result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            for mode in ("flags", "size"):
                kernel.write_bytes(hazard_bytes)
                hide_text_bytes(kernel, mode)
                hidden_bytes = kernel.read_bytes()
                for target, tag in ((kernel, "vmlinux"), (setup, "setup"), (decompressor, "decompressor")):
                    kernel.write_bytes(safe_bytes)
                    setup.write_bytes(safe_bytes)
                    decompressor.write_bytes(safe_bytes)
                    target.write_bytes(hidden_bytes)
                    with self.subTest(mode=mode, target=tag):
                        result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn(f"{tag} (", result.stderr)
                        if mode == "flags":
                            self.assertIn("code section .text is not executable PROGBITS", result.stderr)
                        else:
                            self.assertIn("unclaimed", result.stderr)

            allocated = root / "allocated.s"
            allocated.write_text(
                '.section .hidden,"a"\n.global hidden_start\nhidden_start:\nrdseed %eax\nret\n'
                '.section .text,"ax"\n.global _start\n_start:\nret\n'
            )
            run("as", "--64", "-o", str(root / "allocated.o"), str(allocated))
            run(
                "ld", "-nostdlib", "-e", "hidden_start", "-o",
                str(root / "allocated.elf"), str(root / "allocated.o"),
            )
            make_allocated_code_load_executable(root / "allocated.elf")
            hidden_bytes = (root / "allocated.elf").read_bytes()
            for target, tag in ((kernel, "vmlinux"), (setup, "setup"), (decompressor, "decompressor")):
                kernel.write_bytes(safe_bytes)
                setup.write_bytes(safe_bytes)
                decompressor.write_bytes(safe_bytes)
                target.write_bytes(hidden_bytes)
                with self.subTest(mode="allocated", target=tag):
                    result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn(f"{tag} (", result.stderr)
                    self.assertIn("unreviewed allocated data section '.hidden'", result.stderr)

            inert = root / "inert.s"
            inert.write_text(
                '.section .data,"aw"\n.byte 0x0f,0x31,0x0f,0xc7,0xf8\n'
                '.section .text,"ax"\n.global _start\n_start:\nret\n'
            )
            run("as", "--64", "-o", str(root / "inert.o"), str(inert))
            run("ld", "-nostdlib", "-e", "_start", "-o", str(root / "inert.elf"), str(root / "inert.o"))
            inert_bytes = (root / "inert.elf").read_bytes()
            for target, tag in ((kernel, "vmlinux"), (setup, "setup"), (decompressor, "decompressor")):
                kernel.write_bytes(safe_bytes)
                setup.write_bytes(safe_bytes)
                decompressor.write_bytes(safe_bytes)
                target.write_bytes(inert_bytes)
                with self.subTest(mode="inert-data", target=tag):
                    result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            read_only = root / "read_only.s"
            read_only.write_text(
                '.section .rodata,"a"\n.byte 0x0f,0xc7,0xf8\n'
                '.section .text,"ax"\n.global _start\n_start:\nret\n'
            )
            run("as", "--64", "-o", str(root / "read_only.o"), str(read_only))
            run(
                "ld", "-nostdlib", "-e", "_start", "-o",
                str(root / "read_only.elf"), str(root / "read_only.o"),
            )
            data_bytes = (root / "read_only.elf").read_bytes()
            for target, tag in ((kernel, "vmlinux"), (setup, "setup"), (decompressor, "decompressor")):
                kernel.write_bytes(safe_bytes)
                setup.write_bytes(safe_bytes)
                decompressor.write_bytes(safe_bytes)
                target.write_bytes(data_bytes)
                with self.subTest(mode="read-only-data", target=tag):
                    result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            duplicate = root / "duplicate.s"
            duplicate.write_text(
                '.section .hidden,"a"\n.global hidden_start\nhidden_start:\nrdseed %eax\nret\n'
                '.section .rodata,"a"\n.byte 0x00\n'
                '.section .text,"ax"\n.global _start\n_start:\nret\n'
            )
            run("as", "--64", "-o", str(root / "duplicate.o"), str(duplicate))
            run(
                "ld", "-nostdlib", "-e", "hidden_start", "-o",
                str(root / "duplicate.elf"), str(root / "duplicate.o"),
            )
            make_allocated_code_load_executable(root / "duplicate.elf")
            rename_hidden_to_rodata(root / "duplicate.elf")
            duplicate_bytes = (root / "duplicate.elf").read_bytes()
            for target, tag in ((kernel, "vmlinux"), (setup, "setup"), (decompressor, "decompressor")):
                kernel.write_bytes(safe_bytes)
                setup.write_bytes(safe_bytes)
                decompressor.write_bytes(safe_bytes)
                target.write_bytes(duplicate_bytes)
                with self.subTest(mode="duplicate-rodata", target=tag):
                    result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("duplicate allocated data section '.rodata'", result.stderr)


if __name__ == "__main__":
    unittest.main()
