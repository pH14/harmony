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
                '.section .text,"ax"\n.global _start\n_start:\nret\n.fill 15,1,0xcc\n'
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
                    if tag == "setup":
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn("mixed-mode raw counter instruction", result.stderr)
                    else:
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            read_only = root / "read_only.s"
            read_only.write_text(
                '.section .rodata,"a"\n.byte 0x0f,0xc7,0xf8\n'
                '.section .text,"ax"\n.global _start\n_start:\nret\n.fill 15,1,0xcc\n'
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
                    if tag == "setup":
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn("mixed-mode raw counter instruction", result.stderr)
                    else:
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

            linker = root / "straddle.ld"
            linker.write_text(
                "PHDRS { all PT_LOAD FLAGS(5); }\n"
                "SECTIONS { . = 0x400000; .text : { *(.text) } :all "
                ".rodata : { *(.rodata) } :all .safe : { *(.safe) } :all }\n"
            )
            boundary_cases = (
                (
                    "cross-section-rdseed",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0x0f,0xc7\n'
                    '.section .safe,"ax"\n.byte 0xf8,0xc3\n',
                    "cross-section RDSEED",
                ),
                (
                    "cross-section-rdtsc",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0x0f\n'
                    '.section .safe,"ax"\n.byte 0x31,0xc3\n',
                    "cross-section RDTSC",
                ),
                (
                    "code-data-rdseed",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0x0f,0xc7\n'
                    '.section .rodata,"a"\n.byte 0xf8,0xc3\n'
                    '.section .safe,"ax"\n.byte 0xc3\n',
                    "cross-section RDSEED",
                ),
                (
                    "code-data-prefix-rdtsc",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0x66\n'
                    '.section .rodata,"a"\n.byte 0x0f,0x31,0xc3\n'
                    '.section .safe,"ax"\n.byte 0xc3\n',
                    "incomplete instruction at executable section boundary",
                ),
                (
                    "code-data-prefix-rdseed",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0x48\n'
                    '.section .rodata,"a"\n.byte 0x0f,0xc7,0xf8,0xc3\n'
                    '.section .safe,"ax"\n.byte 0xc3\n',
                    "incomplete instruction at executable section boundary",
                ),
                (
                    "setup-code-data-mixed-mode-prefix",
                    '.section .text,"ax"\n.global _start\n_start:\n'
                    '.byte 0xb8,0x90,0x90,0x90,0x66\n'
                    '.section .rodata,"a"\n.byte 0x0f,0x31,0xc3\n'
                    '.section .safe,"ax"\n.byte 0xc3\n',
                    "can fall through into loaded bytes",
                ),
                (
                    "code-data-fallthrough-incomplete",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb8\n'
                    '.section .rodata,"a"\n.byte 0x90,0x90,0x90,0x90,0x0f,0x31\n'
                    '.fill 20,1,0x90\n.section .safe,"ax"\n.byte 0xc3\n',
                    "can fall through into loaded bytes",
                ),
                (
                    "code-data-fallthrough-complete",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0x90\n'
                    '.section .rodata,"a"\n.byte 0x0f,0x31\n'
                    '.fill 20,1,0x90\n.section .safe,"ax"\n.byte 0xc3\n',
                    "can fall through into loaded bytes",
                ),
                (
                    "code-data-fallthrough-long",
                    '.section .text,"ax"\n.global _start\n_start:\nnop\n'
                    '.section .rodata,"a"\n.fill 14,1,0x90\nrdtsc\n'
                    'mov %eax,%edi\nmov $60,%eax\nsyscall\n'
                    '.section .safe,"ax"\nret\n',
                    "can fall through into loaded bytes",
                ),
                (
                    "code-data-code-desync",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb8\n'
                    '.section .rodata,"a"\n.byte 0x90\n'
                    '.section .safe,"ax"\n.byte 0x90,0x90,0xb0,0x0f,0x31,0xc3\n',
                    "boundary-desync RDTSC",
                ),
                (
                    "boundary-desync",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb0\n'
                    '.section .safe,"ax"\n.byte 0xb8,0x0f,0x31,0x90,0x90,0xc3\n',
                    "boundary-desync RDTSC",
                ),
                (
                    "boundary-desync-long",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb0\n'
                    '.section .safe,"ax"\n'
                    '.byte 0xb8,0x90,0x90,0x90,0xb0\n'
                    '.byte 0xb8,0x90,0x90,0x90,0xb0\n'
                    '.byte 0xb8,0x90,0x90,0x90,0xb0\n'
                    '.byte 0xb8,0x0f,0x31,0x90,0x90,0xc3\n',
                    "incomplete instruction at executable section boundary",
                ),
                (
                    "boundary-desync-shifted-tail",
                    '.section .text,"ax"\n.global _start\n_start:\n'
                    '.byte 0xb8,0x90,0x90\n'
                    '.section .safe,"ax"\n.byte 0x90\n'
                    + '.byte 0xb8,0x90,0x90,0x90,0xb0\n' * 3
                    + '.byte 0xb8,0x0f,0x31,0x90,0x90\n',
                    "incomplete instruction at executable section boundary",
                ),
            )
            for mode, source, expected in boundary_cases:
                asm = root / f"{mode}.s"
                asm.write_text(source)
                obj = root / f"{mode}.o"
                elf = root / f"{mode}.elf"
                run("as", "--64", "-o", str(obj), str(asm))
                run("ld", "-nostdlib", "-T", str(linker), "-e", "_start", "-o", str(elf), str(obj))
                boundary_bytes = elf.read_bytes()
                targets = ((kernel, "vmlinux"), (setup, "setup"), (decompressor, "decompressor"))
                if mode.startswith("boundary-desync") or mode == "code-data-code-desync":
                    targets = targets[:1]
                elif mode == "setup-code-data-mixed-mode-prefix":
                    targets = targets[1:2]
                elif mode.startswith("code-data-fallthrough"):
                    targets = (targets[0], targets[2])
                for target, tag in targets:
                    kernel.write_bytes(safe_bytes)
                    setup.write_bytes(safe_bytes)
                    decompressor.write_bytes(safe_bytes)
                    target.write_bytes(boundary_bytes)
                    with self.subTest(mode=mode, target=tag):
                        result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn(expected, result.stderr)

            split_linker = root / "split-load.ld"
            split_linker.write_text(
                "PHDRS { a PT_LOAD FLAGS(5); b PT_LOAD FLAGS(5); }\n"
                "SECTIONS { . = 0x400fff; .text : { *(.text) } :a "
                ". = 0x401000; .safe : { *(.safe) } :b }\n"
            )
            split_cases = (
                ("split-load-rdtsc", '.byte 0x0f\n', '.byte 0x31,0xc3\n'),
                ("split-load-desync", '.byte 0xb0\n', '.byte 0xb8,0x0f,0x31,0x90,0x90,0xc3\n'),
            )
            for mode, left, right in split_cases:
                asm = root / f"{mode}.s"
                asm.write_text(
                    '.section .text,"ax"\n.global _start\n_start:\n' + left
                    + '.section .safe,"ax"\n' + right
                )
                obj = root / f"{mode}.o"
                elf = root / f"{mode}.elf"
                run("as", "--64", "-o", str(obj), str(asm))
                run("ld", "-nostdlib", "-T", str(split_linker), "-e", "_start", "-o", str(elf), str(obj))
                kernel.write_bytes(elf.read_bytes())
                setup.write_bytes(safe_bytes)
                decompressor.write_bytes(safe_bytes)
                with self.subTest(mode=mode, target="vmlinux"):
                    result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("can fall through into loaded bytes", result.stderr)

            long_linker = root / "split-load-long.ld"
            long_linker.write_text(split_linker.read_text().replace("0x400fff", "0x400fee"))
            long_source = root / "split-load-long.s"
            long_source.write_text(
                '.section .text,"ax"\n.global _start\n_start:\nmov %rsp,%rax\n'
                '.section .safe,"ax"\n.byte 0xc0,0x0f,0x31\n'
                'mov %eax,%edi\nmov $60,%eax\nsyscall\n'
            )
            run("as", "--64", "-o", str(root / "split-load-long.o"), str(long_source))
            run(
                "ld", "-nostdlib", "-T", str(long_linker), "-e", "_start", "-o",
                str(root / "split-load-long.elf"), str(root / "split-load-long.o"),
            )
            kernel.write_bytes((root / "split-load-long.elf").read_bytes())
            setup.write_bytes(safe_bytes)
            decompressor.write_bytes(safe_bytes)
            with self.subTest(mode="split-load-long-gap", target="vmlinux"):
                result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("can fall through into loaded bytes", result.stderr)

            data_linker = root / "split-load-data.ld"
            data_linker.write_text(
                "PHDRS { a PT_LOAD FLAGS(5); b PT_LOAD FLAGS(6); }\n"
                "SECTIONS { . = 0x400fff; .text : { *(.text) } :a "
                ". = 0x401000; .data : { *(.data) } :b }\n"
            )
            data_source = root / "split-load-data.s"
            data_source.write_text(
                '.section .text,"ax"\n.global _start\n_start:\nnop\n'
                '.section .data,"aw"\n.byte 0x0f,0x31\n'
            )
            run("as", "--64", "-o", str(root / "split-load-data.o"), str(data_source))
            run(
                "ld", "-nostdlib", "-T", str(data_linker), "-e", "_start", "-o",
                str(root / "split-load-data.elf"), str(root / "split-load-data.o"),
            )
            for target, tag in ((kernel, "vmlinux"), (setup, "setup"), (decompressor, "decompressor")):
                kernel.write_bytes(safe_bytes)
                setup.write_bytes(safe_bytes)
                decompressor.write_bytes(safe_bytes)
                target.write_bytes((root / "split-load-data.elf").read_bytes())
                with self.subTest(mode="split-load-nonexecutable-data", target=tag):
                    result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("can fall through into loaded bytes", result.stderr)

            overlap = bytearray((root / "split-load-data.elf").read_bytes())
            program_offset = struct.unpack_from("<Q", overlap, 32)[0]
            program_size, program_count = struct.unpack_from("<HH", overlap, 54)
            load_headers = [
                program_offset + index * program_size
                for index in range(program_count)
                if struct.unpack_from("<I", overlap, program_offset + index * program_size)[0] == 1
            ]
            self.assertEqual(len(load_headers), 2)
            first_address = struct.unpack_from("<Q", overlap, load_headers[0] + 16)[0]
            struct.pack_into("<Q", overlap, load_headers[1] + 16, first_address)
            kernel.write_bytes(overlap)
            setup.write_bytes(safe_bytes)
            decompressor.write_bytes(safe_bytes)
            with self.subTest(mode="overlapping-loads", target="vmlinux"):
                result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("overlapping PT_LOAD memory ranges", result.stderr)

            zero_fill_overlap = bytearray((root / "split-load-data.elf").read_bytes())
            next_address = struct.unpack_from("<Q", zero_fill_overlap, load_headers[1] + 16)[0]
            struct.pack_into(
                "<Q", zero_fill_overlap, load_headers[0] + 40,
                next_address - first_address + 1,
            )
            kernel.write_bytes(zero_fill_overlap)
            with self.subTest(mode="overlapping-zero-filled-loads", target="vmlinux"):
                result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("overlapping PT_LOAD memory ranges", result.stderr)

            bss_linker = root / "split-load-bss.ld"
            bss_linker.write_text(
                "PHDRS { a PT_LOAD FLAGS(5); b PT_LOAD FLAGS(5); }\n"
                "SECTIONS { . = 0x400ffe; .text : { *(.text) } :a "
                ".bss (NOLOAD) : { *(.bss) } :a "
                ". = 0x401000; .safe : { *(.safe) } :b }\n"
            )
            bss_asm = root / "split-load-bss.s"
            bss_asm.write_text(
                '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb8\n'
                '.section .bss,"aw",@nobits\n.skip 1\n'
                '.section .safe,"ax"\n.byte 0xb8,0x90,0x90,0x0f,0x31\n'
                'mov %eax,%edi\nmov $60,%eax\nsyscall\n'
            )
            bss_obj = root / "split-load-bss.o"
            bss_elf = root / "split-load-bss.elf"
            run("as", "--64", "-o", str(bss_obj), str(bss_asm))
            run("ld", "-nostdlib", "-T", str(bss_linker), "-e", "_start", "-o", str(bss_elf), str(bss_obj))
            kernel.write_bytes(bss_elf.read_bytes())
            setup.write_bytes(safe_bytes)
            decompressor.write_bytes(safe_bytes)
            with self.subTest(mode="split-load-bss-gap", target="vmlinux"):
                result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("can fall through into loaded bytes", result.stderr)

            control = root / "boundary-control.s"
            control.write_text(
                '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb8,0x00,0x00,0x0f,0x31\n'
                '.section .safe,"ax"\n.byte 0xc3\n'
            )
            run("as", "--64", "-o", str(root / "boundary-control.o"), str(control))
            run(
                "ld", "-nostdlib", "-T", str(linker), "-e", "_start", "-o",
                str(root / "boundary-control.elf"), str(root / "boundary-control.o"),
            )
            kernel.write_bytes((root / "boundary-control.elf").read_bytes())
            setup.write_bytes(safe_bytes)
            decompressor.write_bytes(safe_bytes)
            with self.subTest(mode="boundary-control", target="vmlinux"):
                result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            fenced = root / "boundary-fenced.s"
            fenced.write_text(
                '.section .text,"ax"\n.global _start\n_start:\nret\n.fill 15,1,0xcc\n'
                '.section .rodata,"a"\n.byte 0x0f,0x31,0x0f,0xc7,0xf8\n'
                '.section .safe,"ax"\nret\n'
            )
            run("as", "--64", "-o", str(root / "boundary-fenced.o"), str(fenced))
            run(
                "ld", "-nostdlib", "-T", str(linker), "-e", "_start", "-o",
                str(root / "boundary-fenced.elf"), str(root / "boundary-fenced.o"),
            )
            for target, tag in ((kernel, "vmlinux"), (setup, "setup"), (decompressor, "decompressor")):
                kernel.write_bytes(safe_bytes)
                setup.write_bytes(safe_bytes)
                decompressor.write_bytes(safe_bytes)
                target.write_bytes((root / "boundary-fenced.elf").read_bytes())
                with self.subTest(mode="boundary-fenced-reviewed-data", target=tag):
                    result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                    if tag == "setup":
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn("mixed-mode raw counter instruction", result.stderr)
                    else:
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            short_fence = root / "boundary-short-fence.s"
            short_fence.write_text(fenced.read_text().replace(".fill 15,1,0xcc", ".fill 14,1,0xcc"))
            run("as", "--64", "-o", str(root / "boundary-short-fence.o"), str(short_fence))
            run(
                "ld", "-nostdlib", "-T", str(linker), "-e", "_start", "-o",
                str(root / "boundary-short-fence.elf"), str(root / "boundary-short-fence.o"),
            )
            kernel.write_bytes((root / "boundary-short-fence.elf").read_bytes())
            setup.write_bytes(safe_bytes)
            decompressor.write_bytes(safe_bytes)
            with self.subTest(mode="boundary-short-fence", target="vmlinux"):
                result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("can fall through into loaded bytes", result.stderr)

            prefixed = root / "boundary-prefixed.s"
            prefixed.write_text(
                '.section .text,"ax"\n.global _start\n_start:\nrep stosb\n'
                '.section .safe,"ax"\n.byte 0xc3\n'
            )
            run("as", "--64", "-o", str(root / "boundary-prefixed.o"), str(prefixed))
            run(
                "ld", "-nostdlib", "-T", str(linker), "-e", "_start", "-o",
                str(root / "boundary-prefixed.elf"), str(root / "boundary-prefixed.o"),
            )
            kernel.write_bytes((root / "boundary-prefixed.elf").read_bytes())
            with self.subTest(mode="boundary-prefixed", target="vmlinux"):
                result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            decoder_cases = (
                (
                    "symbol-restart",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb8\n'
                    '.global reset_decode\nreset_decode:\n'
                    '.byte 0x90,0x90,0x90,0xb8\n'
                    + '.byte 0x90,0x90,0x90,0xb0,0xb8\n' * 3
                    + '.byte 0x0f,0x31\nmov %eax,%edi\nmov $60,%eax\nsyscall\n',
                ),
                (
                    "symbol-restart-not-last",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb8,0x90\n'
                    '.global reset_decode\nreset_decode:\n'
                    '.byte 0xb8,0xb8,0xb8,0x0f,0x31\n'
                    'mov %eax,%edi\nmov $60,%eax\nsyscall\n',
                ),
                (
                    "symbol-restart-gt-name",
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb8\n'
                    '.global reset_decode\nreset_decode:\n'
                    '.byte 0x90,0x90,0x90,0xb8\n'
                    + '.byte 0x90,0x90,0x90,0xb0,0xb8\n' * 3
                    + '.byte 0x0f,0x31\nmov %eax,%edi\nmov $60,%eax\nsyscall\n',
                ),
                (
                    "intel64-displacement",
                    '.section .text,"ax"\n.global _start\n_start:\n'
                    'or $1,%eax\n'
                    '.byte 0x66,0x0f,0x84,0x00,0x00,0xef,0x00,0x0f,0x31\n'
                    'mov %eax,%edi\nmov $60,%eax\nsyscall\n',
                ),
            )
            for mode, source in decoder_cases:
                asm = root / f"{mode}.s"
                asm.write_text(source)
                obj = root / f"{mode}.o"
                elf = root / f"{mode}.elf"
                run("as", "--64", "-o", str(obj), str(asm))
                run("ld", "-nostdlib", "-e", "_start", "-o", str(elf), str(obj))
                hazard_bytes = elf.read_bytes()
                if mode == "symbol-restart-gt-name":
                    self.assertIn(b"reset_decode\0", hazard_bytes)
                    hazard_bytes = hazard_bytes.replace(b"reset_decode\0", b"reset>decode\0", 1)
                    elf.write_bytes(hazard_bytes)
                raw_setup = subprocess.run(
                    ["bash", "-c",
                     "source <(sed '/^self_test$/,$d' \"$1\"); "
                     "if raw_byte_scan_one \"$2\" setup; then exit 0; else exit 1; fi",
                     "probe", str(SCAN), str(elf)],
                    capture_output=True, text=True,
                )
                self.assertNotEqual(raw_setup.returncode, 0, raw_setup.stdout + raw_setup.stderr)
                self.assertIn(
                    "raw counter-opcode bytes in setup section .text", raw_setup.stderr,
                    f"exit={raw_setup.returncode} stdout={raw_setup.stdout!r} stderr={raw_setup.stderr!r}",
                )
                for target, tag in ((kernel, "vmlinux"), (decompressor, "decompressor")):
                    kernel.write_bytes(safe_bytes)
                    setup.write_bytes(safe_bytes)
                    decompressor.write_bytes(safe_bytes)
                    target.write_bytes(hazard_bytes)
                    with self.subTest(mode=mode, target=tag):
                        result = subprocess.run(
                            ["bash", str(SCAN), str(kernel)], capture_output=True, text=True
                        )
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn("counter instruction", result.stderr)
                        if mode.startswith("symbol-restart"):
                            self.assertIn("symbol-restart", result.stderr)
                            self.assertIn(f"{tag}:.text:", result.stderr)
                        else:
                            expected_tag = "vmlinux:intel64" if tag == "vmlinux" else f"{tag}:.text:intel64"
                            self.assertIn(expected_tag, result.stderr)

            for far_mnemonic, modrm in (("ljmpw", "2c"), ("lcallw", "1c")):
                far_branch = root / f"symbol-restart-{far_mnemonic}.s"
                far_branch.write_text(
                    '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb8\n'
                    '.global reset_decode\nreset_decode:\n'
                    f'.byte 0x90,0x90,0x90,0xb0,0x66,0xff,0x{modrm},0x24,0xc3\n'
                )
                obj = root / f"symbol-restart-{far_mnemonic}.o"
                far_branch_elf = root / f"symbol-restart-{far_mnemonic}.elf"
                run("as", "--64", "-o", str(obj), str(far_branch))
                run("ld", "-nostdlib", "-e", "_start", "-o", str(far_branch_elf), str(obj))
                for target, tag in ((kernel, "vmlinux"), (decompressor, "decompressor")):
                    kernel.write_bytes(safe_bytes)
                    setup.write_bytes(safe_bytes)
                    decompressor.write_bytes(safe_bytes)
                    target.write_bytes(far_branch_elf.read_bytes())
                    with self.subTest(mode=far_mnemonic, target=tag):
                        result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn("unresolved branch", result.stderr)

            tail = root / "symbol-tail-restart.s"
            tail.write_text(
                '.section .text,"ax"\n.global _start\n_start:\n.byte 0xb0\n'
                '.global reset_decode\nreset_decode:\n.byte 0xc0,0xc3,0x01\n'
                + '.byte 0xb0\n' * 5
                + '.section .safe,"ax"\n'
                + '.byte 0xb0\n' * 21
                + '.byte 0x0f,0x31\nmov %eax,%edi\nmov $60,%eax\nsyscall\n'
            )
            run("as", "--64", "-o", str(root / "symbol-tail-restart.o"), str(tail))
            tail_elf = root / "symbol-tail-restart.elf"
            run(
                "ld", "-nostdlib", "-T", str(linker), "-e", "reset_decode", "-o",
                str(tail_elf), str(root / "symbol-tail-restart.o"),
            )
            kernel.write_bytes(tail_elf.read_bytes())
            setup.write_bytes(safe_bytes)
            decompressor.write_bytes(safe_bytes)
            with self.subTest(mode="symbol-tail-restart", target="vmlinux"):
                result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("incomplete instruction at executable section boundary", result.stderr)

            duplicate_code = root / "duplicate-code.s"
            duplicate_code.write_text(
                '.section .text,"ax"\n.global _start\n_start:\nret\n'
                '.section .safe,"ax"\nsafe:\nret\n'
                '.section .saft,"ax"\nsaft:\nret\n'
            )
            run("as", "--64", "-o", str(root / "duplicate-code.o"), str(duplicate_code))
            duplicate_elf = root / "duplicate-code.elf"
            run("ld", "-nostdlib", "-e", "_start", "-o", str(duplicate_elf), str(root / "duplicate-code.o"))
            duplicate_bytes = duplicate_elf.read_bytes()
            self.assertIn(b".saft\0", duplicate_bytes)
            kernel.write_bytes(duplicate_bytes.replace(b".saft\0", b".safe\0", 1))
            setup.write_bytes(safe_bytes)
            decompressor.write_bytes(safe_bytes)
            with self.subTest(mode="duplicate-code-name", target="vmlinux"):
                result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("duplicate executable section name", result.stderr)

    @unittest.skipUnless(
        platform.system() == "Linux" and platform.machine() == "x86_64",
        "native x86 Linux binutils required",
    )
    def test_direct_branch_into_operand_counter_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            hazard = root / "hazard.s"
            hazard.write_text(
                '.section .text,"ax"\n.global _start\n_start:\n'
                '.byte 0xeb,0x01,0xb8,0x0f,0x31,0x89,0xc7\n'
                'mov $60,%eax\nsyscall\n.fill 15,1,0xcc\n'
            )
            run("as", "--64", "-o", str(root / "hazard.o"), str(hazard))
            kernel = root / "vmlinux"
            run("ld", "-nostdlib", "-e", "_start", "-o", str(kernel), str(root / "hazard.o"))

            safe = root / "safe.s"
            safe.write_text('.section .text,"ax"\n.global _start\n_start:\nret\n')
            run("as", "--64", "-o", str(root / "safe.o"), str(safe))
            run("ld", "-nostdlib", "-e", "_start", "-o", str(root / "safe.elf"), str(root / "safe.o"))
            for target in (
                root / "arch/x86/boot/setup.elf",
                root / "arch/x86/boot/compressed/vmlinux",
            ):
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes((root / "safe.elf").read_bytes())

            result = subprocess.run(["bash", str(SCAN), str(kernel)], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("noncanonical direct branch target", result.stderr)

    @unittest.skipUnless(
        platform.system() == "Linux" and platform.machine() == "x86_64",
        "native x86 Linux binutils required",
    )
    def test_direct_branch_into_reviewed_data_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            hazard = root / "hazard.s"
            hazard.write_text(
                '.section .text,"ax"\n.global _start\n_start:\njmp entropy_site\n'
                '.fill 15,1,0xcc\n'
                '.section .rodata,"a"\n.global entropy_site\nentropy_site:\n'
                'rdtsc\nmov %eax,%edi\nmov $60,%eax\nsyscall\n'
            )
            linker = root / "layout.ld"
            linker.write_text(
                "PHDRS { all PT_LOAD FLAGS(5); }\n"
                "SECTIONS { . = 0x400000; .text : { *(.text) } :all "
                ".rodata : { *(.rodata) } :all }\n"
            )
            run("as", "--64", "-o", str(root / "hazard.o"), str(hazard))
            run(
                "ld", "-nostdlib", "-T", str(linker), "-e", "_start", "-o",
                str(root / "hazard.elf"), str(root / "hazard.o"),
            )
            safe = root / "safe.s"
            safe.write_text('.section .text,"ax"\n.global _start\n_start:\nret\n')
            run("as", "--64", "-o", str(root / "safe.o"), str(safe))
            run("ld", "-nostdlib", "-e", "_start", "-o", str(root / "safe.elf"), str(root / "safe.o"))
            kernel = root / "vmlinux"
            setup = root / "arch/x86/boot/setup.elf"
            decompressor = root / "arch/x86/boot/compressed/vmlinux"
            setup.parent.mkdir(parents=True)
            decompressor.parent.mkdir(parents=True)
            safe_bytes = (root / "safe.elf").read_bytes()
            hazard_bytes = (root / "hazard.elf").read_bytes()
            for target, tag in ((kernel, "vmlinux"), (decompressor, "decompressor")):
                kernel.write_bytes(safe_bytes)
                setup.write_bytes(safe_bytes)
                decompressor.write_bytes(safe_bytes)
                target.write_bytes(hazard_bytes)
                with self.subTest(target=tag):
                    result = subprocess.run(
                        ["bash", str(SCAN), str(kernel)], capture_output=True, text=True
                    )
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("direct branch into loaded non-code", result.stderr)

    @unittest.skipUnless(
        platform.system() == "Linux" and platform.machine() == "x86_64",
        "native x86 Linux binutils required",
    )
    def test_decompressor_head_direct_branch_into_reviewed_data_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            safe = root / "safe.s"
            safe.write_text('.section .text,"ax"\n.global _start\n_start:\nret\n')
            run("as", "--64", "-o", str(root / "safe.o"), str(safe))
            run("ld", "-nostdlib", "-e", "_start", "-o", str(root / "safe.elf"), str(root / "safe.o"))
            kernel = root / "vmlinux"
            setup = root / "arch/x86/boot/setup.elf"
            decompressor = root / "arch/x86/boot/compressed/vmlinux"
            setup.parent.mkdir(parents=True)
            decompressor.parent.mkdir(parents=True)
            safe_bytes = (root / "safe.elf").read_bytes()
            kernel.write_bytes(safe_bytes)
            setup.write_bytes(safe_bytes)
            linker = root / "layout.ld"
            linker.write_text(
                "PHDRS { all PT_LOAD FLAGS(5); }\n"
                "SECTIONS { . = 0x400000; .head.text : { *(.head.text) } :all "
                ".text : { *(.text) } :all .rodata : { *(.rodata) } :all }\n"
            )
            for mode in ("head32", "head64"):
                hazard = root / "hazard.s"
                hazard.write_text(
                    '.section .head.text,"ax"\n.global startup_32\nstartup_32:\n'
                    + ('jmp entropy_site\n' if mode == "head32" else 'nop\n')
                    + '.fill 27,1,0xcc\n.global startup_64\nstartup_64:\n'
                    + ('jmp entropy_site\n' if mode == "head64" else 'ret\n')
                    + '.fill 15,1,0xcc\n'
                    + '.section .text,"ax"\nret\n.fill 15,1,0xcc\n'
                    + '.section .rodata,"a"\n.global entropy_site\nentropy_site:\n'
                    + 'rdtsc\nmov %eax,%edi\nmov $60,%eax\nsyscall\n'
                )
                run("as", "--64", "-o", str(root / "hazard.o"), str(hazard))
                run(
                    "ld", "-nostdlib", "-T", str(linker), "-e", "startup_32", "-o",
                    str(decompressor), str(root / "hazard.o"),
                )
                with self.subTest(mode=mode):
                    result = subprocess.run(
                        ["bash", str(SCAN), str(kernel)], capture_output=True, text=True
                    )
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("direct branch into loaded non-code", result.stderr)


if __name__ == "__main__":
    unittest.main()
